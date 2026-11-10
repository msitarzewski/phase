# Milestone: Local Diffusion Worker

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Protocol Contract and Artifact Plane
**Tracker:** [README.md](./README.md)

## Outcome

An independent LUMEN daemon executes the canonical diffusion job through one real local backend, streams bounded progress/previews, atomically commits final artifacts, handles cancellation/crash/resource limits, and signs a receipt with the node’s Phase identity.

The implementation should mirror the lifecycle discipline of `LlamaCppWorker`—lazy load, health checks, bounded resident resources, cancellation, crash supervision, output commitment, and node-identity receipts—without reusing LUCID code directly (`crates/lucidd/src/worker_llama.rs:1-51`, `crates/lucidd/src/worker_llama.rs:194-220`, `crates/lucidd/src/worker_llama.rs:464-472`). Shared behavior may move downward only after reuse analysis proves it is workload-neutral.

## New-component justification

LUMEN requires its own application crate/daemon because the accepted ADR rejects diffusion as a LUCID mode (`memory-bank/decisions.md:854-884`). At implementation planning time, the proposed `crates/lumend/` or owner-approved equivalent must be validated against existing workspace naming, license, binary, and split-repository plans before creation. It depends on `phase-*` crates, not `lucidd`.

## Required backend decisions

- Selected backend, version, license, process boundary, and supported platforms.
- Supported model/bundle formats and feature subset.
- Health/readiness protocol and structured progress/output frames.
- GPU device selection and operator configuration.
- Model load/cache/eviction and concurrent-job policy.
- Resolution/pixel/steps/batch/duration/VRAM/disk/output caps.
- Seed behavior and nondeterminism disclosure.
- Crash restart budget, quarantine, and cleanup.
- Preview encoder/rate and final output encoder/metadata.

## Work packages

### Daemon and configuration

- [ ] Create the independent application crate only after approved reuse/file plan.
- [ ] Keep binary entrypoint thin; configuration, worker, API, routing, and policy remain testable modules.
- [ ] Load the existing `phase-identity` persistent key and derive the same libp2p PeerId/receipt signer.
- [ ] Validate config at startup: backend path/version, artifact store, bind addresses, device selection, limits, and policy.
- [ ] Fail closed on insecure permissions, missing identity, invalid backend, or impossible resource limits.

### Worker implementation

- [ ] Implement the unchanged Phase `Worker` trait.
- [ ] Advertise only the approved image-generation job kind.
- [ ] Verify signed manifest and all referenced artifact CIDs before backend allocation.
- [ ] Translate the approved job spec into backend-native request/workflow in a controlled deterministic builder.
- [ ] Never execute arbitrary client-provided plugins, scripts, shell commands, or local paths.
- [ ] Emit bounded progress/previews and exactly one terminal event.
- [ ] Commit final output artifact(s) before emitting committed artifact references.
- [ ] Fold committed output chunks/references into the existing accumulator and sign the receipt with node identity.

### Backend lifecycle

- [ ] Probe backend version/capabilities and health.
- [ ] Start lazily or at daemon startup according to the approved measured policy.
- [ ] Bound resident models, queues, concurrent jobs, and backend processes.
- [ ] Track model last-use and unload safely under memory pressure.
- [ ] Supervise crashes/hangs with capped retry/backoff and quarantine.
- [ ] Capture stdout/stderr safely with truncation/redaction; no prompt/image data in operational logs.
- [ ] Ensure child processes are reaped on cancellation, eviction, shutdown, or daemon crash recovery.

### Resource governance

- [ ] Validate width×height and batch/count arithmetic without overflow.
- [ ] Clamp client-requested limits to operator caps.
- [ ] Reject jobs that cannot fit estimated memory rather than relying on OOM.
- [ ] Bound queue wait, model load, per-step idle, total runtime, preview bytes/rate, final output bytes, and disk staging.
- [ ] Integrate manual pause plus platform-aware battery/thermal/interactive-load policy where available.
- [ ] Reserve resources before accepting the job and release on every terminal path.

### Cancellation and failures

- [ ] Observe `JobHandle` cancellation promptly.
- [ ] Stop backend generation through a real backend mechanism or kill/restart the isolated job process if necessary.
- [ ] Mark partial preview/output semantics explicitly on cancellation.
- [ ] Never publish a final artifact reference for incomplete/unverified bytes.
- [ ] Produce a signed cancelled/error receipt where the existing contract permits.
- [ ] Recover next-job capability without leaking GPU memory, queue entries, temp files, or artifact pins.

### Output and provenance

- [ ] Store final outputs through the generic artifact plane.
- [ ] Bind model/bundle and relevant auxiliary CIDs, seed, dimensions, approved generation parameters, backend/version, and final artifact CID according to the protocol/receipt ADR.
- [ ] Distinguish signed facts from observational metrics.
- [ ] Strip or preserve embedded workflow/provenance metadata according to product policy.

## Test matrix

- Real text-to-image on designated hardware/backend/model.
- Every supported canonical option and every explicitly rejected option.
- Fixed seed repeated on same hardware/backend to characterize, not assume, determinism.
- Model load/unload/eviction, two models if supported, queue/capacity, and repeated long runs.
- Cancellation during queue, load, early denoise, late denoise, preview encode, final encode, artifact commit, and receipt signing.
- Backend missing, wrong version, malformed frame, hang, crash, crash loop, GPU reset/OOM, disk full, corrupt bundle/input, and output encode failure.
- Oversized resolution/steps/batch/upload and arithmetic-overflow attempts.
- Receipt signature, PeerId, manifest, output commitment, and artifact CID verification.
- Graceful shutdown and restart with staged/active artifacts.

## Acceptance criteria

- [ ] Real backend produces a valid final image from the canonical job.
- [ ] Worker uses Phase `SignedManifest`, `JobEvent`, cancellation, commitment, and `SignedReceipt` contracts unchanged except for the approved job variant.
- [ ] Every referenced artifact is verified before use and every final output before publication.
- [ ] Progress/previews are live and bounded.
- [ ] Cancellation releases backend/GPU/disk resources.
- [ ] Resource caps and backend supervision prevent unbounded failure.
- [ ] Receipt verifies against the node PeerId and exact final artifact reference.
- [ ] No diffusion code is added to `lucidd`.
- [ ] Real hardware plus full workspace QA passes.

## Completion evidence required in the tracker

- Approved crate/reuse and backend ADRs.
- Hardware/backend/model identifiers and canonical generation recording.
- Resource/cancellation/crash evidence.
- Receipt and artifact-verification evidence.
- Exact commit/PR and task documentation.
