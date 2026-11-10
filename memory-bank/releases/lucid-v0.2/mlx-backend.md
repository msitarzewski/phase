# Milestone: MLX Backend

**Status:** 🟦 In progress — worker implementation approved; real Apple Silicon acceptance pending
**Track:** Backend — parallel
**Blocking:** Yes for the full release; not required for the first intended-stream checkpoint
**Depends on:** Content Plane model-format metadata and Apple Silicon test hardware
**Tracker:** [README.md](./README.md)

## Outcome

LUCID serves inference and supported embeddings on Apple Silicon through an MLX-backed `Worker`, using the same `JobSpec`, `JobEvent`, cancellation, commitment, receipt, registry, router, policy, and Ollama API semantics as `LlamaCppWorker`.

MLX was deferred from the first LUCID release at `memory-bank/releases/lucid/index.yaml:83-92`. The implementation must extend the existing worker pattern in `crates/lucidd/src/worker_llama.rs:76-144` and `crates/lucidd/src/worker_llama.rs:464-472`; it must not introduce an MLX-specific API or duplicate router.

## Required backend ADR

Approve a decision covering:

- Process boundary: supported `mlx-lm` server/CLI mode, Python subprocess, Swift bridge, or another measured adapter.
- Supported MLX/model artifact formats and their relationship to Content Plane bundle CIDs.
- Minimum macOS/Apple Silicon/MLX versions and dependency installation strategy.
- Streaming protocol from the backend adapter, health checks, startup, shutdown, crash recovery, and version detection.
- Concurrency/capacity, KV-cache/session handling, memory accounting, and eviction.
- Packaging/licensing/attribution for MLX and model conversion tools.
- Feature detection and behavior on non-Apple platforms.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Worker contract | `crates/phase-protocol/src/worker.rs:55-88` | Implement `Worker` with the same lifecycle |
| Job semantics | `crates/phase-protocol/src/job_spec.rs:110-182` | Consume existing inference/embedding specs |
| Output and receipts | `crates/phase-protocol/src/worker.rs:315-378`, `crates/phase-protocol/src/commitment.rs:29-86` | Emit ordered chunks and sign the same result shape |
| Existing backend pattern | `crates/lucidd/src/worker_llama.rs:1-51`, `crates/lucidd/src/worker_llama.rs:194-220` | Reuse supervision, bounded resources, health, and error conventions where applicable |
| Router/API | `crates/lucidd/src/router.rs:160-430`, `crates/lucidd/src/ollama.rs:265-278` | Register a backend; do not fork request translation/routing |
| Registry | `crates/lucidd/src/registry.rs:168-343`, `crates/lucidd/src/registry.rs:391-635` | Advertise backend, exact content/bundle CID, capability, and capacity |
| Policy | `crates/lucidd/src/policy.rs:69-164`, `crates/lucidd/src/policy.rs:262-383` | Honor manual pause, thermal/battery, resource, authz, and prompt caps |

## Work packages

### Feasibility spike

- [ ] Pin and record an MLX/`mlx-lm` version on the designated Apple Silicon rig.
- [ ] Run a real open-weight model locally and capture startup, streaming, memory, throughput, power/thermal, cancellation, and failure behavior.
- [ ] Confirm inference and embedding interfaces separately; do not claim embeddings if unsupported by the chosen path.
- [ ] Confirm the adapter exposes enough lifecycle control for bounded server operation.
- [ ] Verify licenses for runtime, adapter, conversion code, and test models.

### Worker implementation

- [ ] Add `MlxWorker` beside `LlamaCppWorker` inside the existing LUCID crate unless reuse analysis at implementation time proves a separate crate is necessary.
- [ ] Implement supported kinds, capacity hint, manifest validation, model resolution, backend start/load, streaming, cancellation, final result, and signed receipt.
- [ ] Use the node identity for receipts so PeerId binding remains valid.
- [ ] Map backend output to `OutputChunk` kinds already understood by the API.
- [ ] Fold exactly emitted output into `CommitmentAccumulator`.
- [ ] Produce deterministic structured errors without leaking local paths or subprocess internals.

### Model lifecycle

- [ ] Resolve only verified Content Plane bundles compatible with MLX.
- [ ] Probe model metadata before expensive allocation.
- [ ] Bound resident models, memory, contexts/sessions, and concurrent work.
- [ ] Define LRU/unload behavior consistent with existing policy.
- [ ] Supervise backend health and cap crash restarts/backoff.
- [ ] Kill child/bridge processes and release sessions on cancellation, eviction, shutdown, and crash.

