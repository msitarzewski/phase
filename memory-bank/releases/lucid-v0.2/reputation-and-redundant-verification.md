# Milestone: Reputation and Redundant Verification

**Status:** 🟦 In progress — implementation checkpoint approved; multi-peer acceptance pending
**Track:** Trust and scale
**Blocking:** Yes
**Depends on:** Live Relay Plane and Intended-Stream Vertical Slice
**Tracker:** [README.md](./README.md)

## Outcome

LUCID records attributable execution evidence, uses bounded redundant execution and spot checks to detect inconsistent peers, and makes routing/policy decisions from inspectable local evidence. It does not pretend that identity alone prevents Sybils or that two matching nondeterministic results constitute cryptographic proof.

The current authorization posture remains default-deny until correctness-evidence validation, reputation, and open-load controls exist (`memory-bank/decisions.md#2026-05-28-hybrid-signer-authorization-policy-default-deny-allowlist--peerid-bind`). Existing receipt verification already binds output to the dispatched manifest and serving PeerId in `crates/lucidd/src/router.rs:1550-1678`; this milestone consumes those verified outcomes rather than inventing a second receipt system.

## Trust model

- **Cryptographic fact:** a specific persistent key signed a receipt bound to a specific manifest and output commitment.
- **Observed fact:** a request succeeded, failed, timed out, refused, produced an invalid receipt, or diverged under a defined comparison.
- **Local judgment:** this node’s policy assigns routing weight or restrictions from its own evidence and configured trust anchors.
- **Not proven:** real-world identity, uniqueness of operator, universal honesty, or correctness of un-recomputed tensor work.

Reputation is therefore evidence-driven and local-first. A global score without Sybil resistance is not an acceptable substitute.

## Integrity versus computation correctness

Receipt verification proves an attributable, untampered transcript. Signature, job binding, PeerId binding, output commitment, chunk count, and sequence checks answer: “Did this peer sign exactly the result delivered for this dispatched job?” They do not answer: “Did the peer load the claimed model and execute the claimed backend, precision, and software correctly?” (`crates/lucidd/src/router.rs:1596-1612`, `crates/phase-protocol/src/commitment.rs:29-86`).

LUCID may strengthen the second claim with a separate, bounded correctness-evidence seam. A serving backend can produce a versioned opaque evidence payload derived from selected execution traces or activations; an independently selected validator interprets that method and signs a verdict. Phase may transport and commit the bounded opaque bytes through its existing worker stream, but inference-specific evidence generation, validation, sampling, tolerance, and trust labels remain in LUCID.

Validator evidence is empirical or probabilistic unless a separately approved mechanism establishes a stronger guarantee. It augments receipt integrity and redundant comparison; it never silently upgrades either into cryptographic proof.

## Method comparison — 2026-11-07

**State:** research/experiment specification, not an implemented validator or a selected production method. This extends the accepted 2026-08-24 sequencing decision; the method-specific ADR and measured acceptance remain required.

