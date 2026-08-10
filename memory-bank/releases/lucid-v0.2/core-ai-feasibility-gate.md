# Milestone: Core AI Feasibility Gate

**Status:** ⬜ Not started
**Track:** Backend research — non-blocking
**Implementation authorized:** No
**Depends on:** MLX findings and current public Core AI documentation/tooling
**Tracker:** [README.md](./README.md)

## Outcome

Produce an evidence-backed go/no-go decision for a future `CoreAIWorker` that runs project-selected open-weight models on Apple’s Neural Engine. This milestone does not implement a production backend and does not authorize one merely because a forward pass works.

The accepted boundary is recorded at `memory-bank/decisions.md:938-953` and the existing research at `memory-bank/research/2026-06-09-apple-coreai-foundation-models.md:48-85`: Core AI may be a legitimate runtime for project-controlled weights, while Apple Foundation Models is explicitly prohibited as a LUCID backend. The make-or-break question is public stateful autoregressive decode/KV-cache support.

## Hard guardrails

- Never evaluate Apple Foundation Models as an implementation fallback.
- Use only models/weights the project is permitted to convert and run.
- Do not create `CoreAIWorker`, a new crate, or production bridge code during this gate without a separately approved implementation plan.
- Do not call one-shot forward-pass success equivalent to a viable streaming LLM backend.
- Do not claim Neural Engine use without verifiable runtime/profiling evidence.

## Questions this gate must answer

### Stateful decode

- Does the public API expose persistent KV-cache or equivalent mutable model state across token steps?
- Can cache state be isolated across concurrent sessions and reclaimed deterministically?
- What is the prefill/decode API shape and copying overhead?
- Can a cancelled request release cache promptly?
- Does state survive only in process memory, or can it be serialized/resumed safely?

### Streaming and control

- Can tokens/logits be produced incrementally with bounded latency?
- Can execution be cancelled below the process boundary?
- Are per-request timeouts and concurrency enforceable?
- Can the adapter expose health, memory, ANE utilization, and structured errors?

### Model pipeline

- Which open model architectures and operations convert without unsupported fallbacks?
- Are tokenizer, sampling loop, RoPE, MoE, quantization, and cache orchestration provided or project-owned?
- How are converted model artifacts externalized, versioned, and content-addressed?
- Is conversion deterministic enough for reproducible bundle manifests?
- What licenses govern conversion tools, generated artifacts, and runtime redistribution?

### Systems integration

- What process boundary is supportable from Rust: Swift subprocess, XPC, C ABI, or another documented mechanism?
- What minimum OS/hardware/toolchain versions are required?
- Can the backend run unattended under the LUCID service model?
- How does energy/thermal behavior compare with MLX for sustained contributor workloads?
- Can errors and logs avoid exposing prompt/model content?

## Evidence plan

### Documentation review

- [ ] Read primary public Core AI API and toolchain documentation current at execution time.
- [ ] Capture exact API symbols and version availability for state, streaming, cancellation, cache, compilation, profiling, and model loading.
- [ ] Verify license/redistribution terms from primary sources.
- [ ] Update the existing research report rather than creating a competing general overview, unless the approved task specifically requires a new dated research artifact.

### Minimal technical probe

- [ ] Convert or obtain a license-compatible tiny open model fixture.
- [ ] Run prefill plus at least 32 iterative decode steps while measuring whether state is reused.
- [ ] Compare token-step cost with and without cache reuse to prove cache behavior.
- [ ] Run two isolated sessions and cancellation.
- [ ] Profile compute target and memory; record whether unsupported operations fall back to CPU/GPU.
- [ ] Keep the probe disposable and clearly non-production.

### MLX comparison

- [ ] Use the same hardware and, where technically possible, the same model family and precision.
- [ ] Compare setup complexity, first-token latency, decode throughput, memory, power/thermal, cancellation, concurrency, and unattended stability.
- [ ] Separate peak throughput from contributor-efficiency conclusions.
- [ ] Identify any model-format implications for the Content Plane.

## Decision rubric

### GO for a later implementation plan only if

- Public APIs support effective stateful autoregressive decode or a bounded, supportable cache design.
- Streaming, cancellation, resource cleanup, and session isolation are demonstrable.
- Model conversion and artifact licensing are acceptable.
- The Rust-to-Apple process boundary can be packaged and operated reliably.
- ANE execution is confirmed and offers a meaningful contributor-efficiency profile beyond MLX.
- Content Plane can represent the artifact bundle without weakening verification.

### NO-GO / defer if any blocker remains

- KV-cache/stateful decode is unavailable, undocumented, or requires rebuilding full context every token.
- Required operations silently fall back such that the claimed ANE profile is false.
- Cancellation/resource isolation is not controllable.
- Toolchain/runtime terms conflict with neutral peer compute.
- Model conversion cannot be reproduced or safely content-addressed.
- Integration cost has no measured benefit over MLX.

## Acceptance criteria

- [ ] Every question above has evidence, an explicit unknown, or a reason it is irrelevant.
- [ ] Stateful decode is proved with a measured probe or recorded as the blocking absence.
- [ ] Compute target and fallback behavior are profiled.
- [ ] MLX comparison uses the same documented hardware context.
- [ ] Licensing and redistribution are reviewed from primary sources.
- [ ] Content/bundle implications are fed back to the Content Plane decision.
- [ ] An approved ADR records GO, DEFER, or REJECT plus re-entry conditions.
- [ ] No production backend code is presented as complete from this research milestone.

## Completion evidence required in the tracker

- Updated research/primary-source matrix.
- Reproducible technical probe and measurements.
- MLX comparison.
- Approved go/defer/reject ADR.
- If GO: a separately proposed implementation task; not an implicit expansion of this milestone.
