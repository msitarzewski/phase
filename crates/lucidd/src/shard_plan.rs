// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pipeline-sharded execution planner, signed partition plans, and chained
//! stage-receipt verification (`ShardWorker` foundation).
//!
//! Trust & honesty boundaries (see `memory-bank/releases/lucid-v0.2/shardworker-adr.md`):
//! - **Tier 1 (`ShardTrustTier::TrustedCircle`)**: explicitly authorized
//!   private circles (e.g., an operator's own heterogeneous fleet: RTX 5060 Ti
//!   16 GB + Mac M1 Max 32 GB + Mac M5 Max 128 GB). Every stage signs a
//!   `ShardStageReceipt` proving identity, layer range, and activation handoff
//!   continuity. Surfaced with explicit label `"trusted-circle-sharded"` —
//!   never misrepresented as cryptographically verified untrusted compute.
//! - **Tier 2 (`ShardTrustTier::UntrustedSpotChecked`)**: blocked fail-closed
//!   until the partial-compute verification research gate and cross-hardware
//!   tolerance ADR pass acceptance.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use phase_identity::NodeIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SHARD_PLAN_SCHEMA_VERSION: u32 = 1;
pub const SHARD_PLAN_DOMAIN: &str = "phase/lucid-shard-plan/v1";
pub const SHARD_STAGE_RECEIPT_DOMAIN: &str = "phase/lucid-shard-stage-receipt/v1";
pub const MAX_SHARD_STAGES: usize = 16;

/// Explicit trust model for a sharded execution plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShardTrustTier {
    /// Private invited/operator-owned peer cluster. Stages sign receipts
    /// proving attribution and handoff hashes; intermediate tensors are not
    /// independently re-verified by third parties.
    TrustedCircle,
    /// Open/untrusted peer cluster with probabilistic activation spot checks.
    /// Currently fail-closed pending partial-compute verification ADR approval.
    UntrustedSpotChecked,
}

impl ShardTrustTier {
    pub fn trust_label(self) -> &'static str {
        match self {
            Self::TrustedCircle => "trusted-circle-sharded",
            Self::UntrustedSpotChecked => "untrusted-spot-checked",
        }
    }
}

/// Hardware & memory envelope advertised by one candidate peer for sharding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerShardCapacity {
    pub peer_id: String,
    /// Hex-encoded 32-byte Ed25519 verifying key bound to `peer_id`.
    pub verifying_key_hex: String,
    /// Free GPU/unified memory the operator permits LUCID to use for weights +
    /// KV cache (after reserving operator headroom).
    pub available_memory_bytes: u64,
    /// Backend label (`"cuda"`, `"metal"`, `"vulkan"`, `"cpu"`).
    pub backend: String,
}

/// Static layer and memory footprint of a model revision (`ModelCid`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelShardSpec {
    pub model_cid_hex: String,
    pub model_alias: String,
    pub total_layers: u32,
    pub per_layer_bytes: u64,
    pub embedding_bytes: u64,
    pub lm_head_bytes: u64,
}

impl ModelShardSpec {
    pub fn total_required_bytes(&self) -> u64 {
        self.embedding_bytes
            .saturating_add(self.lm_head_bytes)
            .saturating_add(
                self.per_layer_bytes
                    .saturating_mul(u64::from(self.total_layers)),
            )
    }
}

/// One contiguous pipeline stage assigned to a single peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardStageAssignment {
    pub stage_index: u32,
    pub peer_id: String,
    pub verifying_key_hex: String,
    pub start_layer: u32,
    /// Exclusive upper bound (`start_layer < end_layer <= total_layers`).
    pub end_layer: u32,
    pub includes_embedding: bool,
    pub includes_lm_head: bool,
    pub estimated_memory_bytes: u64,
}

/// Immutable pipeline partition plan covering every layer `0..total_layers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardPartitionPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub model_cid_hex: String,
    pub model_alias: String,
    pub total_layers: u32,
    pub trust_tier: ShardTrustTier,
    pub trust_label: String,
    pub coordinator_pubkey_hex: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub stages: Vec<ShardStageAssignment>,
}

