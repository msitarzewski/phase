# Milestone: ShardWorker

**Status:** 🟨 Blocked — partial-compute verification research/ADR unresolved; no production worker exists
**Track:** Trust and scale — research-gated
**Blocking:** Yes, but its honest v0.2 outcome may be an approved deferral
**Depends on:** Reputation and Redundant Verification
**Tracker:** [README.md](./README.md)

## Outcome

Resolve the partial-compute verification decision, then implement only the ShardWorker capability justified by that decision. A successful v0.2 outcome is either:

1. An explicitly experimental sharded-inference path whose trust boundary, comparison tolerance, failure recovery, and receipt semantics pass the approved gate; or
2. An approved, evidence-backed deferral that preserves a tested trusted-cluster prototype/research artifact without claiming untrusted sharding is verified.

The open problem is documented at `memory-bank/decisions.md:890-914`: a peer computing intermediate tensors cannot be cheaply verified by replaying public output chunks. The `ShardWorker` name and nominative relationship to exo-style clusters are established at `memory-bank/decisions.md:918-934`.

## Hard stop

No production-untrusted ShardWorker implementation begins before an ADR answers:

- What result is being verified: layer output, shard completion, final output, or peer behavior over time?
- Which threat model is in scope: crash/fault, opportunistic cheating, collusion, poisoned model shard, or malicious scheduler?
- What redundant execution or spot-check rate gives what detection probability and cost?
- How are floating-point differences normalized and bounded across Metal/CUDA/ROCm/Vulkan/CPU?
- Who holds full model/shard reference state for checks?
- How do receipts compose across peers and bind the exact model CID, partition plan, tensor boundaries, ordering, and final output?
- When is a run rejected, retried, degraded to a trusted cluster, or accepted with an explicit trust label?

If these cannot be answered honestly, the milestone stops at research/prototype and records the deferral.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Generic worker | `crates/phase-protocol/src/worker.rs:55-88` | Implement `Worker`; do not add a separate execution abstraction |
| Job spec | `crates/phase-protocol/src/job_spec.rs:35-81` | Add/version a shard-capable spec only through an approved protocol decision |
| Output receipts | `crates/phase-protocol/src/job_spec.rs:267-312`, `crates/phase-protocol/src/commitment.rs:29-86` | Preserve final output commitment; add partial evidence without misusing it as proof |
| Router | `crates/lucidd/src/router.rs:160-430` | Add a route/plan mode inside LUCID after capability and trust checks |
| Registry | `crates/lucidd/src/registry.rs:168-343`, `crates/lucidd/src/registry.rs:391-635` | Advertise shard capability, model partition, memory, interconnect, and protocol version through signed records |
| Reputation | `reputation-and-redundant-verification.md` | Consume attributable partial/final evidence and spot-check results |
| Backend | `crates/lucidd/src/worker_llama.rs:464-472` | Mirror worker lifecycle, cancellation, capacity, and signed receipt conventions |

## Research gate

### Baseline characterization

- [ ] Reproduce a supported upstream sharded-inference path on controlled hardware.
- [ ] Record partition schemes, supported architectures/backends, tensor transport format, memory/latency/bandwidth requirements, cancellation, and failure behavior.
- [ ] Verify upstream license directly before integration and document attribution.
- [ ] Keep “exo” only as nominative compatibility language, never as a Phase component name.

### Determinism study

- [ ] Run identical shard boundaries across same and different hardware/backend versions.
- [ ] Measure bitwise and numeric divergence at intermediate tensors and final tokens.
- [ ] Identify where deterministic seeds do and do not constrain outcomes.
- [ ] Define candidate comparison metrics/tolerances and their false-positive/false-negative behavior.
- [ ] Publish raw methodology and summarized results without proprietary/model-license-violating artifacts.

### Verification experiment

- [ ] Implement a test harness for redundant shard execution and trusted spot checks.
- [ ] Inject skipped layers, stale tensor replay, random perturbation, wrong model shard, reordered steps, truncated tensors, and colluding agreement.
- [ ] Measure detection probability, compute multiplier, bandwidth, and latency.
- [ ] Decide whether the result is adequate for experimental public use, trusted clusters only, or deferral.
- [ ] Record the approved ADR before implementation proceeds.

## Conditional implementation work packages

These packages activate only if the research gate permits them.

### Partition plan

- [ ] Define a canonical signed plan containing model/bundle CID, architecture/revision, ordered shard ranges, tensor schema, precision, participants, deadlines, and verification mode.
- [ ] Ensure every participant verifies the plan and relevant artifact CIDs before allocation.
- [ ] Bind plan hash into every shard receipt/evidence record.
- [ ] Reject incompatible partition/version/capability combinations before execution.

### Capability and scheduling

