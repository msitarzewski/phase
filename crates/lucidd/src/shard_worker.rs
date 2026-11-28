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
//! enrolled coordinator's opaque byte tunnel is bridged onto a loopback
//! `ggml-rpc-server` (see [`make_opt_in_inbound_shard_rpc_handler`]), either
//! operator-run (`--shard-rpc-bridge-port`) or, with zero CLI, the pinned
//! build that [`ManagedShardRpcProvider`] runs only while `cluster_enabled`
//! is on.

use std::path::{Path, PathBuf};
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

// ---------------------------------------------------------------------------
// Zero-CLI auto-managed loopback `ggml-rpc-server` (Tier-1 provider)
// ---------------------------------------------------------------------------

/// File name of the `ggml-rpc` backend server that every pinned `llama.cpp`
/// release archive ships next to `llama-server`.
pub const GGML_RPC_SERVER_BINARY_NAME: &str = "ggml-rpc-server";

/// SHA-256 of `ggml-rpc-server` inside each pinned `b12651` archive of
/// [`crate::runtime_manager::PINNED_LLAMA_ASSETS`], keyed by `platform_id`.
/// Each digest was taken from an archive whose own pinned SHA-256 matched.
///
/// The executable is small: most of its code is in the `libggml*` shared
/// libraries beside it, which it loads from its own directory (Linux builds
/// also load backend plugins found there). Those libraries are covered by the
/// install-time archive verification plus the ownership and write-permission
/// checks of [`locate_pinned_ggml_rpc_server`], not re-hashed on each spawn.
pub const PINNED_GGML_RPC_SERVER_SHA256: &[(&str, &str)] = &[
    (
        "macos-arm64-metal",
        "cc5945ecb7744b0e843acd44115d12729427d4b494820b79b519ebd3114ef8ce",
    ),
    (
        "linux-x86_64-cuda",
        "7f8f4a788f8c2fee8d2ead139d1a0d97e2336051dff0115f2847229da1dbb156",
    ),
    (
        "linux-x86_64-vulkan",
        "a8a238a73da848562e08256364fe983b143e343beab89585981af2d56c8c9dde",
    ),
    (
        "linux-x86_64-cpu",
        "a8a238a73da848562e08256364fe983b143e343beab89585981af2d56c8c9dde",
    ),
    (
        "linux-aarch64-cpu",
        "c7b2129989bce69a5f55589a86715bbedcc65b134cbf100535708e90ce159fc9",
    ),
];

/// Line the pinned `ggml-rpc-server` writes to stderr from its own accept
/// loop (`ggml_backend_rpc_start_server`). Readiness requires it after the
/// probe connection: a listener that won the close-to-bind race for the
/// reserved port could answer the probe, but cannot make our child log it.
const GGML_RPC_SERVER_ACCEPT_LINE: &str = "Accepted client connection";
/// How long the child has to log its accept of the readiness probe.
const MANAGED_RPC_ACCEPT_CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a managed server must stay up after it accepted the probe. When
/// its port was taken, `ggml-rpc-server` prints
/// `Failed to create server socket` and exits (status 0) after device init.
const MANAGED_RPC_SERVER_SETTLE: Duration = Duration::from_millis(300);
const MANAGED_RPC_SERVER_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const MANAGED_RPC_SERVER_PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// Bound on waiting for the guard to exit once its lifeline is closed.
const MANAGED_RPC_REAP_TIMEOUT: Duration = Duration::from_secs(5);
const MANAGED_RPC_MAX_LOG_LINE_BYTES: usize = 4 * 1024;

/// First stderr line of the lifeline guard: `<prefix> <server pid>`, so a
/// last-resort kill can reach the server even if the guard is wedged.
const GUARD_PID_LINE_PREFIX: &str = "lucidd-rpc-guard: server pid";

/// `/bin/sh` guard that runs `ggml-rpc-server` (`$1`, arguments after it)
/// as its own child and ties it to lucidd's lifetime without `unsafe`: its
/// stdin is a pipe whose only writer is lucidd. A watcher kills the server
/// once that pipe reaches EOF, which happens however lucidd ends, SIGKILL
/// included, and also when lucidd closes it to stop the server. The guard
/// itself exits with the server's status once the server is gone, so lucidd
/// still observes an unexpected exit. ggml-rpc has no authentication, so the
/// server must never outlive the libp2p handler in front of it.
const RPC_SERVER_LIFELINE_GUARD: &str = r#"exec 3<&0
server=$1
shift
"$server" "$@" </dev/null 3<&- &
child=$!
printf '%s %s\n' "$GUARD_PID_PREFIX" "$child" >&2
{
    while read -r _; do :; done
    kill -KILL "$child" 2>/dev/null
} <&3 &
watcher=$!
exec 3<&-
wait "$child"
status=$?
kill "$watcher" 2>/dev/null
exit "$status"
"#;
/// Start or run failures tolerated within [`MANAGED_RPC_FAILURE_WINDOW`]
/// before the manager stops retrying until `cluster_enabled` is turned off
/// and on again.
const MANAGED_RPC_MAX_FAILURES: usize = 3;
const MANAGED_RPC_FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MANAGED_RPC_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// A `ggml-rpc-server` executable verified to be a pinned build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRpcServerBinary {
    path: PathBuf,
    sha256: String,
    /// Account trusted (with root) to own the binary, its libraries, and
    /// every directory above them.
    trusted_owner: u32,
}

impl PinnedRpcServerBinary {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Repeat the locate-time checks and require the very bytes located
    /// then, so a binary replaced while lucidd runs is never spawned.
    fn reverify(&self) -> Result<()> {
        verify_rpc_server_binary(&self.path, &[self.sha256.as_str()], self.trusted_owner)
            .map(|_| ())
    }
}

/// Find the pinned `b12651` `ggml-rpc-server` beside the configured
/// `llama-server` (no `PATH` lookup) and verify it:
///
/// - a regular, executable, non-symlink file whose SHA-256 is one of
///   [`PINNED_GGML_RPC_SERVER_SHA256`];
/// - it, its directory, and the `libggml*` libraries it loads from that
///   directory are owned by root or `trusted_owner` (the account that owns
///   this node's identity) and writable by neither group nor others, and
///   every library symlink stays inside the directory;
/// - every directory above is owned by root or `trusted_owner` and, unless
///   sticky, writable by neither group nor others, so nobody else can swap
///   the runtime directory out from under it.
///
/// `trusted_owner` is a Unix uid; it is ignored elsewhere.
pub fn locate_pinned_ggml_rpc_server(
    llama_server: &Path,
    trusted_owner: u32,
) -> Result<PinnedRpcServerBinary> {
    let pinned: Vec<&str> = PINNED_GGML_RPC_SERVER_SHA256
        .iter()
        .map(|(_, sha256)| *sha256)
        .collect();
    locate_rpc_server_beside(llama_server, &pinned, trusted_owner)
}