/// Coordinator-signed wrapper around [`ShardPartitionPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedShardPartitionPlan {
    pub plan: ShardPartitionPlan,
    pub coordinator_signature_hex: String,
}

/// Per-stage signed execution receipt proving which peer ran which contiguous
/// layers and committing to its exact input and output boundary activations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardStageReceiptBody {
    pub schema_version: u32,
    pub plan_id: String,
    pub job_id: String,
    pub model_cid_hex: String,
    pub stage_index: u32,
    pub start_layer: u32,
    pub end_layer: u32,
    pub peer_id: String,
    pub worker_pubkey_hex: String,
    /// SHA-256 hex of the serialized input tokens/activations received by this
    /// stage (`stage 0` commits to the prompt token buffer).
    pub input_commitment_hex: String,
    /// SHA-256 hex of the serialized output activations/tokens emitted by this
    /// stage (`final stage` commits to the canonical completion output).
    pub output_commitment_hex: String,
    pub completed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedShardStageReceipt {
    pub body: ShardStageReceiptBody,
    pub signature_hex: String,
}

/// Outcome of verifying a complete composite stage-receipt chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositeShardVerification {
    pub plan_id: String,
    pub job_id: String,
    pub stage_count: usize,
    pub trust_label: String,
    pub serving_peers: Vec<String>,
    pub final_output_commitment_hex: String,
}

