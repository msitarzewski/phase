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

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use phase_identity::NodeIdentity;
use phase_net::{
    Discovery, PeerId, ShardStageHandler, ShardStageRequest, ShardStageResponse,
    ShardStageResponseKind, SHARD_STAGE_DEFAULT_IDLE_TIMEOUT_MS, SHARD_STAGE_MAX_PAYLOAD_BYTES,
    SHARD_STAGE_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};

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

/// Completed output of an end-to-end Tier-1 multi-peer sharded pipeline run.
#[derive(Debug, Clone)]
pub struct ShardPipelineOutput {
    pub output_payload: Vec<u8>,
    pub receipts: Vec<SignedShardStageReceipt>,
    pub verification: CompositeShardVerification,
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