fn locate_rpc_server_beside(
    llama_server: &Path,
    pinned: &[&str],
    trusted_owner: u32,
) -> Result<PinnedRpcServerBinary> {
    if !llama_server.is_absolute() {
        bail!("llama-server path must be absolute to locate ggml-rpc-server beside it");
    }
    let llama_server = llama_server
        .canonicalize()
        .with_context(|| format!("cannot resolve llama-server {}", llama_server.display()))?;
    let directory = llama_server
        .parent()
        .context("llama-server path has no parent directory")?;
    let path = directory.join(GGML_RPC_SERVER_BINARY_NAME);
    let sha256 = verify_rpc_server_binary(&path, pinned, trusted_owner)?;
    Ok(PinnedRpcServerBinary {
        path,
        sha256,
        trusted_owner,
    })
}

fn verify_rpc_server_binary(path: &Path, pinned: &[&str], trusted_owner: u32) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular file, not a symlink", path.display());
    }
    let directory = path
        .parent()
        .context("ggml-rpc-server path has no parent directory")?;
    let directory_metadata = std::fs::symlink_metadata(directory)
        .with_context(|| format!("cannot inspect {}", directory.display()))?;
    if !directory_metadata.is_dir() {
        bail!("{} must be a real directory", directory.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("{} is not executable", path.display());
        }
        require_owner_only_writable(path, &metadata, trusted_owner, false)?;
        require_owner_only_writable(directory, &directory_metadata, trusted_owner, false)
            .context("ggml-rpc-server loads its libraries from its own directory")?;
        for ancestor in directory.ancestors().skip(1) {
            let ancestor_metadata = std::fs::symlink_metadata(ancestor)
                .with_context(|| format!("cannot inspect {}", ancestor.display()))?;
            require_owner_only_writable(ancestor, &ancestor_metadata, trusted_owner, true)?;
        }
        verify_rpc_server_libraries(directory, trusted_owner)?;
    }
    #[cfg(not(unix))]
    let _ = trusted_owner;
    let actual = crate::runtime_manager::sha256_file_hex(path)?;
    if !pinned
        .iter()
        .any(|expected| expected.eq_ignore_ascii_case(&actual))
    {
        bail!(
            "{} is not the pinned {} ggml-rpc-server (SHA-256 {actual})",
            path.display(),
            crate::runtime_manager::PINNED_LLAMA_CPP_TAG
        );
    }
    Ok(actual)
}

/// `path` is owned by root or `trusted_owner` and neither group nor others
/// can write it; a sticky directory (others cannot rename entries they do
/// not own) is accepted when `allow_sticky`.
#[cfg(unix)]
fn require_owner_only_writable(
    path: &Path,
    metadata: &std::fs::Metadata,
    trusted_owner: u32,
    allow_sticky: bool,
) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    const STICKY: u32 = 0o1000;
    let owner = metadata.uid();
    if owner != 0 && owner != trusted_owner {
        bail!(
            "{} is owned by uid {owner}, not root or this node's owner",
            path.display()
        );
    }
    let mode = metadata.permissions().mode();
    if mode & 0o022 != 0 && !(allow_sticky && metadata.is_dir() && mode & STICKY != 0) {
        bail!("{} must not be writable by group or others", path.display());
    }
    Ok(())
}

/// Every `libggml*` entry beside the binary is a library it may load: each
/// must be a trusted, owner-only-writable regular file, or a symlink that
/// resolves to one inside the same directory.
#[cfg(unix)]
fn verify_rpc_server_libraries(directory: &Path, trusted_owner: u32) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("cannot list {}", directory.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot list {}", directory.display()))?;
        if !entry.file_name().to_string_lossy().starts_with("libggml") {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("cannot inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            let target = path
                .canonicalize()
                .with_context(|| format!("cannot resolve {}", path.display()))?;
            if target.parent() != Some(directory) {
                bail!(
                    "{} must resolve inside {}",
                    path.display(),
                    directory.display()
                );
            }
            let target_metadata = std::fs::metadata(&target)
                .with_context(|| format!("cannot inspect {}", target.display()))?;
            if !target_metadata.is_file() {
                bail!("{} must resolve to a regular file", path.display());
            }
            require_owner_only_writable(&target, &target_metadata, trusted_owner, false)?;
        } else if metadata.is_file() {
            require_owner_only_writable(&path, &metadata, trusted_owner, false)?;
        } else {
            bail!("{} must be a regular file or symlink", path.display());
        }
    }
    Ok(())
}

/// Default private cache directory handed to the managed server's `-c`
/// tensor cache (`LLAMA_CACHE`), kept apart from any other llama.cpp cache.
pub fn default_managed_rpc_cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("could not resolve the platform cache directory")?;
    Ok(base.join("lucidd").join("ggml-rpc-cache"))
}

/// How [`ManagedShardRpcProvider`] runs its `ggml-rpc-server`.
#[derive(Debug, Clone)]
pub struct ManagedRpcServerConfig {
    pub binary: PinnedRpcServerBinary,
    /// Directory for the `-c` tensor cache, passed as `LLAMA_CACHE`. Created
    /// `0700` before every spawn. With `b12651` the coordinator's client sets
    /// the cache flag only for tensors over 10 MiB in a weights buffer
    /// (`rpc_use_hash_cache`), so the cache holds uploaded weights, not
    /// activations. It is shared by every enrolled coordinator and not
    /// size-bounded by `ggml-rpc-server`.
    pub cache_dir: PathBuf,
    /// Extra child environment, e.g. the CUDA library path `llama-server`
    /// also gets. Cannot override `LLAMA_CACHE`.
    pub extra_env: Vec<(String, String)>,
    /// Deadline for a spawned server to accept connections.
    pub ready_timeout: Duration,
    /// How often the live `cluster_enabled` opt-in is re-read.
    pub gate_poll_interval: Duration,
    /// First restart delay after a failure; doubles per failure in the window.
    pub restart_backoff: Duration,
}

impl ManagedRpcServerConfig {
    pub fn new(binary: PinnedRpcServerBinary, cache_dir: PathBuf) -> Self {
        Self {
            binary,
            cache_dir,
            extra_env: Vec::new(),
            ready_timeout: Duration::from_secs(30),
            gate_poll_interval: SHARD_RPC_REAUTHORIZE_INTERVAL,
            restart_backoff: Duration::from_secs(1),
        }
    }
}

