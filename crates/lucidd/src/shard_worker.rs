// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tier-1 Trusted-Circle `ShardWorker` coordinator and inbound stage server.
//!
//! Bridges the pure partition/receipt math in [`crate::shard_plan`] onto
//! `phase-net`'s workload-neutral `/phase/shard-stage/1.0.0` substream so a
//! private invited circle of heterogeneous machines (e.g., 16 GB GPU + 32 GB
//! Apple Silicon + 128 GB Apple Silicon) can execute multi-stage pipelines with
//! per-stage Ed25519 attribution and SHA-256 activation handoff verification.
//!
//! Security & honesty boundaries:
//! - Tier-1 `TrustedCircle` ONLY: `ShardTrustTier::UntrustedSpotChecked` fails
//!   closed on both coordinator and stage worker.
//! - Default-deny inbound authorization: every inbound stage request must come
//!   from an authorized peer (via `PolicyEngine` allowlist or explicit opt-in)
//!   and carry a valid, unexpired coordinator-signed `SignedShardPartitionPlan`
//!   naming this exact node and verifying key for `request.stage_index`.
//! - Never claims third-party cryptographic proof of intermediate tensor math;
//!   every verification result carries `trust_label = "trusted-circle-sharded"`.
//!
//! The same module serves the provider side of `/phase/shard-rpc/1.0.0`: an
//! enrolled coordinator's opaque byte tunnel is bridged onto an operator-run
//! loopback `ggml-rpc-server` (see [`make_opt_in_inbound_shard_rpc_handler`]).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use phase_identity::NodeIdentity;
use phase_net::{
    Discovery, PeerId, ShardRpcAdmission, ShardRpcHandler, ShardRpcLocalEndpoint,
    ShardStageHandler, ShardStageRequest, ShardStageResponse, ShardStageResponseKind,
    SHARD_STAGE_DEFAULT_IDLE_TIMEOUT_MS, SHARD_STAGE_MAX_PAYLOAD_BYTES, SHARD_STAGE_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};

use crate::node_config::NodeConfigStore;
use crate::policy::{PolicyDecision, PolicyEngine};
use crate::shard_plan::{
    sign_stage_receipt, verify_composite_shard_chain, verify_shard_plan,
    CompositeShardVerification, ShardStageAssignment, ShardStageReceiptBody, ShardTrustTier,
    SignedShardPartitionPlan, SignedShardStageReceipt, SHARD_PLAN_SCHEMA_VERSION,
};

/// Context supplied to a local stage backend when executing one slice of a
/// verified [`SignedShardPartitionPlan`].
#[derive(Debug, Clone)]
pub struct ShardStageContext {
    pub plan_id: String,
    pub job_id: String,
    pub model_cid_hex: String,
    pub model_alias: String,
    pub stage_index: u32,
    pub start_layer: u32,
    pub end_layer: u32,
    pub includes_embedding: bool,
    pub includes_lm_head: bool,
    pub trust_label: String,
}

impl ShardStageContext {
    pub fn from_plan_stage(
        plan: &SignedShardPartitionPlan,
        stage: &ShardStageAssignment,
        job_id: &str,
    ) -> Self {
        Self {
            plan_id: plan.plan.plan_id.clone(),
            job_id: job_id.to_string(),
            model_cid_hex: plan.plan.model_cid_hex.clone(),
            model_alias: plan.plan.model_alias.clone(),
            stage_index: stage.stage_index,
            start_layer: stage.start_layer,
            end_layer: stage.end_layer,
            includes_embedding: stage.includes_embedding,
            includes_lm_head: stage.includes_lm_head,
            trust_label: plan.plan.trust_label.clone(),
        }
    }
}

/// Pluggable backend that executes one contiguous layer range on this machine.
///
/// Production deployments bind this to a sandboxed loopback inference runner
/// (e.g. `llama.cpp` RPC/pipeline stage inside `env_clear()` loopback isolation);
/// multi-peer integration tests supply a deterministic executor to exercise the
/// real libp2p transport, authz, signature chaining, and failure boundaries.
#[async_trait]
pub trait ShardStageExecutor: Send + Sync {
    async fn execute_stage(&self, ctx: &ShardStageContext, input_payload: &[u8])
        -> Result<Vec<u8>>;
}

/// Sandboxed loopback HTTP bridge for forwarding a verified stage slice to a
/// local inference stage runner bound strictly to `127.0.0.1`.
pub struct LoopbackStageBridgeExecutor {
    endpoint_url: String,
    client: reqwest::Client,
}

