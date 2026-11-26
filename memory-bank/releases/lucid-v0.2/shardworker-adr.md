# ADR Draft: ShardWorker Architecture & Partial-Compute Verification

**Status:** Proposed — Foundation types, planner, and composite stage-receipt verification implemented in `crates/lucidd/src/shard_plan.rs`; execution transport awaits whole-model baseline (`lucidd bench`) and human ADR sign-off
**Date:** 2026-11-25
**Milestone:** [ShardWorker](./shardworker.md)
**Depends on:** [Reputation and Redundant Verification](./reputation-and-redundant-verification.md), whole-model baseline (`crates/lucidd/src/bench.rs`)

---

## Context

Users with multiple heterogeneous machines (e.g. Ubuntu Desktop 16 GB RTX 5060 Ti + Mac M1 Max 32 GB + Mac M5 Max 128 GB) want to pool memory to run models larger than any single GPU (e.g. 70B–405B Q4/Q8 weights). However, `memory-bank/decisions.md:890-914` and [shardworker.md](./shardworker.md#hard-stop) correctly prohibit untrusted production sharding until seven hard-stop questions are answered without hand-waving.

This ADR separates **two explicitly labeled trust tiers** so LUCID can deliver real multi-machine memory pooling for private invited circles **without** making false cryptographic claims about untrusted peers.

---

## Decision: Two-Tier Sharding Model

| Property | Tier 1: `TrustedCircle` (`trusted-circle-sharded`) | Tier 2: `UntrustedSpotChecked` (`untrusted-spot-checked`) |
|---|---|---|
| **Eligible peers** | Explicitly invited / operator-owned peer circle only (`allow_unauthenticated_jobs = false` + explicit cluster opt-in) | Open / semi-trusted network peers |
| **Current gate status** | **Foundation implemented** (`crates/lucidd/src/shard_plan.rs`); execution transport next after `lucidd bench` baseline | **Fail-closed disabled** in `plan_pipeline_shards` until cross-hardware tolerance study passes |
| **What receipts prove** | Coordinator-signed `ShardPartitionPlan` + per-stage Ed25519 `ShardStageReceipt` chaining exact `ModelCid`, layer range `[start..end)`, peer identity, and SHA-256 activation handoff hashes (`stage[i].out == stage[i+1].in`) | Tier 1 guarantees **plus** probabilistic independent spot-checking of boundary hidden states (TOPLOC-style top-k activation commitments) |
| **Honest UI / HTTP label** | `X-Lucid-Trust-Label: trusted-circle-sharded` (attribution + handoff integrity; not third-party correctness proof) | `X-Lucid-Trust-Label: untrusted-spot-checked` (probabilistic detection only; never "ZK-proven") |

---

## Explicit Answers to the Seven Hard-Stop Questions (`shardworker.md:28-37`)

### 1. What result is being verified?
- **Tier 1:** Stage attribution, exact `ModelCid` binding, contiguous non-overlapping layer coverage `0..total_layers`, handoff hash continuity (`stage[i].output_commitment_hex == stage[i+1].input_commitment_hex`), and final output commitment binding (`stage[N-1].output_commitment_hex == completion_commitment`). Implemented and unit-tested in `crates/lucidd/src/shard_plan.rs:verify_composite_shard_chain`.
- **Tier 2 (research):** Boundary hidden-state activations at sampled decode steps (layer output top-k indices/mantissas), never claiming bitwise determinism across heterogeneous backends.

### 2. Which threat model is in scope?
- **In scope for Tier 1:** Peer crash/disconnect, silent truncation, layer misconfiguration/drift, stale or wrong `ModelCid` substitution, stage reordering, and external MITM/unauthenticated injection (blocked by Noise+TLS, signed `ShardPartitionPlan`, and default-deny authz).
- **Out of scope for Tier 1 (explicitly disclosed):** A deliberately malicious peer inside your own invited circle returning plausible-looking garbage tensors (mitigated in Tier 1 by explicit trust-circle membership; addressed in Tier 2 by spot-checking).
- **In scope for Tier 2:** Opportunistic layer skipping, low-rank/cheaper-model substitution, random perturbation, and single-stage cheating. Coordinated collusion across all stages remains detectable only by an independent full-reference validator.

### 3. Redundant execution / spot-check rate, detection probability, and cost
- **Tier 1:** `0x` redundant compute overhead (`1.0x` total FLOPs) — optimal for private multi-machine clusters (e.g. your 5060 Ti + M1 Max + M5 Max).
- **Tier 2:** Sample `p = 2%–5%` of jobs (bounded by existing `redundant_verification_permille` pattern in `crates/lucidd/src/router/mod.rs`), giving detection probability `1 - (1 - p)^k` (at `p = 0.05`, a cheater modifying 50 requests has a `92.3%` chance of detection; slashing/local peer blocking via `Reputation` deters repeat abuse at `1.05x` average compute cost).

### 4. Floating-point divergence across Metal / CUDA / Vulkan / CPU
- Bitwise equality does **not** hold between Apple Metal FP16, NVIDIA CUDA FP16/BF16, and CPU FP32 kernels.
- **Tier 1:** Handoff commitments hash the exact serialized tensor bytes transmitted on the wire between stage `i` and stage `i+1` — so cross-backend FP divergence does not break handoff verification (stage `i` hashes what it sent, stage `i+1` hashes what it received!).
- **Tier 2:** Before enabling Tier 2, run the determinism study across **Ubuntu Desktop (CUDA)** and **Mac M1 Max / M5 Max (Metal)** on identical `ModelCid` + `temperature=0, seed=42` inputs to calibrate per-quantization cosine-similarity (`>= 0.999`) and top-k logit overlap thresholds.

### 5. Who holds full model/shard reference state?
- **Weight distribution:** Reuses `ContentPlane` (`crates/lucidd/src/content/mod.rs`) content-addressed chunks. Each stage verifies the exact `ModelCid` (or future per-shard tensor-file Merkle root derived from the canonical `ModelCid`) before loading.
- **Tier 1:** Each participating peer holds only its assigned layer range (or full GGUF mmap'd with only assigned layers offloaded to GPU/RAM).
- **Tier 2:** Independent high-memory reference validators (e.g. a 128 GB node or 2-peer spot-check pair) validate sampled stages; no serving peer validates itself.

### 6. How do receipts compose across peers?
Implemented in `crates/lucidd/src/shard_plan.rs`:
1. Coordinator generates and signs `SignedShardPartitionPlan` (domain `"phase/lucid-shard-plan/v1"`) binding `plan_id`, `model_cid_hex`, `total_layers`, `trust_tier`, `expires_at_ms`, and ordered `ShardStageAssignment`s (`stage_index`, `peer_id`, `verifying_key_hex`, `start_layer..end_layer`).
2. Each stage `i` signs `SignedShardStageReceipt` (domain `"phase/lucid-shard-stage-receipt/v1"`) binding `(plan_id, job_id, model_cid_hex, stage_index, start_layer, end_layer, input_commitment_hex, output_commitment_hex)`.
3. Client/coordinator calls `verify_composite_shard_chain(...)`, which fails closed on any missing stage, expired plan, wrong peer key, layer gap/overlap, broken activation handoff (`stage[i].output != stage[i+1].input`), or final output mismatch.

### 7. Reject / retry / degrade policy
- **Stage failure before first token:** Re-plan around remaining healthy peers if aggregate memory still covers `total_layers`; otherwise fail cleanly with `503` (`cluster_capacity_insufficient`).
- **Stage failure mid-stream:** Abort immediately, release KV cache on all stages via cancellation signal, and return an unverified error frame — **never** emit `done: true` with verified receipt.
- **Single peer can fit whole model:** Router prefers whole-model execution unless user explicitly forces cluster mode (zero unnecessary WAN tensor latency).
- **Untrusted sharding requested:** Fail closed (`plan_pipeline_shards` returns error) until Tier 2 passes acceptance.

---

## Execution Transport Options (Next Implementation Phase)

Once `lucidd bench` records the 1-peer vs 2-peer whole-model baseline on **Ubuntu Desktop (5060ti 16GB) + Mac M1 Max (32GB)**:
1. **Stage transport:** Dedicated bounded libp2p stream protocol `/phase/shard-stage/1.0.0` (carried as opaque bytes in `phase-net`, preserving substrate workload neutrality per `memory-bank/decisions.md:854-875`) with FP16/INT8 activation framing + per-stage backpressure.
2. **Backend execution:** Bridge `ShardWorker` (implementing `phase_protocol::Worker` at `crates/phase-protocol/src/worker.rs:55-88`) to `llama.cpp` RPC worker stages (`rpc-server` / pipeline-parallel layer split) running inside the exact same `env_clear()` loopback sandbox as `LlamaCppWorker`.
3. **Desktop UI (`apps/lucid-desktop`):** Surface an explicit **"Cluster Mode (Trusted Circle)"** badge whenever a model exceeds single-machine memory and spans multiple invited peers, showing each contributing peer and its assigned layer range.