/// Zero-CLI Tier-1 Trusted-Circle provider. While the live `cluster_enabled`
/// opt-in is on, it keeps one pinned `ggml-rpc-server -c` running on an
/// ephemeral `127.0.0.1` port and bridges enrolled peers'
/// `/phase/shard-rpc/1.0.0` tunnels to it through
/// [`make_opt_in_inbound_shard_rpc_handler`], so every tunnel passes exactly
/// the same admission (explicit enrollment, `cluster_enabled`, manual pause)
/// as with an operator-run bridge. When `cluster_enabled` turns off, the
/// server exits on its own, or the provider shuts down, it withdraws the
/// handler first (dropping every live tunnel) and then kills and reaps the
/// server. The server runs under a lifeline guard, so it also dies when
/// lucidd dies without cleaning up (SIGKILL, crash). It owns the node's
/// shard-rpc handler slot, so it is only for nodes without a manual
/// `--shard-rpc-bridge-port`.
pub struct ManagedShardRpcProvider {
    stop: tokio::sync::watch::Sender<bool>,
    serving: tokio::sync::watch::Receiver<Option<u16>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ManagedShardRpcProvider {
    /// Start supervising; nothing is spawned until `cluster_enabled` is on.
    pub fn start(
        discovery: Arc<Discovery>,
        policy: Arc<PolicyEngine>,
        node_config: Arc<NodeConfigStore>,
        config: ManagedRpcServerConfig,
    ) -> Self {
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        let (serving_tx, serving) = tokio::sync::watch::channel(None);
        let manager = RpcServerManager {
            discovery,
            policy,
            node_config,
            config,
            stop: stop_rx,
            serving: serving_tx,
            failures: Vec::new(),
        };
        Self {
            stop,
            serving,
            task: Some(tokio::spawn(manager.run())),
        }
    }

    /// Loopback port of the server currently bridged to peers. `None` while
    /// no server runs; it turns `None` only after the server is reaped.
    pub fn serving_port(&self) -> Option<u16> {
        *self.serving.borrow()
    }

    /// Withdraw the shard-rpc handler, kill and reap the server, and wait
    /// (bounded) for the manager to finish.
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(MANAGED_RPC_SHUTDOWN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                tracing::warn!(
                    "auto-managed ggml-rpc-server manager did not stop in time; aborting it"
                );
                // The aborted task drops the child, and `kill_on_drop`
                // still terminates it.
                task.abort();
            }
        }
    }
}

impl Drop for ManagedShardRpcProvider {
    fn drop(&mut self) {
        // The manager notices and cleans up on its own while the runtime
        // lives. If it is torn down instead, dropping the server closes its
        // lifeline, and the guard kills it.
        self.stop.send_replace(true);
    }
}

/// One running managed server under its lifeline guard. Owned by the manager
/// outside any future it races, so a cancelled wait can still kill **and
/// reap** it.
struct ManagedRpcServer {
    /// The `/bin/sh` lifeline guard; `ggml-rpc-server` is its child.
    child: Child,
    /// Write end of the guard's lifeline. Closing it (or lucidd ending in
    /// any way) makes the guard kill the server.
    lifeline: Option<tokio::process::ChildStdin>,
    port: u16,
    /// Number of `Accepted client connection` lines the server has logged.
    accepted: tokio::sync::watch::Receiver<u64>,
    /// The server's pid, as its guard reported it.
    server_pid: tokio::sync::watch::Receiver<Option<u32>>,
    log_drain: tokio::task::JoinHandle<()>,
}

impl Drop for ManagedRpcServer {
    fn drop(&mut self) {
        // Dropping also closes the lifeline, so the guard kills the server
        // even if `kill_on_drop` takes the guard down first.
        self.log_drain.abort();
    }
}

enum StartOutcome {
    Ready(Box<ManagedRpcServer>),
    Stopped,
    ClusterOff,
    Failed(anyhow::Error),
}

enum RunEnd {
    Stopped,
    ClusterOff,
    Exited(std::io::Result<std::process::ExitStatus>),
}

struct RpcServerManager {
    discovery: Arc<Discovery>,
    policy: Arc<PolicyEngine>,
    node_config: Arc<NodeConfigStore>,
    config: ManagedRpcServerConfig,
    stop: tokio::sync::watch::Receiver<bool>,
    serving: tokio::sync::watch::Sender<Option<u16>>,
    /// Recent start/run failures, for the restart budget.
    failures: Vec<std::time::Instant>,
}

impl RpcServerManager {
    async fn run(mut self) {
        loop {
            // Idle until the operator opts in.
            if !self.wait_for_cluster(true).await {
                return;
            }
            let mut server = match self.start().await {
                StartOutcome::Ready(server) => *server,
                StartOutcome::Stopped => return,
                StartOutcome::ClusterOff => continue,
                StartOutcome::Failed(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "auto-managed ggml-rpc-server failed to start");
                    if !self.back_off_after_failure().await {
                        return;
                    }
                    continue;
                }
            };
            let port = server.port;
            let installed = make_opt_in_inbound_shard_rpc_handler(
                self.policy.clone(),
                self.node_config.clone(),
                port,
            )
            .and_then(|handler| self.discovery.set_shard_rpc_handler(Some(handler)));
            if let Err(error) = installed {
                tracing::error!(error = %format!("{error:#}"), "cannot register the shard rpc bridge; stopping the auto-managed ggml-rpc-server");
                reap_managed_rpc_server(&mut server).await;
                return;
            }
            self.serving.send_replace(Some(port));
            tracing::info!(
                loopback_port = port,
                "auto-managed ggml-rpc-server is bridged to enrolled peers over /phase/shard-rpc/1.0.0"
            );

            let end = tokio::select! {
                biased;
                _ = self.stop.wait_for(|stop| *stop) => RunEnd::Stopped,
                () = cluster_enabled_becomes(&self.node_config, false, self.config.gate_poll_interval) => RunEnd::ClusterOff,
                status = server.child.wait() => RunEnd::Exited(status),
            };
            // Withdraw first: no new tunnel can reach the dying server, and
            // every live session the handler admitted drops now.
            if let Err(error) = self.discovery.set_shard_rpc_handler(None) {
                tracing::error!(error = %format!("{error:#}"), "cannot withdraw the shard rpc bridge");
            }
            reap_managed_rpc_server(&mut server).await;
            self.serving.send_replace(None);
            match end {
                RunEnd::Stopped => {
                    tracing::info!("stopped the auto-managed ggml-rpc-server");
                    return;
                }
                RunEnd::ClusterOff => {
                    tracing::info!("cluster_enabled turned off: withdrew the shard rpc bridge and stopped ggml-rpc-server");
                    self.failures.clear();
                }
                RunEnd::Exited(status) => {
                    tracing::warn!(?status, "auto-managed ggml-rpc-server exited unexpectedly; withdrew the shard rpc bridge");
                    if !self.back_off_after_failure().await {
                        return;
                    }
                }
            }
        }
    }

    /// Verify, spawn, and wait for readiness, abandoning (with kill and
    /// reap) as soon as the opt-in is withdrawn or the provider stops.
    async fn start(&mut self) -> StartOutcome {
        let mut server = match spawn_managed_rpc_server(&self.config).await {
            Ok(server) => server,
            Err(error) => return StartOutcome::Failed(error),
        };
        let outcome = tokio::select! {
            biased;
            _ = self.stop.wait_for(|stop| *stop) => StartOutcome::Stopped,
            () = cluster_enabled_becomes(&self.node_config, false, self.config.gate_poll_interval) => StartOutcome::ClusterOff,
            ready = wait_managed_rpc_server_ready(&mut server, self.config.ready_timeout) => match ready {
                Ok(()) => return StartOutcome::Ready(Box::new(server)),
                Err(error) => StartOutcome::Failed(error),
            },
        };
        reap_managed_rpc_server(&mut server).await;
        outcome
    }

    /// Record a failure and wait out the restart backoff. Once the failure
    /// budget is spent, stay stopped until the operator turns
    /// `cluster_enabled` off (and later on) again. Returns `false` when the
    /// provider is stopping.
    async fn back_off_after_failure(&mut self) -> bool {
        let now = std::time::Instant::now();
        self.failures
            .retain(|at| now.duration_since(*at) < MANAGED_RPC_FAILURE_WINDOW);
        self.failures.push(now);
        if self.failures.len() >= MANAGED_RPC_MAX_FAILURES {
            tracing::error!(
                failures = self.failures.len(),
                "auto-managed ggml-rpc-server keeps failing; leaving it stopped until cluster_enabled is turned off and on again"
            );
            let resumed = self.wait_for_cluster(false).await;
            self.failures.clear();
            return resumed;
        }
        let exponent = u32::try_from(self.failures.len() - 1).unwrap_or(u32::MAX);
        let delay = self
            .config
            .restart_backoff
            .saturating_mul(2_u32.saturating_pow(exponent));
        tokio::select! {
            biased;
            _ = self.stop.wait_for(|stop| *stop) => false,
            // Withdrawn meanwhile: the outer loop idles until it is back.
            () = cluster_enabled_becomes(&self.node_config, false, self.config.gate_poll_interval) => true,
            () = tokio::time::sleep(delay) => true,
        }
    }

    /// Wait until `cluster_enabled == enabled`; `false` if stopping first.
    async fn wait_for_cluster(&mut self, enabled: bool) -> bool {
        tokio::select! {
            biased;
            _ = self.stop.wait_for(|stop| *stop) => false,
            () = cluster_enabled_becomes(&self.node_config, enabled, self.config.gate_poll_interval) => true,
        }
    }
}