impl LoopbackStageBridgeExecutor {
    pub fn new(port: u16, timeout: Duration) -> Result<Self> {
        if port == 0 {
            bail!("loopback stage bridge port must be non-zero");
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .context("failed to build loopback stage HTTP client")?;
        Ok(Self {
            endpoint_url: format!("http://127.0.0.1:{port}/shard/stage"),
            client,
        })
    }
}

#[async_trait]
impl ShardStageExecutor for LoopbackStageBridgeExecutor {
    async fn execute_stage(
        &self,
        ctx: &ShardStageContext,
        input_payload: &[u8],
    ) -> Result<Vec<u8>> {
        let resp = self
            .client
            .post(&self.endpoint_url)
            .header("x-lucid-plan-id", &ctx.plan_id)
            .header("x-lucid-job-id", &ctx.job_id)
            .header("x-lucid-model-cid", &ctx.model_cid_hex)
            .header("x-lucid-stage-index", ctx.stage_index.to_string())
            .header(
                "x-lucid-layer-range",
                format!("{}..{}", ctx.start_layer, ctx.end_layer),
            )
            .body(input_payload.to_vec())
            .send()
            .await
            .context("loopback stage backend request failed")?;

        if !resp.status().is_success() {
            bail!(
                "loopback stage backend returned HTTP {}",
                resp.status().as_u16()
            );
        }
        let bytes = resp
            .bytes()
            .await
            .context("failed reading loopback stage backend output")?;
        if bytes.is_empty() || bytes.len() > SHARD_STAGE_MAX_PAYLOAD_BYTES {
            bail!(
                "loopback stage output size {} outside 1..={SHARD_STAGE_MAX_PAYLOAD_BYTES}",
                bytes.len()
            );
        }
        Ok(bytes.to_vec())
    }
}

/// Configuration for spawning a sandboxed loopback stage-runner binary
/// (e.g. a SHA-256-pinned `llama.cpp` `rpc-server` or pipeline stage runner).
///
/// Security boundary:
/// - Binary path MUST be absolute and verified by SHA-256 before spawn (no
///   `PATH` lookup).
/// - Child environment is cleared via `env_clear()`.
/// - Listener is strictly bound to `127.0.0.1` so remote peers can NEVER
///   bypass libp2p Noise+TLS, `PolicyEngine` authz, or signed partition plans.
#[derive(Debug, Clone)]
pub struct SandboxedStageProcessConfig {
    pub binary_path: PathBuf,
    pub expected_sha256: String,
    pub loopback_port: u16,
    pub extra_args: Vec<String>,
}

/// Owned sandboxed loopback stage subprocess. Killed automatically on drop.
pub struct SandboxedStageProcess {
    child: Child,
    port: u16,
}

impl SandboxedStageProcess {
    /// Verify the binary's SHA-256 digest and spawn it inside an `env_clear()`
    /// loopback-only sandbox (`127.0.0.1:<port>`).
    pub async fn spawn_verified(cfg: &SandboxedStageProcessConfig) -> Result<Self> {
        if !cfg.binary_path.is_absolute() {
            bail!("stage runner binary path must be absolute");
        }
        if cfg.loopback_port == 0 {
            bail!("stage runner loopback_port must be non-zero");
        }
        let meta = std::fs::symlink_metadata(&cfg.binary_path).with_context(|| {
            format!("cannot inspect stage runner {}", cfg.binary_path.display())
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            bail!("stage runner binary must be a regular non-symlink file");
        }
        let path_for_hash = cfg.binary_path.clone();
        let actual_sha = tokio::task::spawn_blocking(move || {
            crate::runtime_manager::sha256_file_hex(&path_for_hash)
        })
        .await
        .context("stage runner SHA-256 task panicked")??;
        if !actual_sha.eq_ignore_ascii_case(cfg.expected_sha256.trim()) {
            bail!(
                "stage runner binary SHA-256 mismatch (expected {}, got {actual_sha})",
                cfg.expected_sha256
            );
        }

        let mut cmd = Command::new(&cfg.binary_path);
        cmd.env_clear();
        cmd.env("NO_COLOR", "1");
        cmd.arg("--host").arg("127.0.0.1");
        cmd.arg("--port").arg(cfg.loopback_port.to_string());
        for arg in &cfg.extra_args {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn sandboxed stage runner {}",
                cfg.binary_path.display()
            )
        })?;
        Ok(Self {
            child,
            port: cfg.loopback_port,
        })
    }

    pub fn loopback_port(&self) -> u16 {
        self.port
    }

    pub fn bridge_executor(&self, timeout: Duration) -> Result<LoopbackStageBridgeExecutor> {
        LoopbackStageBridgeExecutor::new(self.port, timeout)
    }

    pub async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
    }
}

/// Completed output of an end-to-end Tier-1 multi-peer sharded pipeline run.
#[derive(Debug, Clone)]
pub struct ShardPipelineOutput {
    pub output_payload: Vec<u8>,
    pub receipts: Vec<SignedShardStageReceipt>,
    pub verification: CompositeShardVerification,
}

/// Build the inbound `/phase/shard-stage/1.0.0` libp2p handler gated by live
/// `NodeConfigStore::cluster_enabled` opt-in plus `PolicyEngine` authz/pause.
///
/// Turning `cluster_enabled` off in `node.toml` (via the Desktop app or
/// `/api/lucid/settings`) immediately rejects inbound stage requests with zero
/// daemon restart.
pub fn make_opt_in_inbound_shard_stage_handler(
    identity: Arc<NodeIdentity>,
    local_peer_id: PeerId,
    policy: Arc<PolicyEngine>,
    node_config: Arc<NodeConfigStore>,
    executor: Arc<dyn ShardStageExecutor>,
) -> ShardStageHandler {
    let inner = make_inbound_shard_stage_handler(identity, local_peer_id, policy, executor);
    Arc::new(move |caller_peer: PeerId, request: ShardStageRequest| {
        let inner = inner.clone();
        let node_config = node_config.clone();
        Box::pin(async move {
            if !node_config.snapshot().cluster_enabled {
                return ShardStageResponse {
                    schema_version: SHARD_STAGE_SCHEMA_VERSION,
                    session_id: request.session_id,
                    stage_index: request.stage_index,
                    kind: ShardStageResponseKind::Rejected {
                        reason: "trusted-circle cluster mode is disabled by operator preference"
                            .to_string(),
                    },
                };
            }
            inner(caller_peer, request).await
        })
    })
}