/// Plan a deterministic, memory-proportional contiguous layer partition across
/// heterogeneous peers.
///
/// Sequencing & safety rules enforced:
/// - Rejects `ShardTrustTier::UntrustedSpotChecked` fail-closed pending ADR.
/// - Requires at least 2 distinct peers (whole-model path handles 1 peer).
/// - Guarantees every stage fits strictly within its peer's
///   `available_memory_bytes`, with exact coverage of `0..total_layers`.
pub fn plan_pipeline_shards(
    coordinator: &NodeIdentity,
    model: &ModelShardSpec,
    peers: &[PeerShardCapacity],
    trust_tier: ShardTrustTier,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<SignedShardPartitionPlan> {
    if trust_tier != ShardTrustTier::TrustedCircle {
        bail!(
            "untrusted sharded execution ({}) is disabled pending partial-compute verification ADR approval",
            trust_tier.trust_label()
        );
    }
    if model.total_layers < 2 {
        bail!("model must have at least 2 layers to partition across peers");
    }
    if model.per_layer_bytes == 0 {
        bail!("per_layer_bytes must be positive");
    }
    if model.model_cid_hex.len() != 64
        || !model.model_cid_hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("model_cid_hex must be a 64-character hex SHA-256 digest");
    }
    if peers.len() < 2 {
        bail!("pipeline sharding requires at least 2 distinct peers");
    }
    if peers.len() > MAX_SHARD_STAGES {
        bail!("at most {MAX_SHARD_STAGES} shard stages are permitted");
    }
    if ttl_ms == 0 || ttl_ms > 24 * 3600 * 1000 {
        bail!("shard plan TTL must be in 1ms..=24h");
    }

    // Validate distinct peers and verifying keys, sorting deterministically by
    // descending available memory (tie-broken by peer_id) so stage 0 and the
    // final stage absorb embedding/lm_head cleanly on the highest-memory peers.
    let mut seen_peers = HashSet::new();
    let mut ordered = Vec::with_capacity(peers.len());
    for peer in peers {
        if peer.peer_id.trim().is_empty() {
            bail!("peer_id cannot be empty");
        }
        if !seen_peers.insert(peer.peer_id.as_str()) {
            bail!(
                "duplicate peer_id '{}' in shard candidate list",
                peer.peer_id
            );
        }
        parse_verifying_key_hex(&peer.verifying_key_hex)
            .with_context(|| format!("invalid verifying_key_hex for peer {}", peer.peer_id))?;
        ordered.push(peer.clone());
    }
    ordered.sort_by(|a, b| {
        b.available_memory_bytes
            .cmp(&a.available_memory_bytes)
            .then_with(|| a.peer_id.cmp(&b.peer_id))
    });

    // Compute exact maximum layers each stage can hold after fixed stage
    // overhead (stage 0 holds embedding, final stage holds lm_head).
    let stage_count = ordered.len();
    let mut max_layers_per_stage = Vec::with_capacity(stage_count);
    let mut total_layer_capacity: u64 = 0;

    for (idx, peer) in ordered.iter().enumerate() {
        let fixed_overhead = (if idx == 0 { model.embedding_bytes } else { 0 }).saturating_add(
            if idx + 1 == stage_count {
                model.lm_head_bytes
            } else {
                0
            },
        );
        if peer.available_memory_bytes <= fixed_overhead {
            bail!(
                "peer {} has insufficient memory ({} bytes) for stage {idx} fixed overhead ({} bytes)",
                peer.peer_id,
                peer.available_memory_bytes,
                fixed_overhead
            );
        }
        let max_layers = (peer.available_memory_bytes - fixed_overhead) / model.per_layer_bytes;
        if max_layers == 0 {
            bail!(
                "peer {} cannot hold even 1 layer after stage {idx} overhead",
                peer.peer_id
            );
        }
        max_layers_per_stage.push(max_layers);
        total_layer_capacity = total_layer_capacity.saturating_add(max_layers);
    }

    if total_layer_capacity < u64::from(model.total_layers) {
        bail!(
            "aggregate cluster layer capacity ({total_layer_capacity}) is below required {} layers",
            model.total_layers
        );
    }

    // Allocate layers proportionally to each peer's remaining layer capacity
    // while guaranteeing every participating stage receives at least 1 layer
    // and never exceeds its own `max_layers_per_stage[i]`.
    let mut assigned: Vec<u32> = vec![1; stage_count];
    let mut remaining = model
        .total_layers
        .checked_sub(stage_count as u32)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "more peers ({stage_count}) than model layers ({})",
                model.total_layers
            )
        })?;

    if remaining > 0 {
        let slack_sum: u64 = max_layers_per_stage
            .iter()
            .map(|cap| cap.saturating_sub(1))
            .sum();
        if slack_sum < u64::from(remaining) {
            bail!("cluster memory slack cannot cover remaining layers");
        }
        // First pass: proportional floor allocation.
        for (idx, cap) in max_layers_per_stage.iter().enumerate() {
            let slack = cap.saturating_sub(1);
            let share =
                ((u128::from(remaining) * u128::from(slack)) / u128::from(slack_sum)) as u32;
            let clamped = share.min(slack.min(u64::from(u32::MAX)) as u32);
            assigned[idx] += clamped;
        }
        let distributed: u32 = assigned.iter().sum();
        remaining = model.total_layers.saturating_sub(distributed);

        // Second pass: round-robin remainder onto peers with free headroom.
        let mut cursor = 0usize;
        while remaining > 0 {
            let cap = max_layers_per_stage[cursor].min(u64::from(u32::MAX)) as u32;
            if assigned[cursor] < cap {
                assigned[cursor] += 1;
                remaining -= 1;
            }
            cursor = (cursor + 1) % stage_count;
        }
    }

    let mut stages = Vec::with_capacity(stage_count);
    let mut next_layer = 0u32;
    for (idx, peer) in ordered.iter().enumerate() {
        let count = assigned[idx];
        let start_layer = next_layer;
        let end_layer = start_layer + count;
        next_layer = end_layer;
        let includes_embedding = idx == 0;
        let includes_lm_head = idx + 1 == stage_count;
        let estimated_memory_bytes = (if includes_embedding {
            model.embedding_bytes
        } else {
            0
        })
        .saturating_add(if includes_lm_head {
            model.lm_head_bytes
        } else {
            0
        })
        .saturating_add(model.per_layer_bytes.saturating_mul(u64::from(count)));
        if estimated_memory_bytes > peer.available_memory_bytes {
            bail!(
                "internal planner invariant violated: stage {idx} exceeds peer {} memory",
                peer.peer_id
            );
        }
        stages.push(ShardStageAssignment {
            stage_index: idx as u32,
            peer_id: peer.peer_id.clone(),
            verifying_key_hex: peer.verifying_key_hex.clone(),
            start_layer,
            end_layer,
            includes_embedding,
            includes_lm_head,
            estimated_memory_bytes,
        });
    }
    debug_assert_eq!(next_layer, model.total_layers);

    let plan = ShardPartitionPlan {
        schema_version: SHARD_PLAN_SCHEMA_VERSION,
        plan_id: format!("shardplan-{}", uuid::Uuid::new_v4().simple()),
        model_cid_hex: model.model_cid_hex.to_ascii_lowercase(),
        model_alias: model.model_alias.clone(),
        total_layers: model.total_layers,
        trust_tier,
        trust_label: trust_tier.trust_label().to_string(),
        coordinator_pubkey_hex: hex::encode(coordinator.peer_id_bytes()),
        created_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(ttl_ms),
        stages,
    };

    sign_shard_plan(coordinator, plan)
}