### Configuration and selection

- [ ] Add explicit backend configuration and validated adapter path.
- [ ] Detect Apple Silicon/macOS support; unavailable platforms fail at configuration/selection, not at request midstream.
- [ ] Define auto-selection versus operator selection. Do not silently move to a backend with different artifact requirements.
- [ ] Surface backend/version/capacity through status and registry without leaking sensitive host data.
- [ ] Keep default behavior backward compatible for existing llama.cpp operators.

### API compatibility

- [ ] `/api/chat` and `/api/generate` return the same Ollama-compatible shapes.
- [ ] Streaming and `stream:false` both work.
- [ ] Sampling parameters either map correctly or have documented ignored/unsupported behavior.
- [ ] Cancellation propagates from HTTP through the worker adapter.
- [ ] Embedding endpoints are enabled only after real vector shape/order/commitment tests.
- [ ] Terminal metadata truthfully identifies MLX and receipt verification state.

## Test matrix

- Unit tests for configuration, platform detection, command/bridge construction, frame parsing, error mapping, and artifact compatibility.
- Integration tests against a controlled local adapter fixture; test fixtures may simulate protocol frames but cannot substitute for hardware acceptance.
- Real Apple Silicon runs for chat, generate, optional embeddings, cancellation, crash/restart, load/unload, multiple models, memory pressure, battery/thermal pause, and remote relay.
- Cross-backend comparison using the same model family where formats permit; output equality is not required, but API/receipt invariants are.
- Non-Apple build/test proving unsupported code is gated cleanly.
- Content mismatch, wrong format, corrupt bundle, missing adapter, incompatible version, backend hang, malformed frame, and child crash.

## Acceptance criteria

- [ ] Real Apple Silicon inference produces live output through `MlxWorker`.
- [ ] Local and peer-routed MLX requests use the same router/API path as llama.cpp.
- [ ] CID/bundle format is verified before load.
- [ ] Output commitment and signed receipt pass existing consumer verification.
- [ ] Client cancellation frees backend resources.
- [ ] Crash-loop, memory, concurrent-work, and resident-model limits are enforced.
- [ ] Thermal/battery/manual-pause policy remains effective.
- [ ] At least one unmodified Ollama-compatible client works.
- [ ] Non-Apple platforms build and fail unsupported configuration cleanly.
- [ ] Performance/power measurements are recorded without making unsupported superiority claims.
- [ ] Workspace QA passes.

## Explicit non-goals

- Replacing llama.cpp as the universal/default backend without comparative evidence.
- Supporting every MLX-convertible architecture in the first milestone.
- Building a separate MLX registry or API.
- Calling a mocked adapter sufficient for completion.
- Implementing Core AI or Apple Foundation Models.

## Completion evidence required in the tracker

- Approved backend ADR and verified dependency licenses.
- Hardware/OS/runtime/model identifiers and performance/power report.
- Local and remote-stream recordings with receipt verification.
- Cancellation/crash/resource-limit QA.
- Exact commit/PR and task documentation.

## 2026-08-09 approved implementation checkpoint

- Added `MlxWorker` beside `LlamaCppWorker` in the existing LUCID crate, preserving the shared `Worker` lifecycle, router, API, commitment, node-identity receipt, policy, and registry paths (`crates/lucidd/src/worker_mlx.rs:100-1160`).
- The adapter hashes and pins a canonical bundle, rejects symlinks/hardlinks/writable or ambiguous entries, rechecks runtime/model bytes before spawn and request, binds loopback only, forces one model/capacity, disables remote code, bounds SSE/frame/body sizes, and invalidates/kills on cancellation, malformed output, EOF, or timeout.
- Tests cover canonical bundle identity, mutation after construction, runtime mutation, malicious/oversized/CRLF SSE, cancellation, hung streams, occupied ports, wrong model/kind, bounded recursive scan, signed output, and Linux fail-closed platform behavior.
- macOS ARM64 workspace QA and native Linux x86_64 unsupported-platform QA pass. Test fixtures validate the adapter contract but do not satisfy hardware acceptance.
- Remaining before completion: pin the chosen real MLX/`mlx-lm` version and license set, run a real open-weight model on Apple Silicon, validate local and peer routing/cancellation/crash/resource behavior, and record performance/power measurements.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