/// Build the inbound `/phase/shard-stage/1.0.0` libp2p handler for a serving
/// node participating in a Trusted Circle.
///
/// Enforces:
/// 1. `PolicyEngine` serving & pause gates (manual pause, battery, thermal).
/// 2. Default-deny caller authorization (`authorized_submitters` /
///    `allow_unauthenticated_jobs` — fail closed for unknown peers).
/// 3. Cryptographic verification of `SignedShardPartitionPlan` (signature,
///    expiry, contiguous layer coverage, `Tier 1 TrustedCircle` only).
/// 4. Strict binding of `stage_index`, `local_peer_id`, and `local_verifying_key`.
/// 5. Exact SHA-256 input/output commitment signing into `SignedShardStageReceipt`.
pub fn make_inbound_shard_stage_handler(
    identity: Arc<NodeIdentity>,
    local_peer_id: PeerId,
    policy: Arc<PolicyEngine>,
    executor: Arc<dyn ShardStageExecutor>,
) -> ShardStageHandler {
    let local_peer_str = local_peer_id.to_string();
    let local_pubkey_hex = hex::encode(identity.peer_id_bytes());

    Arc::new(move |caller_peer: PeerId, request: ShardStageRequest| {
        let identity = identity.clone();
        let policy = policy.clone();
        let executor = executor.clone();
        let local_peer_str = local_peer_str.clone();
        let local_pubkey_hex = local_pubkey_hex.clone();

        Box::pin(async move {
            let reject = |reason: String| ShardStageResponse {
                schema_version: SHARD_STAGE_SCHEMA_VERSION,
                session_id: request.session_id,
                stage_index: request.stage_index,
                kind: ShardStageResponseKind::Rejected { reason },
            };
            let fail = |reason: String| ShardStageResponse {
                schema_version: SHARD_STAGE_SCHEMA_VERSION,
                session_id: request.session_id,
                stage_index: request.stage_index,
                kind: ShardStageResponseKind::Failed { reason },
            };

            let envelope: InboundPlanEnvelope = match serde_json::from_slice(&request.plan_bytes) {
                Ok(env) => env,
                Err(_) => return reject("malformed shard plan envelope".to_string()),
            };

            // 1. Default-deny Trusted-Circle caller authorization.
            if !is_caller_authorized(&policy, &caller_peer, &envelope.signed_plan) {
                return reject(
                    "caller peer is not authorized for trusted-circle sharded execution"
                        .to_string(),
                );
            }

            // 2. Cryptographically verify the coordinator-signed plan.
            let now_ms = unix_time_ms();
            if let Err(err) = verify_shard_plan(&envelope.signed_plan, now_ms) {
                return reject(format!("invalid shard partition plan: {err}"));
            }
            if envelope.signed_plan.plan.trust_tier != ShardTrustTier::TrustedCircle {
                return reject("untrusted sharded execution is disabled".to_string());
            }

            // 3. Enforce operator policy (manual pause, battery, thermal, model allowlist).
            match policy.should_serve(&envelope.signed_plan.plan.model_alias, 0) {
                PolicyDecision::Allow => {}
                PolicyDecision::Pause { reason } => {
                    return reject(format!("node policy paused shard execution: {reason:?}"));
                }
            }

            // 4. Verify this exact stage is assigned to THIS peer and verifying key.
            let Some(stage) = envelope
                .signed_plan
                .plan
                .stages
                .get(request.stage_index as usize)
            else {
                return reject("requested stage_index out of bounds".to_string());
            };
            if stage.stage_index != request.stage_index
                || stage.peer_id != local_peer_str
                || !stage
                    .verifying_key_hex
                    .eq_ignore_ascii_case(&local_pubkey_hex)
            {
                return reject("stage assignment does not match this peer's identity".to_string());
            }

            // 5. Execute the assigned layer slice and commit exact wire bytes.
            let input_commitment_hex = sha256_hex(&request.input_payload);
            let ctx =
                ShardStageContext::from_plan_stage(&envelope.signed_plan, stage, &envelope.job_id);
            let output_payload = match executor.execute_stage(&ctx, &request.input_payload).await {
                Ok(out) if !out.is_empty() && out.len() <= SHARD_STAGE_MAX_PAYLOAD_BYTES => out,
                Ok(_) => return fail("stage executor returned empty or oversized output".into()),
                Err(err) => return fail(format!("stage execution failed: {err}")),
            };
            let output_commitment_hex = sha256_hex(&output_payload);

            let receipt_body = ShardStageReceiptBody {
                schema_version: SHARD_PLAN_SCHEMA_VERSION,
                plan_id: envelope.signed_plan.plan.plan_id.clone(),
                job_id: envelope.job_id,
                model_cid_hex: envelope.signed_plan.plan.model_cid_hex.clone(),
                stage_index: stage.stage_index,
                start_layer: stage.start_layer,
                end_layer: stage.end_layer,
                peer_id: local_peer_str,
                worker_pubkey_hex: local_pubkey_hex,
                input_commitment_hex,
                output_commitment_hex,
                completed_at_ms: unix_time_ms(),
            };
            let signed_receipt = match sign_stage_receipt(&identity, receipt_body) {
                Ok(r) => r,
                Err(err) => return fail(format!("failed to sign stage receipt: {err}")),
            };
            let receipt_bytes = match serde_json::to_vec(&signed_receipt) {
                Ok(b) => b,
                Err(err) => return fail(format!("failed to encode stage receipt: {err}")),
            };

            ShardStageResponse {
                schema_version: SHARD_STAGE_SCHEMA_VERSION,
                session_id: request.session_id,
                stage_index: request.stage_index,
                kind: ShardStageResponseKind::Completed {
                    output_payload,
                    receipt_bytes,
                },
            }
        })
    })
}

