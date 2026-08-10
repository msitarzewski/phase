# Milestone: Reputation and Redundant Verification

**Status:** 🟦 In progress — implementation checkpoint approved; multi-peer acceptance pending
**Track:** Trust and scale
**Blocking:** Yes
**Depends on:** Live Relay Plane and Intended-Stream Vertical Slice
**Tracker:** [README.md](./README.md)

## Outcome

LUCID records attributable execution evidence, uses bounded redundant execution and spot checks to detect inconsistent peers, and makes routing/policy decisions from inspectable local evidence. It does not pretend that identity alone prevents Sybils or that two matching nondeterministic results constitute cryptographic proof.

The current authorization posture remains default-deny until reputation and open-load controls exist (`memory-bank/decisions.md:782-802`). Existing receipt verification already binds output to the dispatched manifest and serving PeerId in `crates/lucidd/src/router.rs:430-535`; this milestone consumes those verified outcomes rather than inventing a second receipt system.

## Trust model

- **Cryptographic fact:** a specific persistent key signed a receipt bound to a specific manifest and output commitment.
- **Observed fact:** a request succeeded, failed, timed out, refused, produced an invalid receipt, or diverged under a defined comparison.
- **Local judgment:** this node’s policy assigns routing weight or restrictions from its own evidence and configured trust anchors.
- **Not proven:** real-world identity, uniqueness of operator, universal honesty, or correctness of un-recomputed tensor work.

Reputation is therefore evidence-driven and local-first. A global score without Sybil resistance is not an acceptable substitute.

## Required reputation ADR

Approve a decision covering:

- Evidence schema, canonical identifiers, storage location, retention, pruning, and migration.
- Which outcomes are objective versus policy interpretations.
- Score/decision computation, weights, decay, confidence, cold start, and recovery.
- Sybil assumptions and why identities are or are not allowed to vouch for each other.
- Import/export and whether any signed peer observations are shared.
- Redundancy sampling rate, who pays the duplicate compute cost, and privacy implications.
- Output comparison for deterministic tokens versus floating-point tensors/embeddings.
- Appeals/operator overrides, auditability, and protection against poisoned evidence.
- Exact capability gate for changing the default authorization posture.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Stable identity | `crates/phase-identity/src/keypair.rs:27-100`, `crates/phase-identity/src/storage.rs:25-99`, `crates/phase-net/src/discovery.rs:210-228` | Key reputation by persistent PeerId/public key with explicit rotation handling |
| Verified outcomes | `crates/lucidd/src/router.rs:430-535` | Record evidence only after full receipt checks or a precisely classified failure |
| Routing | `crates/lucidd/src/router.rs:160-430` | Apply policy-defined eligibility/ordering without bypassing capability and content checks |
| Policy | `crates/lucidd/src/policy.rs:69-164`, `crates/lucidd/src/policy.rs:262-383` | Add reputation/open-load gates inside the existing policy engine |
| Metrics | `crates/phase-protocol/src/job_spec.rs:306-311` | Treat worker-attested metrics as untrusted observability, not correctness proof |
| DHT | `crates/lucidd/src/registry.rs:168-343`, `crates/lucidd/src/registry.rs:391-635` | Do not overload model advertisements with a universal reputation score |

## Evidence taxonomy

At minimum, distinguish:

- Verified successful completion.
- Verified cancellation.
- Verified worker error.
- Policy refusal and capacity refusal.
- Pre-output transport/discovery failure.
- Mid-stream transport loss.
- Missing terminal event or receipt.
- Invalid receipt signature.
- Manifest/job mismatch.
- Signer/PeerId mismatch.
- Output commitment/chunk-count/sequence mismatch.
- Deadline/idle timeout.
- Redundant-execution agreement, disagreement, or incomparable result.
- Operator-reviewed abuse evidence.

Do not collapse all failures into “bad peer.” Network loss, requester cancellation, and malicious receipt mismatch have different evidentiary value.

## Work packages

### Evidence store

- [ ] Define an append-oriented, crash-safe record keyed by event ID, local observer, remote PeerId, job class, CID, timestamp, and outcome.
- [ ] Exclude prompts, tokens, embeddings, model bytes, and other raw private payloads.
- [ ] Bind records to hashes/commitments and minimal diagnostic metadata.
- [ ] Make duplicate event ingestion idempotent.
- [ ] Support retention, compaction, corruption detection, backup, export, and reset.
- [ ] Record software/protocol versions so regressions can be distinguished from durable behavior.

### Decision engine

- [ ] Separate raw evidence from derived routing state.
- [ ] Compute confidence from evidence volume and recency; no high-confidence score from one event.
- [ ] Apply decay and recovery according to the ADR.
- [ ] Allow explicit operator pins/blocks with visible precedence.
- [ ] Expose a human-readable explanation for every reputation-influenced route/refusal.
- [ ] Preserve deterministic ordering when scores tie.