| Method | Existing evidence / integration | Permitted interpretation | Limit to test |
|---|---|---|---|
| Signed receipt plus commitment replay | `crates/lucidd/src/router.rs:1550-1612`; already implemented | Attributable transcript integrity | A dishonest model can still sign an internally consistent wrong transcript |
| Exact-CID redundant execution | `crates/lucidd/src/router.rs:1305-1398`; bounded seeded/backend-equivalent checks exist | Literal agreement, disagreement or incomparable | Collusion, nondeterminism, two identities operated by one party, duplicate compute cost |
| Activation fingerprints / TOPLOC | [Paper](https://arxiv.org/abs/2501.16007), [implementation](https://github.com/PrimeIntellect-ai/toploc), [INTELLECT-2 usage](https://arxiv.org/abs/2505.07291) | Empirical consistency with the specified reference/method under measured tolerances | Backend activation hooks, quantization differences, sampling checks, privacy exposure and validator overhead; upstream headline accuracy is not a Phase guarantee |
| Cross-provider replay audit | [OpenHydra known issues](https://github.com/samtroberts/openhydra/blob/main/KNOWN_ISSUES.md), [verification policy](https://github.com/samtroberts/openhydra/blob/main/protocol/src/verify.rs) | Agreement under an explicitly defined comparison | Cross-backend false disagreements, collusion, duplicated private prompts and fan-out amplification |
| Teacher-forced log-probability challenge | [OpenHydra network path](https://github.com/samtroberts/openhydra/blob/main/agent/src/prompt_score.rs), [method and limitations](https://github.com/samtroberts/openhydra/blob/main/protocol/src/logprob.rs) | Ability to produce the claimed distribution for the challenged request | A challenge-aware provider may pass checks while substituting real traffic; reference reuse/replay and model/precision substitution |
| TEE attestation | [Cocoon architecture](https://cocoon.org/architecture) | Trust in an attested image under the hardware/vendor/root assumptions | Consumer-device availability, image/model allowlists, root control and vendor trust; outside the current non-TEE release contract |

### Reproducible experiment

1. Pin source, model CID, tokenizer/template, backend/version, precision, seed/sampling policy, input commitment and output commitment. Use the existing alias/provider/content path for every reference model; a name or provider claim alone is not an exact revision.
2. Run the honest baseline on each supported backend/hardware pair separately. Measure repeated-run and cross-backend divergence before selecting any tolerance. The current llama.cpp adapter's completion API does not supply TOPLOC activations; an adapter hook must be implemented and measured before claiming that method is supported. MLX fixture success is not an activation hook or real-device validation.
3. Introduce controlled wrong-model, wrong-precision, stale-CID, output-substitution, truncated-evidence, replay, challenge-only-honesty, worker/validator-collusion and validator-unavailable cases. Keep these in an isolated research/test boundary, never a production fake worker.
4. Select a validator independently of the serving peer. Bind its signed verdict to exact job, output, model/backend identity, method/version, evidence commitment and expiry. A second PeerId does not establish a second operator; state the trust anchor and collusion assumptions.
5. Record honest rejection and adversarial detection counts with sample sizes, supported/unsupported configurations, validation latency, peak memory, extra compute multiplier and bytes disclosed to validators. Do not label a small observed error count as a universal probability bound.
6. Persist only bounded verdict metadata and commitments in the existing evidence store (`MAX_EVIDENCE_RECORD_BYTES` is 4 KiB at `crates/lucidd/src/reputation.rs:43`). Raw activation traces and prompts require a separately approved ephemeral transport/privacy budget; do not insert them into reputation records.
7. Compare one-peer baseline cost with each candidate. Use the existing redundant-check concurrency ceiling and record actual validator fan-out. Failure or budget exhaustion must not silently downgrade a caller-requested stronger trust mode to verified success.

### Go/no-go record

The method ADR must include the reproducible commands/source revisions, configuration matrix, measured detection and honest-divergence results, bounds, validator independence, privacy exposure, unsupported cases and client labels. Without those results, the honest outcome is **method not qualified**. It does not unblock public authorization or ShardWorker.

Client/reporting vocabulary must keep `transcript integrity verified`, `redundant agreement`, `model capability challenge passed` and `independent execution consistency checked` separate. The last label is allowed only after its method gate passes; none of these labels asserts cryptographic proof of arbitrary model execution. Existing receipt headers remain integrity-only and are not renamed into stronger guarantees.

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
| Correctness evidence | `crates/phase-protocol/src/worker.rs:315-378`, `crates/phase-protocol/src/commitment.rs:29-86` | Carry only negotiated, bounded, opaque evidence; keep inference interpretation and validator policy in LUCID |
| Model binding | `crates/lucidd/src/registry.rs:685-711` | Bind evidence and validation to exact model CID, backend, precision/quantization, and compatible capability class |
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

### Correctness-evidence seam

- [ ] Define a versioned evidence envelope with strict byte, item-count, decode, generation-time, retention, and validator-compute limits.
- [ ] Bind every evidence envelope to the job/manifest hash, exact model CID, backend and precision class, output commitment, serving PeerId, software/protocol version, and evidence method/version.
- [ ] Negotiate evidence-method compatibility before dispatch; unsupported or incompatible peers remain eligible only under the explicitly requested lower trust mode.
- [ ] Keep evidence generation inside the serving backend adapter and validation inside a separately selected LUCID validator. The serving peer cannot validate its own evidence for a higher trust label.
- [ ] Make validation deterministic where the method permits; otherwise record the approved tolerance, sampling basis, confidence, and known false-positive/false-negative behavior.
- [ ] Record validator PeerId, method/version, verdict, confidence/trust label, timing, and exact evidence commitment in the local evidence store.
- [ ] Classify pass, fail, disagreement, incomparable, unsupported, malformed, expired, and budget-exhausted outcomes separately.
- [ ] Prevent raw prompts, generated tokens, complete activations, model weights, secrets, or unbounded backend traces from entering persistent evidence records.
- [ ] Bound validator fan-out and make selection resistant to requester-controlled amplification and trivial worker/validator collusion.
- [ ] Preserve the signed alias/provider and verified CID pull paths as the only model-revision distribution mechanism; validators do not introduce a second artifact channel.

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
- [ ] Receipt/API language identifies transcript integrity separately from computation-correctness evidence and exposes the selected trust mode.
- [ ] Correctness evidence is versioned, bounded, exactly bound, independently validated, and rejected or downgraded safely when unsupported or incomparable.
- [ ] A validator cannot award its own serving execution a higher trust label, and validator fan-out remains within approved compute/amplification bounds.
- [ ] Resource amplification stays within approved bounds.
- [ ] Sybil, replay, poisoned-evidence, and mass-network-failure tests pass.
- [ ] Default authorization changes only if the separate recorded capability trigger passes.
- [ ] Workspace and multi-peer QA pass.

## Explicit non-goals

- A blockchain, token staking, payment-weighted trust, or global consensus.
- Claiming that reputation proves computation correct.
- Claiming that a valid receipt, output hash, redundant agreement, or worker-generated evidence alone proves correct model execution.
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
- No backend-derived correctness-evidence envelope or independent validator is implemented at this checkpoint. That seam is an approved upgrade target, not a shipped trust claim.
- Remaining before completion: physical multi-peer divergence demo, load/amplification measurement, network-partition behavior, larger Sybil/collusion campaign, version recovery, and an approved reputation/redundancy ADR and threat model.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