async fn cluster_enabled_becomes(node_config: &NodeConfigStore, enabled: bool, every: Duration) {
    let mut checks = tokio::time::interval(every);
    checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        checks.tick().await;
        if node_config.snapshot().cluster_enabled == enabled {
            return;
        }
    }
}

/// Re-verify the pinned binary, then spawn it on a fresh ephemeral loopback
/// port inside a cleared environment, under the lifeline guard.
async fn spawn_managed_rpc_server(config: &ManagedRpcServerConfig) -> Result<ManagedRpcServer> {
    let binary = config.binary.clone();
    let cache_dir = config.cache_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        binary.reverify()?;
        crate::node_config::ensure_private_dir(&cache_dir)
    })
    .await
    .context("ggml-rpc-server verification task failed")??;

    // ggml-rpc-server cannot inherit a listener, so this only proves the port
    // was free a moment ago; losing the close-to-bind race makes the server
    // exit, which readiness and supervision catch.
    let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .context("reserve a loopback port for ggml-rpc-server")?
        .port();
    let directory = config
        .binary
        .path()
        .parent()
        .context("ggml-rpc-server path has no parent directory")?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(RPC_SERVER_LIFELINE_GUARD)
        .arg("ggml-rpc-guard")
        .arg(config.binary.path());
    // Nothing from lucidd's environment reaches the guard or the server.
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin");
    for (key, value) in &config.extra_env {
        cmd.env(key, value);
    }
    cmd.env("LLAMA_CACHE", &config.cache_dir);
    cmd.env("GUARD_PID_PREFIX", GUARD_PID_LINE_PREFIX);
    // ggml also searches the working directory for backend plugins; keep it
    // the verified binary directory rather than wherever lucidd started.
    cmd.current_dir(directory);
    cmd.arg("-H").arg("127.0.0.1");
    cmd.arg("-p").arg(port.to_string());
    cmd.arg("-c");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn the /bin/sh lifeline guard for {}",
            config.binary.path().display()
        )
    })?;
    let lifeline = child.stdin.take();
    let (accepted_tx, accepted) = tokio::sync::watch::channel(0_u64);
    let (server_pid_tx, server_pid) = tokio::sync::watch::channel(None);
    let log_drain = match child.stderr.take() {
        Some(stderr) => tokio::spawn(drain_managed_rpc_server_log(
            stderr,
            accepted_tx,
            server_pid_tx,
        )),
        None => tokio::spawn(async {}),
    };
    Ok(ManagedRpcServer {
        child,
        lifeline,
        port,
        accepted,
        server_pid,
        log_drain,
    })
}

/// Drain the guard's and server's stderr in bounded lines (a full pipe would
/// stall the server), recording the server pid the guard reports and
/// counting the accept lines readiness relies on.
async fn drain_managed_rpc_server_log(
    mut stderr: tokio::process::ChildStderr,
    accepted: tokio::sync::watch::Sender<u64>,
    server_pid: tokio::sync::watch::Sender<Option<u32>>,
) {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0_u8; 1024];
    let mut line: Vec<u8> = Vec::new();
    loop {
        let read = match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        for &byte in &buffer[..read] {
            if byte != b'\n' {
                if line.len() < MANAGED_RPC_MAX_LOG_LINE_BYTES {
                    line.push(byte);
                }
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            if text.trim() == GGML_RPC_SERVER_ACCEPT_LINE {
                accepted.send_modify(|count| *count = count.wrapping_add(1));
            } else if server_pid.borrow().is_none() {
                if let Some(pid) = text
                    .strip_prefix(GUARD_PID_LINE_PREFIX)
                    .and_then(|pid| pid.trim().parse::<u32>().ok())
                {
                    server_pid.send_replace(Some(pid));
                }
            }
            tracing::trace!(target: "lucidd::ggml_rpc_server", line = %text.escape_debug(), "ggml-rpc-server output");
            line.clear();
        }
    }
}

async fn wait_managed_rpc_server_ready(
    server: &mut ManagedRpcServer,
    deadline: Duration,
) -> Result<()> {
    let started = tokio::time::Instant::now();
    loop {
        if let Some(status) = server.child.try_wait()? {
            bail!("ggml-rpc-server exited during startup ({status})");
        }
        if started.elapsed() >= deadline {
            bail!("ggml-rpc-server did not accept connections within {deadline:?}");
        }
        let accepted_before = *server.accepted.borrow();
        let probe = tokio::time::timeout(
            MANAGED_RPC_SERVER_PROBE_TIMEOUT,
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, server.port)),
        )
        .await;
        if let Ok(Ok(probe)) = probe {
            drop(probe);
            // Only our child logs its own accepts; anything else answering on
            // the port (it won the close-to-bind race) never produces one.
            let accepted = &mut server.accepted;
            let confirmed = tokio::time::timeout(MANAGED_RPC_ACCEPT_CONFIRM_TIMEOUT, async move {
                accepted
                    .wait_for(|accepted| *accepted > accepted_before)
                    .await
                    .is_ok()
            })
            .await;
            if confirmed != Ok(true) {
                bail!(
                    "127.0.0.1:{} answered, but ggml-rpc-server did not accept that connection: another process holds the port",
                    server.port
                );
            }
            tokio::time::sleep(MANAGED_RPC_SERVER_SETTLE).await;
            if let Some(status) = server.child.try_wait()? {
                bail!("ggml-rpc-server exited right after starting ({status})");
            }
            return Ok(());
        }
        tokio::time::sleep(MANAGED_RPC_SERVER_PROBE_INTERVAL).await;
    }
}