/// How often a live shard-rpc bridge session re-checks the authorization that
/// admitted it. Revoking `cluster_enabled`, the caller's enrollment, or
/// flipping the manual pause tears the session down within this interval.
pub const SHARD_RPC_REAUTHORIZE_INTERVAL: Duration = Duration::from_millis(250);
const SHARD_RPC_BRIDGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Build the inbound `/phase/shard-rpc/1.0.0` admission handler for a Tier-1
/// Trusted-Circle provider.
///
/// Each admitted tunnel is bridged onto a fresh TCP connection to
/// `127.0.0.1:<bridge_port>`, where the operator runs
/// `ggml-rpc-server -H 127.0.0.1 -p <bridge_port> -c`. `ggml-rpc` has no
/// authentication of its own, so this handler is the whole access boundary:
///
/// 1. The authenticated libp2p caller is an *enrolled* peer: its Ed25519 key
///    is in `authorized_submitters` or holds an unexpired enrollment grant.
///    `allow_unauthenticated_jobs` never opens a raw tunnel; while that
///    development escape hatch is on, only an explicit `authorized_submitters`
///    entry admits a caller. Un-enrolled peers are dropped by the peer
///    pre-filter before `phase-net` reads anything from them, so they cannot
///    hold handshake slots or learn the node's settings.
/// 2. Live `cluster_enabled = true` in `node.toml` (checked per tunnel, no
///    restart needed to turn it off).
/// 3. The operator's manual pause is not engaged.
///
/// The same checks re-run every [`SHARD_RPC_REAUTHORIZE_INTERVAL`] for as
/// long as a session lives; the first failure drops the bridge connection
/// (and with it the remote coordinator's RPC session) fail-closed.
pub fn make_opt_in_inbound_shard_rpc_handler(
    policy: Arc<PolicyEngine>,
    node_config: Arc<NodeConfigStore>,
    bridge_port: u16,
) -> Result<ShardRpcHandler> {
    if bridge_port == 0 {
        bail!("shard rpc bridge port must be non-zero");
    }
    let filter_policy = policy.clone();
    let handler = ShardRpcHandler::new(move |caller: PeerId, _open| {
        let policy = policy.clone();
        let node_config = node_config.clone();
        async move {
            if let Some(reason) = shard_rpc_refusal(&policy, &node_config, &caller) {
                tracing::info!(peer = %caller, reason, "refusing shard rpc tunnel");
                return ShardRpcAdmission::reject(reason);
            }
            let connect = tokio::time::timeout(
                SHARD_RPC_BRIDGE_CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge_port)),
            )
            .await;
            let bridge = match connect {
                Ok(Ok(bridge)) => bridge,
                Ok(Err(error)) => {
                    tracing::warn!(peer = %caller, bridge_port, %error, "shard rpc loopback bridge is unreachable");
                    return ShardRpcAdmission::reject("local shard rpc backend is unavailable");
                }
                Err(_) => {
                    tracing::warn!(peer = %caller, bridge_port, "shard rpc loopback bridge connect timed out");
                    return ShardRpcAdmission::reject("local shard rpc backend is unavailable");
                }
            };
            // ggml-rpc is chatty and latency-bound (one small command per
            // graph step); never let Nagle batch it.
            let _ = bridge.set_nodelay(true);
            // Authorization may have changed while connecting.
            if let Some(reason) = shard_rpc_refusal(&policy, &node_config, &caller) {
                return ShardRpcAdmission::reject(reason);
            }
            tracing::info!(peer = %caller, bridge_port, "admitted shard rpc tunnel");
            let revoked = shard_rpc_authorization_revoked(policy, node_config, caller);
            ShardRpcAdmission::Accept(ShardRpcLocalEndpoint::new(bridge).with_revocation(revoked))
        }
    });
    Ok(handler.with_peer_filter(move |peer| is_enrolled_shard_rpc_peer(&filter_policy, peer)))
}

/// Uniform refusal for any caller that is not an enrolled member.
const SHARD_RPC_NOT_ENROLLED: &str = "caller peer is not an enrolled trusted-circle member";

/// `None` when `caller` may hold a shard-rpc session right now, otherwise the
/// refusal reason sent back to the caller. Enrollment is checked first, so a
/// stranger learns nothing about the node's other settings.
fn shard_rpc_refusal(
    policy: &PolicyEngine,
    node_config: &NodeConfigStore,
    caller: &PeerId,
) -> Option<&'static str> {
    if !is_enrolled_shard_rpc_peer(policy, caller) {
        return Some(SHARD_RPC_NOT_ENROLLED);
    }
    if !node_config.snapshot().cluster_enabled {
        return Some("trusted-circle cluster mode is disabled by operator preference");
    }
    if policy.config().manual_pause {
        return Some("node is manually paused");
    }
    None
}

async fn shard_rpc_authorization_revoked(
    policy: Arc<PolicyEngine>,
    node_config: Arc<NodeConfigStore>,
    caller: PeerId,
) {
    // `interval` ticks immediately: re-check before the first sleep.
    let mut checks = tokio::time::interval(SHARD_RPC_REAUTHORIZE_INTERVAL);
    loop {
        checks.tick().await;
        if let Some(reason) = shard_rpc_refusal(&policy, &node_config, &caller) {
            tracing::warn!(peer = %caller, reason, "revoking active shard rpc tunnel");
            return;
        }
    }
}

/// Explicit allowlist entry or unexpired enrollment grant — never the
/// `allow_unauthenticated_jobs` development escape hatch.
fn is_enrolled_shard_rpc_peer(policy: &PolicyEngine, caller: &PeerId) -> bool {
    let Some(pubkey_hex) = ed25519_pubkey_hex_for_peer(caller) else {
        return false;
    };
    policy.is_explicitly_enrolled_submitter(&pubkey_hex)
}

/// Lowercase-hex Ed25519 key that `peer` was derived from, or `None` for any
/// non-Ed25519 or non-inline PeerId (fail closed). The key is re-derived and
/// compared, so the result always binds to exactly this PeerId.
fn ed25519_pubkey_hex_for_peer(peer: &PeerId) -> Option<String> {
    use phase_net::libp2p_identity::PublicKey;
    const IDENTITY_MULTIHASH_CODE: u64 = 0x00;
    let multihash = peer.as_ref();
    if multihash.code() != IDENTITY_MULTIHASH_CODE {
        return None;
    }
    let key = PublicKey::try_decode_protobuf(multihash.digest())
        .ok()?
        .try_into_ed25519()
        .ok()?;
    let pubkey_hex = hex::encode(key.to_bytes());
    (PeerId::from(PublicKey::from(key)) == *peer).then_some(pubkey_hex)
}

/// Wire envelope carried inside `ShardStageRequest::plan_bytes` (opaque to `phase-net`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct InboundPlanEnvelope {
    job_id: String,
    signed_plan: SignedShardPartitionPlan,
}

fn is_caller_authorized(
    policy: &PolicyEngine,
    caller_peer: &PeerId,
    plan: &SignedShardPartitionPlan,
) -> bool {
    let cfg = policy.config();
    if cfg.allow_unauthenticated_jobs {
        return true;
    }
    // Check whether the coordinator's Ed25519 verifying key derives to the
    // authenticated libp2p caller PeerId AND is explicitly allowlisted in the
    // operator's trusted submitter keys.
    let Ok(coord_bytes) = hex::decode(&plan.plan.coordinator_pubkey_hex) else {
        return false;
    };
    let Ok(coord_arr) = <[u8; 32]>::try_from(coord_bytes.as_slice()) else {
        return false;
    };
    let Ok(libp2p_ed) = phase_net::libp2p_identity::ed25519::PublicKey::try_from_bytes(&coord_arr)
    else {
        return false;
    };
    let expected_peer = PeerId::from(phase_net::libp2p_identity::PublicKey::from(libp2p_ed));
    if expected_peer != *caller_peer {
        return false;
    }
    policy.is_authorized_submitter(&plan.plan.coordinator_pubkey_hex)
}