pub fn sign_shard_plan(
    coordinator: &NodeIdentity,
    plan: ShardPartitionPlan,
) -> Result<SignedShardPartitionPlan> {
    let payload = domain_digest(SHARD_PLAN_DOMAIN, &plan)?;
    let sig = coordinator.signing_key().sign(&payload);
    Ok(SignedShardPartitionPlan {
        plan,
        coordinator_signature_hex: hex::encode(sig.to_bytes()),
    })
}

pub fn verify_shard_plan(signed: &SignedShardPartitionPlan, now_ms: u64) -> Result<()> {
    let plan = &signed.plan;
    if plan.schema_version != SHARD_PLAN_SCHEMA_VERSION {
        bail!("unsupported shard plan schema {}", plan.schema_version);
    }
    if plan.trust_tier != ShardTrustTier::TrustedCircle {
        bail!("untrusted sharded execution is not permitted");
    }
    if now_ms > plan.expires_at_ms {
        bail!("shard partition plan {} is expired", plan.plan_id);
    }
    if plan.stages.len() < 2 || plan.stages.len() > MAX_SHARD_STAGES {
        bail!("invalid stage count {}", plan.stages.len());
    }

    let vk = parse_verifying_key_hex(&plan.coordinator_pubkey_hex)?;
    let sig = parse_signature_hex(&signed.coordinator_signature_hex)?;
    let payload = domain_digest(SHARD_PLAN_DOMAIN, plan)?;
    vk.verify(&payload, &sig)
        .map_err(|_| anyhow::anyhow!("coordinator signature verification failed"))?;

    // Verify exact contiguous coverage of 0..total_layers with zero gaps/overlaps.
    let mut expected_layer = 0u32;
    for (idx, stage) in plan.stages.iter().enumerate() {
        if stage.stage_index != idx as u32 {
            bail!("stage index mismatch at position {idx}");
        }
        if stage.start_layer != expected_layer || stage.end_layer <= stage.start_layer {
            bail!(
                "non-contiguous layer range {}..{} at stage {idx}",
                stage.start_layer,
                stage.end_layer
            );
        }
        expected_layer = stage.end_layer;
    }
    if expected_layer != plan.total_layers {
        bail!(
            "plan covers {} layers, expected {}",
            expected_layer,
            plan.total_layers
        );
    }
    Ok(())
}

pub fn sign_stage_receipt(
    worker: &NodeIdentity,
    body: ShardStageReceiptBody,
) -> Result<SignedShardStageReceipt> {
    let payload = domain_digest(SHARD_STAGE_RECEIPT_DOMAIN, &body)?;
    let sig = worker.signing_key().sign(&payload);
    Ok(SignedShardStageReceipt {
        body,
        signature_hex: hex::encode(sig.to_bytes()),
    })
}