/// Stop the server and wait for it, so its port and GPU context are released
/// before this returns: closing the lifeline makes the guard SIGKILL the
/// server, reap it, and exit. Killing both directly is only the fallback.
async fn reap_managed_rpc_server(server: &mut ManagedRpcServer) {
    drop(server.lifeline.take());
    if tokio::time::timeout(MANAGED_RPC_REAP_TIMEOUT, server.child.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            port = server.port,
            "ggml-rpc-server guard did not exit after its lifeline closed; killing the server and its guard"
        );
        // The guard is still alive, so it has not reaped the server and the
        // pid cannot have been reused: kill the server first, then the guard.
        let server_pid = *server.server_pid.borrow();
        if let Some(pid) = server_pid {
            let _ = Command::new("/bin/kill")
                .env_clear()
                .arg("-KILL")
                .arg(pid.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if let Err(error) = server.child.kill().await {
            tracing::warn!(%error, port = server.port, "failed to kill the ggml-rpc-server guard");
        }
    }
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

    // -----------------------------------------------------------------
    // Zero-CLI auto-managed ggml-rpc-server
    // -----------------------------------------------------------------

    /// The managed provider driven against a stand-in `ggml-rpc-server`
    /// re-entered from this test executable through a recording launcher,
    /// so the real spawn, readiness, supervision, and reaping paths run.
    mod managed_rpc_server {
        use super::*;
        use std::io::{Read, Write};

        const ENTRYPOINT: &str =
            "shard_worker::tests::managed_rpc_server::fake_ggml_rpc_server_entrypoint";

        /// Internal subprocess entrypoint: a loopback echo server on the
        /// launcher's `-p` port. Like the pinned `ggml-rpc-server`, it logs
        /// each accept to stderr and exits with status 0 when its port is
        /// taken. Ignored in ordinary runs.
        #[test]
        #[ignore = "internal ggml-rpc-server fixture subprocess entrypoint"]
        fn fake_ggml_rpc_server_entrypoint() {
            if std::env::var_os("PHASE_FAKE_GGML_RPC_PROCESS").is_none() {
                eprintln!("internal fixture entrypoint unavailable outside its launcher");
                return;
            }
            let port: u16 = std::env::var("PHASE_FAKE_GGML_RPC_PORT")
                .expect("launcher sets the port")
                .parse()
                .expect("port is a u16");
            if let Some(ms) = std::env::var("PHASE_FAKE_GGML_RPC_EXIT_AFTER_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
            {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    std::process::exit(3);
                });
            }
            if let Some(ms) = std::env::var("PHASE_FAKE_GGML_RPC_BIND_DELAY_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
            {
                std::thread::sleep(Duration::from_millis(ms));
            }
            // Silent: answers connections like any listener, but never logs
            // the accept (what readiness sees when another process holds
            // the port).
            let silent = std::env::var_os("PHASE_FAKE_GGML_RPC_SILENT").is_some();
            let Ok(listener) = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            else {
                eprintln!("Failed to create server socket");
                std::process::exit(0);
            };
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                if !silent {
                    eprintln!("{GGML_RPC_SERVER_ACCEPT_LINE}");
                }
                std::thread::spawn(move || {
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        match stream.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                if stream.write_all(&buffer[..read]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        }

        fn shell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\\''"))
        }

        #[cfg(unix)]
        fn set_mode(path: &Path, mode: u32) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        struct Fixture {
            dir: tempfile::TempDir,
            llama_server: PathBuf,
            launcher: PathBuf,
        }

        /// How the stand-in server misbehaves, if at all.
        #[derive(Default)]
        struct Behaviour {
            /// Exit on its own this long after starting.
            exit_after_ms: Option<u64>,
            /// Wait this long before binding its port.
            bind_delay_ms: Option<u64>,
            /// Never log accepts.
            silent: bool,
        }

        impl Fixture {
            fn new(exit_after_ms: Option<u64>) -> Self {
                Self::with(Behaviour {
                    exit_after_ms,
                    ..Behaviour::default()
                })
            }

            /// A fake runtime directory: a `llama-server` file and, beside it,
            /// a `ggml-rpc-server` launcher that records each spawn's argv,
            /// environment, working directory, and PID before exec'ing the
            /// stand-in with `behaviour`.
            fn with(behaviour: Behaviour) -> Self {
                let dir = tempfile::tempdir().unwrap();
                let llama_server = dir.path().join("llama-server");
                std::fs::write(&llama_server, b"#!/bin/sh\nexit 0\n").unwrap();
                let records = dir.path().join("fixture");
                let test_exe = std::env::current_exe().unwrap();
                let script = format!(
                    r#"#!/bin/sh
records={records}
for arg in "$@"; do
    printf '%s\n' "$arg"
done > "$records.argv.$$"
/usr/bin/env > "$records.env.$$"
pwd -P > "$records.cwd.$$"
port=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -p)
            [ "$#" -ge 2 ] || exit 64
            port="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
[ -n "$port" ] || exit 64
for kind in argv env cwd; do
    mv "$records.$kind.$$" "$records.$port.$kind"
done
printf '%s\n' "$$" >> "$records.pids"
export PHASE_FAKE_GGML_RPC_PROCESS=1
export PHASE_FAKE_GGML_RPC_PORT="$port"
export PHASE_FAKE_GGML_RPC_EXIT_AFTER_MS={exit_after}
export PHASE_FAKE_GGML_RPC_BIND_DELAY_MS={bind_delay}
{silent}exec {exe} --exact {entrypoint} --ignored --nocapture
"#,
                    records = shell_quote(records.to_str().unwrap()),
                    exit_after = shell_quote(
                        &behaviour
                            .exit_after_ms
                            .map(|ms| ms.to_string())
                            .unwrap_or_default()
                    ),
                    bind_delay = shell_quote(
                        &behaviour
                            .bind_delay_ms
                            .map(|ms| ms.to_string())
                            .unwrap_or_default()
                    ),
                    silent = if behaviour.silent {
                        "export PHASE_FAKE_GGML_RPC_SILENT=1\n"
                    } else {
                        ""
                    },
                    exe = shell_quote(test_exe.to_str().unwrap()),
                    entrypoint = ENTRYPOINT,
                );
                let launcher = dir.path().join(GGML_RPC_SERVER_BINARY_NAME);
                std::fs::write(&launcher, script).unwrap();
                #[cfg(unix)]
                {
                    set_mode(&llama_server, 0o755);
                    set_mode(&launcher, 0o755);
                }
                Self {
                    dir,
                    llama_server,
                    launcher,
                }
            }

            /// The launcher, pinned to its own digest (production pins only
            /// accept the real `b12651` bytes).
            fn pinned(&self) -> PinnedRpcServerBinary {
                let sha256 = crate::runtime_manager::sha256_file_hex(&self.launcher).unwrap();
                locate_rpc_server_beside(&self.llama_server, &[sha256.as_str()], self.owner())
                    .unwrap()
            }

            /// The test's own account, as owner of everything it created.
            fn owner(&self) -> u32 {
                owner_of(self.dir.path())
            }

            fn cache_dir(&self) -> PathBuf {
                self.dir.path().join("rpc-cache")
            }

            fn config(&self) -> ManagedRpcServerConfig {
                ManagedRpcServerConfig {
                    ready_timeout: Duration::from_secs(15),
                    gate_poll_interval: Duration::from_millis(50),
                    restart_backoff: Duration::from_millis(50),
                    ..ManagedRpcServerConfig::new(self.pinned(), self.cache_dir())
                }
            }

            fn pids(&self) -> Vec<u32> {
                std::fs::read_to_string(self.dir.path().join("fixture.pids"))
                    .unwrap_or_default()
                    .lines()
                    .map(|line| line.parse().unwrap())
                    .collect()
            }

            fn record(&self, port: u16, kind: &str) -> Vec<String> {
                std::fs::read_to_string(self.dir.path().join(format!("fixture.{port}.{kind}")))
                    .unwrap_or_else(|_| panic!("spawn on {port} recorded its {kind}"))
                    .lines()
                    .map(str::to_string)
                    .collect()
            }
        }

        #[cfg(unix)]
        fn owner_of(path: &Path) -> u32 {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).unwrap().uid()
        }

        #[cfg(not(unix))]
        fn owner_of(_: &Path) -> u32 {
            0
        }

        /// `true` while `pid` still exists, zombie included: a reaped child
        /// is gone from the process table.
        fn pid_exists(pid: u32) -> bool {
            std::process::Command::new("/bin/kill")
                .arg("-0")
                .arg(pid.to_string())
                .stderr(Stdio::null())
                .status()
                .expect("probe pid")
                .success()
        }

        async fn wait_serving(
            provider: &ManagedShardRpcProvider,
            within: Duration,
            predicate: impl Fn(Option<u16>) -> bool,
        ) -> Option<u16> {
            let started = tokio::time::Instant::now();
            loop {
                let port = provider.serving_port();
                if predicate(port) {
                    return port;
                }
                assert!(
                    started.elapsed() < within,
                    "serving port never reached the expected state (now {port:?})"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// The provider's shard-rpc handler slot is empty: `phase-net` drops
        /// the stream before any admission decision. A handler still
        /// installed would answer with a rejection instead (cluster off, or
        /// its dead backend unavailable).
        async fn assert_no_bridge(coordinator: &Discovery, provider: PeerId, session: u8) {
            let refused = coordinator
                .open_shard_rpc_tunnel(provider, rpc_open(session))
                .await
                .expect_err("no bridge, no tunnel");
            assert!(
                !format!("{refused:#}").contains("rejected"),
                "the bridge must be withdrawn, not merely refusing: {refused:#}"
            );
        }

        async fn wait_pids(fixture: &Fixture, count: usize, within: Duration) {
            let started = tokio::time::Instant::now();
            while fixture.pids().len() < count {
                assert!(
                    started.elapsed() < within,
                    "expected {count} spawns, saw {:?}",
                    fixture.pids()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        async fn echo_round_trip(
            coordinator: &Discovery,
            provider: PeerId,
            session_byte: u8,
        ) -> (
            tokio::io::DuplexStream,
            tokio::task::JoinHandle<phase_net::ShardRpcSessionReport>,
        ) {
            let tunnel = coordinator
                .open_shard_rpc_tunnel(provider, rpc_open(session_byte))
                .await
                .expect("enrolled coordinator is admitted");
            let (splice_io, mut local) = tokio::io::duplex(64 * 1024);
            let session = tokio::spawn(tunnel.splice(splice_io, None));
            let payload = pattern(256 * 1024, u32::from(session_byte));
            local.write_all(&payload).await.unwrap();
            let mut echoed = vec![0_u8; payload.len()];
            tokio::time::timeout(Duration::from_secs(20), local.read_exact(&mut echoed))
                .await
                .expect("echo through the managed server completes")
                .unwrap();
            assert!(echoed == payload, "bytes survive the managed bridge intact");
            (local, session)
        }

        #[cfg(unix)]
        #[test]
        fn locate_requires_a_pinned_owner_only_executable_beside_llama_server() {
            let fixture = Fixture::new(None);
            let digest = crate::runtime_manager::sha256_file_hex(&fixture.launcher).unwrap();
            let pinned = [digest.as_str()];
            let owner = fixture.owner();
            let located = locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap();
            assert_eq!(
                located.path(),
                fixture
                    .dir
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(GGML_RPC_SERVER_BINARY_NAME)
            );
            assert_eq!(located.sha256(), digest);

            // Production pins accept only the real b12651 bytes.
            let unpinned = locate_pinned_ggml_rpc_server(&fixture.llama_server, owner).unwrap_err();
            assert!(
                format!("{unpinned:#}").contains("not the pinned b12651"),
                "{unpinned:#}"
            );
            // Never resolved through PATH or a relative path.
            assert!(locate_rpc_server_beside(Path::new("llama-server"), &pinned, owner).is_err());

            // Writable by group/others, or in such a directory: refused.
            set_mode(&fixture.launcher, 0o775);
            assert!(locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).is_err());
            set_mode(&fixture.launcher, 0o644);
            let not_executable =
                locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap_err();
            assert!(format!("{not_executable:#}").contains("not executable"));
            set_mode(&fixture.launcher, 0o755);
            set_mode(fixture.dir.path(), 0o777);
            let shared_dir =
                locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap_err();
            assert!(
                format!("{shared_dir:#}").contains("loads its libraries"),
                "{shared_dir:#}"
            );
            set_mode(fixture.dir.path(), 0o700);

            // A symlink in place of the binary is refused, even to pinned bytes.
            let real = fixture.dir.path().join("real-ggml-rpc-server");
            std::fs::rename(&fixture.launcher, &real).unwrap();
            std::os::unix::fs::symlink(&real, &fixture.launcher).unwrap();
            assert!(locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).is_err());
            std::fs::remove_file(&fixture.launcher).unwrap();
            assert!(locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).is_err());
            std::fs::rename(&real, &fixture.launcher).unwrap();

            // Owned by anyone but root or this node's owner: refused. (Run
            // as root, everything here is root-owned and trusted.)
            if owner != 0 {
                let foreign =
                    locate_rpc_server_beside(&fixture.llama_server, &pinned, owner.wrapping_add(1))
                        .unwrap_err();
                assert!(
                    format!("{foreign:#}").contains("not root or this node's owner"),
                    "{foreign:#}"
                );
            }

            // Libraries it loads from its directory must be owner-only
            // writable and must not point outside it.
            let library = fixture.dir.path().join("libggml-base.so");
            std::fs::write(&library, b"library").unwrap();
            set_mode(&library, 0o666);
            let writable_library =
                locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap_err();
            assert!(
                format!("{writable_library:#}").contains("libggml-base.so"),
                "{writable_library:#}"
            );
            set_mode(&library, 0o644);
            locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let elsewhere = outside.path().join("libggml-cpu.so");
            std::fs::write(&elsewhere, b"library").unwrap();
            std::os::unix::fs::symlink(&elsewhere, fixture.dir.path().join("libggml-cpu.so"))
                .unwrap();
            let escaping =
                locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap_err();
            assert!(
                format!("{escaping:#}").contains("must resolve inside"),
                "{escaping:#}"
            );
            std::fs::remove_file(fixture.dir.path().join("libggml-cpu.so")).unwrap();
            // A library symlink to any writable file beside it is refused too.
            let blob = fixture.dir.path().join("blob.so");
            std::fs::write(&blob, b"library").unwrap();
            set_mode(&blob, 0o666);
            std::os::unix::fs::symlink(&blob, fixture.dir.path().join("libggml-cpu.so")).unwrap();
            let writable_target =
                locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap_err();
            assert!(
                format!("{writable_target:#}").contains("blob.so"),
                "{writable_target:#}"
            );
            std::fs::remove_file(fixture.dir.path().join("libggml-cpu.so")).unwrap();
            std::fs::remove_file(&blob).unwrap();

            // A directory above it that others can write (and is not sticky)
            // lets them swap the whole runtime directory: refused.
            let shared = fixture.dir.path().join("shared");
            let runtime = shared.join("runtime");
            std::fs::create_dir_all(&runtime).unwrap();
            for name in ["llama-server", GGML_RPC_SERVER_BINARY_NAME] {
                std::fs::copy(fixture.dir.path().join(name), runtime.join(name)).unwrap();
                set_mode(&runtime.join(name), 0o755);
            }
            set_mode(&shared, 0o777);
            let swappable = locate_rpc_server_beside(&runtime.join("llama-server"), &pinned, owner)
                .unwrap_err();
            assert!(format!("{swappable:#}").contains("shared"), "{swappable:#}");
            set_mode(&shared, 0o1777);
            locate_rpc_server_beside(&runtime.join("llama-server"), &pinned, owner)
                .expect("a sticky ancestor cannot have our entries renamed");
            set_mode(&shared, 0o755);

            // Replacing the located bytes later fails re-verification.
            let located = locate_rpc_server_beside(&fixture.llama_server, &pinned, owner).unwrap();
            located.reverify().unwrap();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&fixture.launcher)
                .unwrap();
            file.write_all(b"# replaced\n").unwrap();
            assert!(located.reverify().is_err());
        }

        #[test]
        fn pinned_rpc_server_digests_cover_every_pinned_llama_asset() {
            for asset in crate::runtime_manager::PINNED_LLAMA_ASSETS {
                assert_eq!(asset.tag, "b12651");
                let pinned = PINNED_GGML_RPC_SERVER_SHA256
                    .iter()
                    .find(|(platform, _)| *platform == asset.platform_id)
                    .unwrap_or_else(|| panic!("no ggml-rpc-server pin for {}", asset.platform_id));
                assert_eq!(pinned.1.len(), 64);
                assert!(pinned
                    .1
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            }
            assert_eq!(
                PINNED_GGML_RPC_SERVER_SHA256.len(),
                crate::runtime_manager::PINNED_LLAMA_ASSETS.len()
            );
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn runs_only_while_cluster_enabled_and_bridges_enrolled_tunnels() {
            let _gate = two_node_gate().await;
            let fixture = Fixture::new(None);
            let provider_identity = NodeIdentity::generate();
            let coordinator_identity = NodeIdentity::generate();
            let provider = discovery_for(&provider_identity);
            let coordinator = discovery_for(&coordinator_identity);
            connect(&coordinator, &provider).await;
            let provider_peer = *provider.local_peer_id();
            let node_config = cluster_config(false);
            let managed = ManagedShardRpcProvider::start(
                provider.clone(),
                policy_enrolling(&[&coordinator_identity]),
                node_config.clone(),
                fixture.config(),
            );

            // Default off: no server, no bridge.
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(managed.serving_port(), None);
            assert!(
                fixture.pids().is_empty(),
                "nothing spawns before the opt-in"
            );
            assert!(
                !fixture.cache_dir().exists(),
                "nothing is even prepared before the opt-in"
            );
            assert_no_bridge(&coordinator, provider_peer, 20).await;

            // Opting in spawns the server on demand and bridges to it.
            set_cluster(&node_config, true);
            let port = wait_serving(&managed, Duration::from_secs(15), |port| port.is_some())
                .await
                .unwrap();
            assert_eq!(
                fixture.record(port, "argv"),
                ["-H", "127.0.0.1", "-p", &port.to_string(), "-c"]
            );
            let env = fixture.record(port, "env");
            assert!(env.contains(&"PATH=/usr/bin:/bin".to_string()), "{env:?}");
            assert!(
                env.contains(&format!("LLAMA_CACHE={}", fixture.cache_dir().display())),
                "{env:?}"
            );
            assert!(
                !env.iter()
                    .any(|entry| entry.starts_with("HOME=") || entry.starts_with("CARGO")),
                "lucidd's environment must not reach the server: {env:?}"
            );
            assert_eq!(
                fixture.record(port, "cwd"),
                [fixture
                    .pinned()
                    .path()
                    .parent()
                    .unwrap()
                    .display()
                    .to_string()]
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(fixture.cache_dir())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700, "tensor cache stays private");
            }
            let (mut local, session) = echo_round_trip(&coordinator, provider_peer, 21).await;

            // Withdrawing the opt-in drops the live tunnel, then kills and
            // reaps the server before the port reads as no longer serving.
            let first_pid = fixture.pids()[0];
            set_cluster(&node_config, false);
            wait_serving(&managed, Duration::from_secs(5), |port| port.is_none()).await;
            assert!(
                !pid_exists(first_pid),
                "ggml-rpc-server {first_pid} must be reaped"
            );
            assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
            let mut rest = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), local.read_to_end(&mut rest))
                .await
                .expect("coordinator observes the tunnel ending")
                .unwrap();
            drop(local);
            let report = tokio::time::timeout(Duration::from_secs(5), session)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(report.end, ShardRpcSessionEnd::RemoteClosed, "{report:?}");
            assert_no_bridge(&coordinator, provider_peer, 22).await;

            // Opting in again starts a fresh server.
            set_cluster(&node_config, true);
            wait_serving(&managed, Duration::from_secs(15), |port| port.is_some()).await;
            assert_eq!(fixture.pids().len(), 2);
            let (local, _session) = echo_round_trip(&coordinator, provider_peer, 23).await;
            drop(local);

            // Shutdown (lucidd exiting) kills and reaps it and withdraws the bridge.
            let second_pid = fixture.pids()[1];
            managed.shutdown().await;
            assert!(
                !pid_exists(second_pid),
                "ggml-rpc-server {second_pid} must be reaped"
            );
            assert_no_bridge(&coordinator, provider_peer, 24).await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_crashed_server_is_unbridged_and_restarted() {
            let _gate = two_node_gate().await;
            let fixture = Fixture::new(Some(3_000));
            let provider = discovery_for(&NodeIdentity::generate());
            let managed = ManagedShardRpcProvider::start(
                provider,
                policy_enrolling(&[]),
                cluster_config(true),
                fixture.config(),
            );
            wait_serving(&managed, Duration::from_secs(15), |port| port.is_some()).await;
            // The server exits on its own; the bridge comes down with it...
            wait_serving(&managed, Duration::from_secs(5), |port| port.is_none()).await;
            assert!(!pid_exists(fixture.pids()[0]));
            // ...and a replacement is started and bridged after the backoff.
            wait_serving(&managed, Duration::from_secs(15), |port| port.is_some()).await;
            assert!(fixture.pids().len() >= 2);
            managed.shutdown().await;
            assert!(fixture.pids().iter().all(|pid| !pid_exists(*pid)));
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn repeated_start_failures_stop_retrying_until_cluster_is_toggled() {
            let _gate = two_node_gate().await;
            // Exits immediately, as if it lost its port every time.
            let fixture = Fixture::new(Some(0));
            let node_config = cluster_config(true);
            let managed = ManagedShardRpcProvider::start(
                discovery_for(&NodeIdentity::generate()),
                policy_enrolling(&[]),
                node_config.clone(),
                fixture.config(),
            );
            let started = tokio::time::Instant::now();
            while fixture.pids().len() < MANAGED_RPC_MAX_FAILURES {
                assert_eq!(
                    managed.serving_port(),
                    None,
                    "a failed start is never bridged"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(15),
                    "{:?}",
                    fixture.pids()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert_eq!(
                fixture.pids().len(),
                MANAGED_RPC_MAX_FAILURES,
                "budget spent: no more retries"
            );
            assert!(fixture.pids().iter().all(|pid| !pid_exists(*pid)));

            // Turning the opt-in off and on again re-arms it.
            set_cluster(&node_config, false);
            tokio::time::sleep(Duration::from_millis(200)).await;
            set_cluster(&node_config, true);
            wait_pids(
                &fixture,
                MANAGED_RPC_MAX_FAILURES + 1,
                Duration::from_secs(10),
            )
            .await;
            managed.shutdown().await;
            assert!(fixture.pids().iter().all(|pid| !pid_exists(*pid)));
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_listener_that_never_logs_our_accept_is_never_bridged() {
            let _gate = two_node_gate().await;
            // Stands in for another process winning the race for the reserved
            // port: the probe connects, but our child never logs accepting it.
            let fixture = Fixture::with(Behaviour {
                silent: true,
                ..Behaviour::default()
            });
            let managed = ManagedShardRpcProvider::start(
                discovery_for(&NodeIdentity::generate()),
                policy_enrolling(&[]),
                cluster_config(true),
                fixture.config(),
            );
            let started = tokio::time::Instant::now();
            while fixture.pids().len() < MANAGED_RPC_MAX_FAILURES {
                assert_eq!(
                    managed.serving_port(),
                    None,
                    "an unconfirmed listener is never bridged"
                );
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "{:?}",
                    fixture.pids()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Each attempt is refused once its accept confirmation times
            // out, then killed and reaped; the spent budget starts no more.
            let started = tokio::time::Instant::now();
            while fixture.pids().iter().any(|pid| pid_exists(*pid)) {
                assert_eq!(managed.serving_port(), None);
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "an unconfirmed server was not reaped"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert_eq!(managed.serving_port(), None);
            assert_eq!(fixture.pids().len(), MANAGED_RPC_MAX_FAILURES);
            managed.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn withdrawing_cluster_during_startup_kills_the_starting_server() {
            let _gate = two_node_gate().await;
            let fixture = Fixture::with(Behaviour {
                bind_delay_ms: Some(10_000),
                ..Behaviour::default()
            });
            let node_config = cluster_config(true);
            let managed = ManagedShardRpcProvider::start(
                discovery_for(&NodeIdentity::generate()),
                policy_enrolling(&[]),
                node_config.clone(),
                fixture.config(),
            );
            wait_pids(&fixture, 1, Duration::from_secs(10)).await;
            let pid = fixture.pids()[0];
            set_cluster(&node_config, false);
            let started = tokio::time::Instant::now();
            while pid_exists(pid) {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "starting server outlived the opt-in"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(managed.serving_port(), None);
            assert_eq!(fixture.pids().len(), 1, "no restart while opted out");
            managed.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn dropping_the_provider_without_shutdown_still_reaps_the_server() {
            let _gate = two_node_gate().await;
            let fixture = Fixture::new(None);
            let managed = ManagedShardRpcProvider::start(
                discovery_for(&NodeIdentity::generate()),
                policy_enrolling(&[]),
                cluster_config(true),
                fixture.config(),
            );
            wait_serving(&managed, Duration::from_secs(15), |port| port.is_some()).await;
            let pid = fixture.pids()[0];
            drop(managed);
            let started = tokio::time::Instant::now();
            while pid_exists(pid) {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "server outlived its provider"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn the_server_dies_with_its_lifeline_even_when_the_guard_is_killed() {
            let _gate = two_node_gate().await;
            let fixture = Fixture::new(None);
            let config = fixture.config();
            let mut server = spawn_managed_rpc_server(&config).await.unwrap();
            wait_managed_rpc_server_ready(&mut server, Duration::from_secs(15))
                .await
                .expect("stand-in becomes ready");
            let pid = fixture.pids()[0];
            assert_eq!(
                *server.server_pid.borrow(),
                Some(pid),
                "the guard reports the server pid the last-resort kill targets"
            );

            // SIGKILL the guard, as `kill_on_drop` or a crash would: the
            // server keeps running only while lucidd's end of the lifeline
            // is open...
            server.child.kill().await.unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(pid_exists(pid), "the guard alone does not hold the server");
            // ...and dies as soon as it closes, which is also what any exit
            // of lucidd itself does, SIGKILL included.
            drop(server.lifeline.take());
            let started = tokio::time::Instant::now();
            while pid_exists(pid) {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "server outlived its lifeline"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        #[tokio::test]
        async fn a_binary_replaced_after_startup_is_never_spawned() {
            let fixture = Fixture::new(None);
            let config = fixture.config();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&fixture.launcher)
                .unwrap();
            file.write_all(b"# replaced\n").unwrap();
            drop(file);
            let managed = ManagedShardRpcProvider::start(
                discovery_for(&NodeIdentity::generate()),
                policy_enrolling(&[]),
                cluster_config(true),
                config,
            );
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            assert_eq!(managed.serving_port(), None);
            assert!(
                fixture.pids().is_empty(),
                "a replaced binary must never run"
            );
            managed.shutdown().await;
        }
    }
}