/// Execute a verified multi-stage Tier-1 Trusted-Circle pipeline across local
/// and remote peers over `/phase/shard-stage/1.0.0`, verifying every intermediate
/// handoff and the complete composite receipt chain before returning.
pub async fn execute_sharded_pipeline(
    discovery: &Discovery,
    local_identity: &NodeIdentity,
    signed_plan: &SignedShardPartitionPlan,
    job_id: &str,
    initial_input: &[u8],
    local_executor: Option<&dyn ShardStageExecutor>,
    per_stage_timeout: Duration,
) -> Result<ShardPipelineOutput> {
    if initial_input.is_empty() || initial_input.len() > SHARD_STAGE_MAX_PAYLOAD_BYTES {
        bail!(
            "initial pipeline input size {} outside 1..={SHARD_STAGE_MAX_PAYLOAD_BYTES}",
            initial_input.len()
        );
    }
    let now_ms = unix_time_ms();
    verify_shard_plan(signed_plan, now_ms).context("cannot execute invalid shard plan")?;

    let envelope_bytes = serde_json::to_vec(&InboundPlanEnvelope {
        job_id: job_id.to_string(),
        signed_plan: signed_plan.clone(),
    })
    .context("failed to serialize shard plan envelope")?;

    let local_peer_str = discovery.local_peer_id().to_string();
    let local_pubkey_hex = hex::encode(local_identity.peer_id_bytes());
    let prompt_commitment_hex = sha256_hex(initial_input);

    let mut current_payload = initial_input.to_vec();
    let mut receipts = Vec::with_capacity(signed_plan.plan.stages.len());
    let idle_ms = per_stage_timeout
        .as_millis()
        .clamp(250, u128::from(SHARD_STAGE_DEFAULT_IDLE_TIMEOUT_MS * 4)) as u64;

    for stage in &signed_plan.plan.stages {
        if stage.peer_id == local_peer_str {
            let executor = local_executor.ok_or_else(|| {
                anyhow!(
                    "stage {} assigned to local peer but no local stage executor is configured",
                    stage.stage_index
                )
            })?;
            let in_hash = sha256_hex(&current_payload);
            let ctx = ShardStageContext::from_plan_stage(signed_plan, stage, job_id);
            let out_payload = executor
                .execute_stage(&ctx, &current_payload)
                .await
                .with_context(|| format!("local stage {} execution failed", stage.stage_index))?;
            let out_hash = sha256_hex(&out_payload);
            let receipt = sign_stage_receipt(
                local_identity,
                ShardStageReceiptBody {
                    schema_version: SHARD_PLAN_SCHEMA_VERSION,
                    plan_id: signed_plan.plan.plan_id.clone(),
                    job_id: job_id.to_string(),
                    model_cid_hex: signed_plan.plan.model_cid_hex.clone(),
                    stage_index: stage.stage_index,
                    start_layer: stage.start_layer,
                    end_layer: stage.end_layer,
                    peer_id: local_peer_str.clone(),
                    worker_pubkey_hex: local_pubkey_hex.clone(),
                    input_commitment_hex: in_hash,
                    output_commitment_hex: out_hash,
                    completed_at_ms: unix_time_ms(),
                },
            )?;
            receipts.push(receipt);
            current_payload = out_payload;
        } else {
            let peer_id: PeerId = stage.peer_id.parse().with_context(|| {
                format!(
                    "invalid PeerId '{}' in stage {}",
                    stage.peer_id, stage.stage_index
                )
            })?;
            let session_id =
                derive_session_id(&signed_plan.plan.plan_id, job_id, stage.stage_index);
            let req_now = unix_time_ms();
            let request = ShardStageRequest {
                schema_version: SHARD_STAGE_SCHEMA_VERSION,
                session_id,
                stage_index: stage.stage_index,
                plan_bytes: envelope_bytes.clone(),
                input_payload: current_payload.clone(),
                deadline_unix_ms: req_now.saturating_add(idle_ms.saturating_mul(2)),
                idle_timeout_ms: idle_ms,
            };
            let response = discovery
                .execute_shard_stage(peer_id, request)
                .await
                .with_context(|| {
                    format!(
                        "remote shard stage {} on peer {} failed",
                        stage.stage_index, stage.peer_id
                    )
                })?;

            match response.kind {
                ShardStageResponseKind::Completed {
                    output_payload,
                    receipt_bytes,
                } => {
                    let receipt: SignedShardStageReceipt = serde_json::from_slice(&receipt_bytes)
                        .with_context(|| {
                        format!("malformed stage {} receipt from peer", stage.stage_index)
                    })?;
                    let expected_out = sha256_hex(&output_payload);
                    if !receipt
                        .body
                        .output_commitment_hex
                        .eq_ignore_ascii_case(&expected_out)
                    {
                        bail!(
                            "stage {} peer returned output bytes that do not match its signed receipt",
                            stage.stage_index
                        );
                    }
                    receipts.push(receipt);
                    current_payload = output_payload;
                }
                ShardStageResponseKind::Rejected { reason } => {
                    bail!(
                        "stage {} rejected by peer {}: {reason}",
                        stage.stage_index,
                        stage.peer_id
                    );
                }
                ShardStageResponseKind::Failed { reason } => {
                    bail!(
                        "stage {} failed on peer {}: {reason}",
                        stage.stage_index,
                        stage.peer_id
                    );
                }
            }
        }
    }

    let final_output_hex = sha256_hex(&current_payload);
    let verification = verify_composite_shard_chain(
        signed_plan,
        &receipts,
        job_id,
        &prompt_commitment_hex,
        &final_output_hex,
        unix_time_ms(),
    )
    .context("composite shard receipt chain verification failed")?;

    Ok(ShardPipelineOutput {
        output_payload: current_payload,
        receipts,
        verification,
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn derive_session_id(plan_id: &str, job_id: &str, stage_index: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"phase/shard-stage-session/v1\0");
    hasher.update(plan_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(job_id.as_bytes());
    hasher.update(stage_index.to_be_bytes());
    hasher.finalize().into()
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Shared two-node fixtures for the shard-rpc provider and coordinator tests.
#[cfg(test)]
pub(crate) mod shard_rpc_test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use phase_identity::NodeIdentity;
    use phase_net::{Discovery, DiscoveryConfig, Multiaddr, PeerId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::node_config::{NodeConfig, NodeConfigStore};
    use crate::policy::{PolicyConfig, PolicyEngine, PolicyState};

    /// Real libp2p swarms on one host contend for CPU during protocol
    /// negotiation; serialize only these heavyweight tests.
    pub(crate) async fn two_node_gate() -> tokio::sync::SemaphorePermit<'static> {
        static GATE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
        GATE.get_or_init(|| tokio::sync::Semaphore::new(1))
            .acquire()
            .await
            .expect("two-node test gate stays open")
    }

    pub(crate) fn peer_id_of(identity: &NodeIdentity) -> PeerId {
        use phase_net::libp2p_identity::{ed25519, PublicKey};
        let key = ed25519::PublicKey::try_from_bytes(&identity.peer_id_bytes())
            .expect("node identity holds a valid Ed25519 key");
        PeerId::from(PublicKey::from(key))
    }

    pub(crate) fn discovery_for(identity: &NodeIdentity) -> Arc<Discovery> {
        Arc::new(
            Discovery::new(DiscoveryConfig {
                identity: Some(identity.clone()),
                ..DiscoveryConfig::default()
            })
            .expect("discovery"),
        )
    }

    /// `client` dials `server` over loopback TCP (Noise + yamux) and waits
    /// until the connection is established.
    pub(crate) async fn connect(client: &Discovery, server: &Discovery) {
        server
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("server listens");
        let mut address: Option<Multiaddr> = None;
        for _ in 0..100 {
            if let Some(found) = server.listen_addrs().await.unwrap().into_iter().next() {
                address = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let address = address.expect("server reports a listen address");
        client
            .dial_peer(&format!("{address}/p2p/{}", server.local_peer_id()))
            .await
            .expect("client dials server");
        for _ in 0..250 {
            let snapshot = client.reachability_snapshot().await.unwrap();
            if snapshot
                .connections
                .iter()
                .any(|connection| connection.peer_id == *server.local_peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("client never connected to server");
    }

    /// Policy that enrolls exactly `enrolled` (by Ed25519 key) and nothing else.
    pub(crate) fn policy_enrolling(enrolled: &[&NodeIdentity]) -> Arc<PolicyEngine> {
        Arc::new(PolicyEngine::new_for_tests(
            PolicyConfig {
                authorized_submitters: enrolled
                    .iter()
                    .map(|identity| hex::encode(identity.peer_id_bytes()))
                    .collect(),
                ..PolicyConfig::default()
            },
            PolicyState::default(),
        ))
    }

    pub(crate) fn cluster_config(enabled: bool) -> Arc<NodeConfigStore> {
        NodeConfigStore::in_memory(NodeConfig {
            cluster_enabled: enabled,
            ..NodeConfig::default()
        })
    }

    pub(crate) fn set_cluster(store: &NodeConfigStore, enabled: bool) {
        store
            .update(|config| {
                config.cluster_enabled = enabled;
                Ok(())
            })
            .expect("update node config");
    }

    /// Loopback TCP echo server standing in for an operator's
    /// `ggml-rpc-server`. Counts accepted connections and live connections.
    pub(crate) struct EchoBackend {
        pub(crate) port: u16,
        pub(crate) accepted: Arc<AtomicUsize>,
        pub(crate) live: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl EchoBackend {
        pub(crate) async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind echo backend");
            let port = listener.local_addr().unwrap().port();
            let accepted = Arc::new(AtomicUsize::new(0));
            let live = Arc::new(AtomicUsize::new(0));
            let (accepted_count, live_count) = (accepted.clone(), live.clone());
            let task = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    accepted_count.fetch_add(1, Ordering::SeqCst);
                    live_count.fetch_add(1, Ordering::SeqCst);
                    let live_count = live_count.clone();
                    tokio::spawn(async move {
                        let mut buffer = vec![0_u8; 64 * 1024];
                        loop {
                            match socket.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(read) => {
                                    if socket.write_all(&buffer[..read]).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        live_count.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
            Self {
                port,
                accepted,
                live,
                task,
            }
        }

        pub(crate) async fn wait_live(&self, expected: usize, within: Duration) -> bool {
            wait_count(&self.live, expected, within).await
        }

        pub(crate) async fn wait_accepted(&self, expected: usize, within: Duration) -> bool {
            wait_count(&self.accepted, expected, within).await
        }
    }

    /// Wait until `counter == expected`, then confirm it stays there briefly
    /// (so an overshoot racing in is still caught).
    async fn wait_count(counter: &AtomicUsize, expected: usize, within: Duration) -> bool {
        let started = tokio::time::Instant::now();
        while started.elapsed() < within {
            if counter.load(Ordering::SeqCst) == expected {
                tokio::time::sleep(Duration::from_millis(50)).await;
                return counter.load(Ordering::SeqCst) == expected;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    impl Drop for EchoBackend {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Deterministic non-repeating test bytes.
    pub(crate) fn pattern(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xff) as u8
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_config::NodeConfig;

    #[tokio::test]
    async fn sandboxed_stage_process_rejects_relative_and_sha_mismatch_fail_closed() {
        // Relative path rejected before touching filesystem.
        let rel = SandboxedStageProcess::spawn_verified(&SandboxedStageProcessConfig {
            binary_path: PathBuf::from("rpc-server"),
            expected_sha256: "0".repeat(64),
            loopback_port: 19991,
            extra_args: Vec::new(),
        })
        .await;
        assert!(rel.is_err());

        // Real file with wrong SHA-256 rejected before spawn.
        let dir = tempfile::tempdir().unwrap();
        let fake_bin = dir.path().join("fake-rpc");
        std::fs::write(&fake_bin, b"#!/bin/sh\necho nope\n").unwrap();
        let bad_sha = SandboxedStageProcess::spawn_verified(&SandboxedStageProcessConfig {
            binary_path: fake_bin,
            expected_sha256: "f".repeat(64),
            loopback_port: 19992,
            extra_args: Vec::new(),
        })
        .await;
        assert!(bad_sha
            .err()
            .map(|e| e.to_string().contains("SHA-256 mismatch"))
            .unwrap_or(false));
    }

    struct NoopExec;

    #[async_trait]
    impl ShardStageExecutor for NoopExec {
        async fn execute_stage(
            &self,
            _ctx: &ShardStageContext,
            input_payload: &[u8],
        ) -> Result<Vec<u8>> {
            Ok(input_payload.to_vec())
        }
    }

    #[tokio::test]
    async fn opt_in_gate_rejects_immediately_while_cluster_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let id = Arc::new(NodeIdentity::generate());
        let peer_id = PeerId::random();
        let policy = Arc::new(
            PolicyEngine::load_or_default(Some(dir.path().join("policy.toml")))
                .await
                .unwrap(),
        );
        let node_cfg = NodeConfigStore::in_memory(NodeConfig::default()); // cluster_enabled = false
        let handler = make_opt_in_inbound_shard_stage_handler(
            id,
            peer_id,
            policy,
            node_cfg,
            Arc::new(NoopExec),
        );
        let resp = handler(
            PeerId::random(),
            ShardStageRequest {
                schema_version: SHARD_STAGE_SCHEMA_VERSION,
                session_id: [1_u8; 32],
                stage_index: 0,
                plan_bytes: b"{}".to_vec(),
                input_payload: b"hi".to_vec(),
                deadline_unix_ms: unix_time_ms() + 10_000,
                idle_timeout_ms: 5_000,
            },
        )
        .await;
        match resp.kind {
            ShardStageResponseKind::Rejected { reason } => {
                assert!(reason.contains("cluster mode is disabled"));
            }
            other => panic!("expected Rejected when cluster_enabled=false, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // /phase/shard-rpc/1.0.0 provider bridge
    // -----------------------------------------------------------------

    use super::shard_rpc_test_support::*;
    use phase_net::{ShardRpcOpen, ShardRpcSessionEnd};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rpc_open(session: u8) -> ShardRpcOpen {
        ShardRpcOpen::new([session; 32])
    }

    async fn admit(handler: &ShardRpcHandler, caller: PeerId) -> Result<(), String> {
        match handler.admit(caller, rpc_open(1)).await {
            ShardRpcAdmission::Accept(_) => Ok(()),
            ShardRpcAdmission::Reject { reason } => Err(reason),
        }
    }

    #[test]
    fn shard_rpc_caller_key_binds_exactly_to_its_peer_id() {
        let identity = NodeIdentity::generate();
        let peer = peer_id_of(&identity);
        assert_eq!(
            ed25519_pubkey_hex_for_peer(&peer).as_deref(),
            Some(hex::encode(identity.peer_id_bytes()).as_str())
        );
        // A PeerId that does not inline an Ed25519 key never authorizes.
        assert_eq!(ed25519_pubkey_hex_for_peer(&PeerId::random()), None);
    }

    #[tokio::test]
    async fn shard_rpc_admission_fails_closed_on_every_gate() {
        let enrolled = NodeIdentity::generate();
        let stranger = NodeIdentity::generate();
        let backend = EchoBackend::start().await;
        let node_config = cluster_config(false);
        let policy = policy_enrolling(&[&enrolled]);
        let handler = make_opt_in_inbound_shard_rpc_handler(
            policy.clone(),
            node_config.clone(),
            backend.port,
        )
        .unwrap();

        // 0. Strangers are filtered before phase-net reads anything, and get
        //    one uniform refusal that reveals none of the node's settings.
        assert!(handler.accepts_peer(&peer_id_of(&enrolled)));
        assert!(!handler.accepts_peer(&peer_id_of(&stranger)));
        assert!(!handler.accepts_peer(&PeerId::random()));
        let refused = admit(&handler, peer_id_of(&stranger)).await.unwrap_err();
        assert_eq!(refused, SHARD_RPC_NOT_ENROLLED);

        // 1. cluster_enabled = false (the default) refuses even enrolled peers.
        let refused = admit(&handler, peer_id_of(&enrolled)).await.unwrap_err();
        assert!(refused.contains("cluster mode is disabled"), "{refused}");

        // 2. Opted in, but the caller is not enrolled.
        set_cluster(&node_config, true);
        let refused = admit(&handler, peer_id_of(&stranger)).await.unwrap_err();
        assert_eq!(refused, SHARD_RPC_NOT_ENROLLED);
        assert!(admit(&handler, PeerId::random()).await.is_err());

        // 3. The open-jobs development escape hatch never opens a raw tunnel.
        policy
            .update_config(|config| config.allow_unauthenticated_jobs = true)
            .unwrap();
        let refused = admit(&handler, peer_id_of(&stranger)).await.unwrap_err();
        assert_eq!(refused, SHARD_RPC_NOT_ENROLLED);
        assert!(!handler.accepts_peer(&peer_id_of(&stranger)));
        // An explicit allowlist entry still counts in that mode.
        admit(&handler, peer_id_of(&enrolled)).await.unwrap();
        policy
            .update_config(|config| config.allow_unauthenticated_jobs = false)
            .unwrap();

        // 4. The operator's manual pause wins.
        policy.set_manual_pause(true).await;
        let refused = admit(&handler, peer_id_of(&enrolled)).await.unwrap_err();
        assert!(refused.contains("manually paused"), "{refused}");
        policy.set_manual_pause(false).await;

        // 5. Every gate holds: admitted onto a loopback bridge connection.
        admit(&handler, peer_id_of(&enrolled)).await.unwrap();
        assert!(
            backend.wait_accepted(2, Duration::from_secs(2)).await,
            "refused callers must never reach the loopback backend; the two admitted ones must"
        );

        // 6. A missing backend is refused, not left hanging.
        let closed_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let orphan =
            make_opt_in_inbound_shard_rpc_handler(policy.clone(), node_config, closed_port)
                .unwrap();
        let refused = admit(&orphan, peer_id_of(&enrolled)).await.unwrap_err();
        assert!(refused.contains("backend is unavailable"), "{refused}");

        assert!(make_opt_in_inbound_shard_rpc_handler(policy, cluster_config(true), 0).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn enrolled_two_node_tunnel_bridges_loopback_tcp_and_drops_when_cluster_disabled() {
        let _gate = two_node_gate().await;
        let provider_identity = NodeIdentity::generate();
        let coordinator_identity = NodeIdentity::generate();
        let provider = discovery_for(&provider_identity);
        let coordinator = discovery_for(&coordinator_identity);
        connect(&coordinator, &provider).await;

        let backend = EchoBackend::start().await;
        let node_config = cluster_config(true);
        provider
            .set_shard_rpc_handler(Some(
                make_opt_in_inbound_shard_rpc_handler(
                    policy_enrolling(&[&coordinator_identity]),
                    node_config.clone(),
                    backend.port,
                )
                .unwrap(),
            ))
            .unwrap();

        let tunnel = coordinator
            .open_shard_rpc_tunnel(*provider.local_peer_id(), rpc_open(2))
            .await
            .expect("enrolled coordinator is admitted");
        let (splice_io, mut local) = tokio::io::duplex(256 * 1024);
        let session = tokio::spawn(tunnel.splice(splice_io, None));

        // Coordinator loopback -> libp2p (Noise) -> provider -> loopback TCP
        // backend, and back: a multi-MiB payload arrives intact.
        let payload = pattern(2 * 1024 * 1024 + 3, 0xabc);
        let expected = payload.clone();
        let (mut reader, mut writer) = tokio::io::split(local);
        let write = tokio::spawn(async move {
            writer.write_all(&payload).await.unwrap();
            writer
        });
        let mut echoed = vec![0_u8; expected.len()];
        tokio::time::timeout(Duration::from_secs(30), reader.read_exact(&mut echoed))
            .await
            .expect("echo completes")
            .unwrap();
        assert!(echoed == expected, "bytes survive the round trip intact");
        local = reader.unsplit(write.await.unwrap());
        assert!(backend.wait_live(1, Duration::from_secs(2)).await);

        // Turning cluster_enabled off mid-session drops the live bridge
        // connection and the coordinator's tunnel, without a restart.
        set_cluster(&node_config, false);
        assert!(
            backend.wait_live(0, Duration::from_secs(3)).await,
            "provider must drop its loopback bridge connection"
        );
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), local.read_to_end(&mut rest))
            .await
            .expect("coordinator endpoint observes the tunnel ending")
            .unwrap();
        drop(local);
        let report = tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .expect("coordinator session ends")
            .unwrap();
        assert_eq!(report.end, ShardRpcSessionEnd::RemoteClosed, "{report:?}");

        // And no new tunnel is admitted while it stays off.
        let refused = coordinator
            .open_shard_rpc_tunnel(*provider.local_peer_id(), rpc_open(3))
            .await
            .expect_err("cluster disabled refuses new tunnels");
        assert!(
            refused.to_string().contains("cluster mode is disabled"),
            "{refused:#}"
        );
        assert_eq!(
            backend.accepted.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unenrolled_peer_is_refused_and_withdrawn_enrollment_drops_live_tunnel() {
        let _gate = two_node_gate().await;
        let provider_identity = NodeIdentity::generate();
        let enrolled_identity = NodeIdentity::generate();
        let stranger_identity = NodeIdentity::generate();
        let provider = discovery_for(&provider_identity);
        let enrolled = discovery_for(&enrolled_identity);
        let stranger = discovery_for(&stranger_identity);
        connect(&enrolled, &provider).await;
        connect(&stranger, &provider).await;

        let backend = EchoBackend::start().await;
        let policy = policy_enrolling(&[&enrolled_identity]);
        provider
            .set_shard_rpc_handler(Some(
                make_opt_in_inbound_shard_rpc_handler(
                    policy.clone(),
                    cluster_config(true),
                    backend.port,
                )
                .unwrap(),
            ))
            .unwrap();

        // An authenticated but un-enrolled peer gets nothing — not even a
        // refusal reason: its stream is dropped before anything is read.
        let refused = stranger
            .open_shard_rpc_tunnel(*provider.local_peer_id(), rpc_open(4))
            .await
            .expect_err("un-enrolled peer is refused");
        let refused = format!("{refused:#}");
        assert!(
            !refused.contains("cluster") && !refused.contains("paused"),
            "strangers must not learn the node's settings: {refused}"
        );
        assert_eq!(
            backend.accepted.load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        let tunnel = enrolled
            .open_shard_rpc_tunnel(*provider.local_peer_id(), rpc_open(5))
            .await
            .expect("enrolled peer is admitted");
        let (splice_io, mut local) = tokio::io::duplex(64 * 1024);
        let session = tokio::spawn(tunnel.splice(splice_io, None));
        local.write_all(b"hello").await.unwrap();
        let mut echoed = [0_u8; 5];
        local.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"hello");

        // Withdrawing the enrollment drops the live tunnel fail-closed.
        policy
            .update_config(|config| config.authorized_submitters.clear())
            .unwrap();
        assert!(backend.wait_live(0, Duration::from_secs(3)).await);
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), local.read_to_end(&mut rest))
            .await
            .expect("enrolled endpoint observes revocation")
            .unwrap();
        drop(local);
        let report = tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.end, ShardRpcSessionEnd::RemoteClosed, "{report:?}");
    }
}