/// Verify an entire composite chain of per-stage signed receipts against a
/// verified [`SignedShardPartitionPlan`], enforcing exact peer attribution,
/// layer boundaries, and activation commitment handoff continuity across every
/// stage boundary.
pub fn verify_composite_shard_chain(
    signed_plan: &SignedShardPartitionPlan,
    receipts: &[SignedShardStageReceipt],
    expected_job_id: &str,
    expected_input_commitment_hex: &str,
    expected_final_output_hex: &str,
    now_ms: u64,
) -> Result<CompositeShardVerification> {
    verify_shard_plan(signed_plan, now_ms)?;
    let plan = &signed_plan.plan;

    if receipts.len() != plan.stages.len() {
        bail!(
            "expected {} stage receipts, received {}",
            plan.stages.len(),
            receipts.len()
        );
    }

    let mut expected_in = expected_input_commitment_hex.to_ascii_lowercase();
    let mut serving_peers = Vec::with_capacity(plan.stages.len());

    for (stage, receipt) in plan.stages.iter().zip(receipts.iter()) {
        let body = &receipt.body;
        if body.schema_version != SHARD_PLAN_SCHEMA_VERSION {
            bail!("unsupported stage receipt schema {}", body.schema_version);
        }
        if body.plan_id != plan.plan_id {
            bail!("stage {} receipt bound to wrong plan_id", stage.stage_index);
        }
        if body.job_id != expected_job_id {
            bail!("stage {} receipt bound to wrong job_id", stage.stage_index);
        }
        if !body.model_cid_hex.eq_ignore_ascii_case(&plan.model_cid_hex) {
            bail!(
                "stage {} receipt bound to wrong model_cid",
                stage.stage_index
            );
        }
        if body.stage_index != stage.stage_index
            || body.start_layer != stage.start_layer
            || body.end_layer != stage.end_layer
        {
            bail!(
                "stage {} layer assignment mismatch in receipt",
                stage.stage_index
            );
        }
        if body.peer_id != stage.peer_id
            || !body
                .worker_pubkey_hex
                .eq_ignore_ascii_case(&stage.verifying_key_hex)
        {
            bail!(
                "stage {} receipt signed by unexpected peer/key",
                stage.stage_index
            );
        }
        if !body.input_commitment_hex.eq_ignore_ascii_case(&expected_in) {
            bail!(
                "activation handoff broken entering stage {}",
                stage.stage_index
            );
        }

        let vk = parse_verifying_key_hex(&stage.verifying_key_hex)?;
        let sig = parse_signature_hex(&receipt.signature_hex)?;
        let payload = domain_digest(SHARD_STAGE_RECEIPT_DOMAIN, body)?;
        vk.verify(&payload, &sig).map_err(|_| {
            anyhow::anyhow!("invalid signature on stage {} receipt", stage.stage_index)
        })?;

        expected_in = body.output_commitment_hex.to_ascii_lowercase();
        serving_peers.push(stage.peer_id.clone());
    }

    if !expected_in.eq_ignore_ascii_case(expected_final_output_hex) {
        bail!("final stage output commitment does not match delivered completion");
    }

    Ok(CompositeShardVerification {
        plan_id: plan.plan_id.clone(),
        job_id: expected_job_id.to_string(),
        stage_count: plan.stages.len(),
        trust_label: plan.trust_label.clone(),
        serving_peers,
        final_output_commitment_hex: expected_in,
    })
}

fn domain_digest<T: Serialize>(domain: &str, value: &T) -> Result<[u8; 32]> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0u8]);
    hasher.update(&bytes);
    Ok(hasher.finalize().into())
}

