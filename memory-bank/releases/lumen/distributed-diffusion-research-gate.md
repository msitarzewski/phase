# Milestone: Distributed Diffusion Research Gate

**Status:** ⬜ Not started
**Blocking:** No for the first LUMEN release unless explicitly promoted
**Implementation authorized:** No
**Depends on:** Intended Image Flow and the shared trust/sharding research
**Tracker:** [README.md](./README.md)

## Outcome

Determine whether any multi-peer diffusion strategy is technically useful and honestly verifiable enough to justify a future implementation plan. Do not add distributed denoise, conditioning, or model sharding to the production release merely because upstream code can split work.

The core risk is already recorded at `memory-bank/decisions.md:890-914`: intermediate floating-point work is expensive to verify, and honest cross-hardware outputs may differ. This gate shares methodology with `../lucid-v0.2/shardworker.md` but must study diffusion-specific partitioning, artifacts, and perceptual/numeric comparison.

## Candidate strategies to evaluate

- Pipeline/model partitioning across devices.
- Denoise-step partitioning or handoff.
- Conditioning branch parallelism, such as guidance branches where applicable.
- Batch/image-level parallelism, which may be independent jobs rather than partial computation.
- Tiled/region generation where seams and context make correctness measurable.
- Redundant full-job execution for verification/reliability.

The study must distinguish embarrassingly parallel independent images from true partial computation. Independent batch routing may be safely useful without solving shard verification.

## Required questions

- What workload is split, at what boundary, and what tensor/state crosses peers?
- Does the strategy reduce time-to-image or only aggregate throughput?
- What bandwidth/latency makes it beneficial versus one GPU?
- How are exact model/bundle, scheduler, seed, precision, and backend versions bound?
- What output differences occur across identical and heterogeneous devices?
- Can intermediate results be compared numerically; can final images be compared meaningfully without accepting malicious semantic changes?
- What threat model is addressed by redundancy, reputation, TEE, or later ZK work?
- How does cancellation/failure recovery affect the entire denoise trajectory?
- What privacy is exposed in latent/tensor/conditioning exchange?
- Is a trusted-cluster feature useful even if open-untrusted execution is not?

## Research work packages

### Baseline and partition study

- [ ] Reproduce the selected local backend and canonical job measurements.
- [ ] Prototype candidate splits in a controlled trusted environment without production integration.
- [ ] Record topology, partition plan, tensor sizes, bandwidth, latency, memory, synchronization, and failure behavior.
- [ ] Compare against one-device and independent batch baselines.
- [ ] Identify strategies that are net-negative outside LAN/high-bandwidth environments.

### Nondeterminism characterization

- [ ] Repeat same seed/model/backend/device and quantify intermediate/final variance.
- [ ] Repeat across supported hardware/backends/precision.
- [ ] Measure numeric tensor distance and image-level metrics while retaining awareness that perceptual similarity is not correctness proof.
- [ ] Inject controlled malicious deviations: stale tensor, skipped steps, altered conditioning, noise/latent perturbation, wrong model/LoRA, and semantically targeted changes.
- [ ] Measure false positive/negative behavior of proposed comparisons.

### Verification experiment

- [ ] Test redundant partial/full execution and random trusted spot checks.
- [ ] Bind every experiment to exact artifact CIDs and signed partition plans.
- [ ] Measure compute multiplier, detection probability, bandwidth, and latency.
- [ ] Evaluate collusion and Sybil limitations.
- [ ] Determine client-visible trust labels that do not overclaim.

### Decision

- [ ] Classify each strategy as independent-job safe, trusted-cluster only, experimental reputation-checked, future research, or rejected.
- [ ] Record a GO/DEFER/REJECT ADR with guarantees, costs, limitations, and re-entry conditions.
- [ ] If GO, prepare a separate implementation plan with explicit approval; this research milestone does not authorize production code.

## Acceptance criteria

- [ ] At least the most promising true-partial and independent-batch strategies are benchmarked against local baseline.
- [ ] Cross-hardware nondeterminism and adversarial deviations are measured.
- [ ] Verification cost/detection claims are quantitative and reproducible.
- [ ] Independent job-level parallelism is not conflated with verified partial computation.
- [ ] Privacy, bandwidth, failure, collusion, and Sybil limitations are documented.
- [ ] Approved ADR records go/defer/reject and exact client trust language.
- [ ] Public LUMEN roadmap is corrected if the result is defer/reject.

## Explicit non-goals

- Shipping distributed diffusion from a successful trusted LAN demo alone.
- Treating image similarity metrics as cryptographic proof.
- Reusing LUCID tensor/shard schemas without diffusion-specific evidence.
- Hiding redundancy cost or accepting colluding peers as verified.
- Making the first LUMEN release wait on unresolved research unless scope is explicitly changed.

## Completion evidence required in the tracker

- Benchmark and nondeterminism reports with methodology.
- Adversarial-injection and verification-cost results.
- Approved go/defer/reject ADR.
- If GO: separately approved implementation proposal.