- [ ] Advertise memory, supported backend/precision, model/shard availability, bandwidth class, and concurrency without exposing sensitive host detail.
- [ ] Select participants using content availability, reachability, capacity, policy, reputation, and interconnect requirements.
- [ ] Reserve all required capacity before committing the client stream.
- [ ] Release the entire plan on partial reservation failure.
- [ ] Distinguish trusted-cluster, reputation-checked, and experimental-untrusted plans in client-visible metadata.

### Tensor transport

- [ ] Version and bound tensor frames independently from user-output `JobEvent`s.
- [ ] Bind sender/receiver, plan, stage, sequence, shape, dtype, size, and content checksum.
- [ ] Apply backpressure and hard per-plan memory/bandwidth limits.
- [ ] Encrypt/authenticate through the existing libp2p identity/transport.
- [ ] Cancel downstream stages when upstream fails and prevent stale tensor reuse.

### Execution and recovery

- [ ] Implement the worker/adapter without leaking backend-specific tensors into generic Phase APIs beyond approved opaque/versioned payloads.
- [ ] Propagate cancellation across every participant.
- [ ] Define which failures can retry a shard and which require full restart to preserve state.
- [ ] Prevent duplicated retries from concurrently mutating the same plan state.
- [ ] Produce terminal client output and signed receipt only after the plan’s approved verification procedure completes.

### Partial evidence and reputation

- [ ] Produce attributable per-stage evidence bound to the partition plan.
- [ ] Run approved redundant/spot checks at bounded rates.
- [ ] Record disagreement without overstating the guilty party.
- [ ] Quarantine cryptographically invalid evidence immediately under policy.
- [ ] Feed only approved objective outcomes into reputation.

## Test matrix

- Same-backend two-device partition.
- Cross-backend/heterogeneous partition if supported by the chosen upstream path.
- Relay path and direct path.
- Participant loss before reservation, during load, during tensor transfer, and after final stage.
- Slow participant/backpressure and scheduler timeout.
- Model/shard CID mismatch.
- Wrong shape/dtype, reordered/duplicate/stale tensor, and plan-hash mismatch.
- Honest floating-point divergence near the tolerance boundary.
- Injected malicious deviations from the research harness.
- Cancellation and cleanup across every participant.
- Colluding redundant peers demonstrating the limit of the chosen trust model.

## Acceptance criteria

### Research gate — always required

- [ ] Determinism and adversarial-injection report is reproducible.
- [ ] Verification ADR states guarantees, probabilities, costs, exclusions, and client-visible trust labels.
- [ ] Upstream license and trademark language are verified.
- [ ] Go, trusted-cluster-only, experimental, or defer decision is explicitly approved.

### If implementation proceeds

- [ ] A model that does not fit one participating device completes through at least two devices under the approved trust mode.
- [ ] Partition plan and every stage are bound to exact model/shard CIDs and peer identities.
- [ ] Cancellation/failure releases all reservations and resources.
- [ ] Injected deviations meet the measured detection behavior claimed by the ADR.
- [ ] Client/API clearly labels trust mode and never says “verified” beyond the evidence.
- [ ] Performance, bandwidth, redundancy multiplier, and failure-recovery costs are published.

### If deferred

- [ ] Tracker marks the milestone ⏭️ only after explicit approval.
- [ ] The research report, trusted prototype, open blockers, and re-entry criteria are preserved.
- [ ] Public roadmap/README is corrected so it does not imply production sharding shipped.
- [ ] Release Qualification treats the approved deferral as honest scope, not a silent omission.

## Explicit non-goals

- Claiming cryptographic verification from hashes of unverified intermediate tensors.
- Hiding hardware nondeterminism with an arbitrary tolerance.
- Forking or rebranding an upstream cluster project as Phase-owned technology.
- Flipping open authorization merely because a trusted-cluster demo works.
- Making LUMEN’s distributed diffusion depend on unproven LUCID sharding semantics.

## Completion evidence required in the tracker

- Research report, raw methodology, and approved verification ADR.
- Upstream license/trademark verification.
- Prototype/demo and adversarial-injection evidence, or approved deferral record.
- Resource/performance measurements and trust-label examples.
- Exact commit/PR and task documentation.

## 2026-08-09 blocker checkpoint

- No `ShardWorker`, tensor transport, partition scheduler, or production sharding claim was added.
- The new content, receipt, evidence, reputation, and bounded redundant-execution primitives are prerequisites for the research harness, not proof that partial tensor computation is correct.
- The existing open-problem record at `memory-bank/decisions.md#2026-05-29-verifying-sharded--partial-computation-is-an-unsolved-open-problem-recorded-risk` remains controlling.
- Unblock only after the determinism/adversarial-injection study and verification ADR specify measurable guarantees, costs, exclusions, and client-visible trust labels. Until then authorization remains default-deny and the public release must not imply sharding shipped.
- Related implementation checkpoint: [2026-08-09 task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