### Redundant execution

- [ ] Define eligible job classes, sampling triggers, privacy limits, and maximum duplicate cost.
- [ ] Dispatch duplicates to independently identified peers; do not accidentally choose the same underlying peer twice.
- [ ] For deterministic seeded inference, compare canonical output chunks/commitments.
- [ ] For embeddings or nondeterministic outputs, use only an approved tolerance/comparison and record “incomparable” when the comparison is not meaningful.
- [ ] Never expose two independent streams as one client success before selection semantics are defined.
- [ ] Attribute disagreement without automatically deciding which peer is malicious when no trusted reference exists.

### Spot checks

- [ ] Support controlled re-execution on a trusted/local worker where capacity permits.
- [ ] Select checks unpredictably enough that peers cannot trivially behave only on checked jobs.
- [ ] Bound check cost and ensure it cannot be amplified by an attacker.
- [ ] Record comparison method, tolerance, reference worker, and software/model CID.
- [ ] Keep raw comparison evidence for operator review within retention policy.

### Routing and authorization integration

- [ ] Reputation can filter or order only after content CID, capability, policy, and reachability checks.
- [ ] A cold-start peer receives a defined bounded opportunity, not unconditional trust or permanent exclusion.
- [ ] Invalid cryptographic evidence can trigger immediate quarantine under the ADR; ordinary failures should not.
- [ ] Route responses/logs explain whether reputation affected the decision.
- [ ] The authz default flips only after the explicit capability/load gate is approved and all abuse controls pass; this milestone does not flip it merely because a score exists.

## Adversarial tests

- [ ] Sybil swarm with many fresh PeerIds cannot manufacture high local confidence merely through self-claims.
- [ ] Colluding peers that agree on wrong output are not described as cryptographically verified.
- [ ] Evidence replay, duplicate delivery, forged observer, forged remote identity, timestamp manipulation, and database corruption are detected.
- [ ] A requester cannot cheaply force unbounded redundant compute or spot checks.
- [ ] Network partition and relay failure do not mass-penalize otherwise healthy peers.
- [ ] Version-specific regression can decay/recover after a fixed peer upgrades.
- [ ] Operator block/pin precedence is deterministic and auditable.
- [ ] Privacy review confirms no raw request/result content is persisted.

## Acceptance criteria

- [ ] Every remote execution produces either a classified local evidence record or an explicitly documented no-evidence condition.
- [ ] Raw evidence and derived reputation can be inspected separately.
- [ ] Routing decisions show an explanation and remain deterministic under identical state.
- [ ] Redundant deterministic jobs detect injected divergent output.
- [ ] Ambiguous nondeterministic disagreement remains ambiguous rather than falsely convicting a peer.
- [ ] Resource amplification stays within approved bounds.
- [ ] Sybil, replay, poisoned-evidence, and mass-network-failure tests pass.
- [ ] Default authorization changes only if the separate recorded capability trigger passes.
- [ ] Workspace and multi-peer QA pass.

## Explicit non-goals

- A blockchain, token staking, payment-weighted trust, or global consensus.
- Claiming that reputation proves computation correct.
- Storing prompts/results for later moderation.
- Automatically punishing peers for requester cancellations or unrelated network outages.
- Solving partial tensor verification; this milestone supplies evidence primitives used by ShardWorker’s research gate.

## Completion evidence required in the tracker

- Approved reputation/redundancy ADR and threat model.
- Evidence-schema examples containing no sensitive payload.
- Sybil/amplification/failure-injection QA report.
- Routing explanation demo and redundant divergence demo.
- Exact commit/PR and task documentation.

## 2026-08-09 approved implementation checkpoint

- Implemented private-at-creation append-only evidence storage, canonical event IDs, identity/context validation, bounded decoding, partial-tail recovery, corruption detection, retention/compaction, and operator-visible reset/export primitives at `crates/lucidd/src/reputation.rs:89-760`.
- Derived assessment separates raw evidence from decayed confidence; cold-start behavior, deterministic tie ordering, operator pin/block precedence, and attributable explanations are explicit and tested.
- LUCID records the full remote outcome taxonomy without raw prompts, tokens, embeddings, model bytes, or other private payloads. Serialization tests enforce that privacy boundary.
- Redundant verification is disabled by default, deterministically sampled and concurrency-bounded when enabled, restricted to exact-CID seeded jobs, and refuses to treat embedding/nondeterministic comparisons as proof at `crates/lucidd/src/router.rs:118-1398`.
- Remaining before completion: physical multi-peer divergence demo, load/amplification measurement, network-partition behavior, larger Sybil/collusion campaign, version recovery, and an approved reputation/redundancy ADR and threat model.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