fn parse_verifying_key_hex(raw: &str) -> Result<VerifyingKey> {
    let bytes = hex::decode(raw).context("invalid hex verifying key")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("verifying key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| anyhow::anyhow!("invalid Ed25519 verifying key"))
}

fn parse_signature_hex(raw: &str) -> Result<Signature> {
    let bytes = hex::decode(raw).context("invalid hex signature")?;
    let arr: [u8; 64] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    Ok(Signature::from_bytes(&arr))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn sample_70b_model() -> ModelShardSpec {
        // ~42 GiB 70B Q4_K_M model with 80 transformer layers (~500 MiB/layer +
        // 1 GiB embedding + 1 GiB LM head): cannot fit on a single 16 GiB or
        // 32 GiB node, fits cleanly across 5060 Ti (12 GiB free) + M1 Max
        // (24 GiB free) + M5 Max (64 GiB free).
        ModelShardSpec {
            model_cid_hex: "a".repeat(64),
            model_alias: "llama3.3:70b-q4_k_m".to_string(),
            total_layers: 80,
            per_layer_bytes: 500 * 1024 * 1024,
            embedding_bytes: GIB,
            lm_head_bytes: GIB,
        }
    }

    #[test]
    fn plans_heterogeneous_fleet_proportionally_and_verifies_composite_receipt_chain() {
        let coord = NodeIdentity::generate();
        let desktop_5060ti = NodeIdentity::generate();
        let mac_m1_max = NodeIdentity::generate();
        let mac_m5_max = NodeIdentity::generate();

        let peers = vec![
            PeerShardCapacity {
                peer_id: "peer-5060ti".to_string(),
                verifying_key_hex: hex::encode(desktop_5060ti.peer_id_bytes()),
                available_memory_bytes: 12 * GIB,
                backend: "cuda".to_string(),
            },
            PeerShardCapacity {
                peer_id: "peer-m1max".to_string(),
                verifying_key_hex: hex::encode(mac_m1_max.peer_id_bytes()),
                available_memory_bytes: 24 * GIB,
                backend: "metal".to_string(),
            },
            PeerShardCapacity {
                peer_id: "peer-m5max".to_string(),
                verifying_key_hex: hex::encode(mac_m5_max.peer_id_bytes()),
                available_memory_bytes: 64 * GIB,
                backend: "metal".to_string(),
            },
        ];

        let model = sample_70b_model();
        let signed_plan = plan_pipeline_shards(
            &coord,
            &model,
            &peers,
            ShardTrustTier::TrustedCircle,
            1_000,
            60_000,
        )
        .expect("plan should succeed across 100 GiB aggregate memory");

        assert_eq!(signed_plan.plan.stages.len(), 3);
        assert_eq!(signed_plan.plan.stages[0].start_layer, 0);
        assert_eq!(signed_plan.plan.stages[2].end_layer, 80);
        assert_eq!(signed_plan.plan.trust_label, "trusted-circle-sharded");

        // Build chained stage receipts from each assigned peer.
        let identities = [
            ("peer-m5max", &mac_m5_max),
            ("peer-m1max", &mac_m1_max),
            ("peer-5060ti", &desktop_5060ti),
        ];
        let prompt_hash = "1".repeat(64);
        let h1 = "2".repeat(64);
        let h2 = "3".repeat(64);
        let final_hash = "4".repeat(64);
        let boundaries = [(&prompt_hash, &h1), (&h1, &h2), (&h2, &final_hash)];

        let mut receipts = Vec::new();
        for (idx, stage) in signed_plan.plan.stages.iter().enumerate() {
            let id = identities
                .iter()
                .find(|(name, _)| *name == stage.peer_id)
                .unwrap()
                .1;
            let body = ShardStageReceiptBody {
                schema_version: SHARD_PLAN_SCHEMA_VERSION,
                plan_id: signed_plan.plan.plan_id.clone(),
                job_id: "job-70b-1".to_string(),
                model_cid_hex: model.model_cid_hex.clone(),
                stage_index: stage.stage_index,
                start_layer: stage.start_layer,
                end_layer: stage.end_layer,
                peer_id: stage.peer_id.clone(),
                worker_pubkey_hex: hex::encode(id.peer_id_bytes()),
                input_commitment_hex: boundaries[idx].0.clone(),
                output_commitment_hex: boundaries[idx].1.clone(),
                completed_at_ms: 2_000 + idx as u64,
            };
            receipts.push(sign_stage_receipt(id, body).unwrap());
        }

        let verified = verify_composite_shard_chain(
            &signed_plan,
            &receipts,
            "job-70b-1",
            &prompt_hash,
            &final_hash,
            5_000,
        )
        .expect("composite receipt chain must verify");
        assert_eq!(verified.stage_count, 3);
        assert_eq!(verified.trust_label, "trusted-circle-sharded");

        // Tampering with an intermediate activation handoff must fail closed.
        let mut broken = receipts.clone();
        broken[1].body.input_commitment_hex = "9".repeat(64);
        assert!(verify_composite_shard_chain(
            &signed_plan,
            &broken,
            "job-70b-1",
            &prompt_hash,
            &final_hash,
            5_000,
        )
        .is_err());

        // Untrusted sharding remains fail-closed pending ADR.
        assert!(plan_pipeline_shards(
            &coord,
            &model,
            &peers,
            ShardTrustTier::UntrustedSpotChecked,
            1_000,
            60_000,
        )
        .is_err());
    }
}
