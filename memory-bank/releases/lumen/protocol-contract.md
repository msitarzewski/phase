# Milestone: Protocol Contract

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Product Contract and Compatibility Wedge
**Tracker:** [README.md](./README.md)

## Outcome

Add the smallest workload-neutral Phase protocol extension needed for LUMEN jobs, progress/previews, final artifacts, cancellation, and signed receipts. The `Worker` trait itself remains unchanged.

`JobSpec` is explicitly `#[non_exhaustive]` and already lists image generation as a future variant at `crates/phase-protocol/src/job_spec.rs:35-54`. `JobEvent` and `OutputChunk` are already designed for image tiles and other workloads at `crates/phase-protocol/src/worker.rs:315-378`. This milestone extends those seams rather than creating a LUMEN-private transport contract.

## Protocol design decisions

- [ ] Job variant/kind names and protocol-version impact.
- [ ] Required and optional job fields with canonical serialization/defaults.
- [ ] Artifact references for models, auxiliary weights, input images, masks, control inputs, and outputs.
- [ ] Parameter representation: typed common fields versus controlled extension map.
- [ ] Preview representation and whether previews are committed output or informational progress.
- [ ] Final output representation: inline bytes are prohibited above a small bound; define artifact reference and metadata.
- [ ] Multi-image ordering and partial-success behavior.
- [ ] Cancellation and terminal receipt semantics.
- [ ] Signed manifest resource limits and worker-side clamping.
- [ ] Schema evolution and behavior for older peers.

## Proposed shape constraints

The exact schema requires ADR approval, but it must satisfy:

- Model and auxiliary artifacts are immutable content references, never arbitrary local paths or URLs.
- Input images/masks are content references or tightly bounded inline payloads under an approved threshold.
- Resolution, pixel count, steps, batch/count, duration, output bytes, and preview rate are explicit or clampable.
- Seed semantics distinguish random/unspecified from exact reproducibility requests.
- Backend-specific options cannot bypass common safety/resource limits.
- The job spec does not embed ComfyUI, A1111, or backend names into generic Phase primitives; LUMEN translates at its edge.

## Work packages

### Job spec and kind

- [ ] Add the approved job variant and `JobSpecKind` discriminator in `crates/phase-protocol/src/job_spec.rs:42-81`.
- [ ] Define typed, documented, serde-compatible fields.
- [ ] Update `JobSpec::kind()` exhaustively.
- [ ] Update crate docs/SPEC and compatibility notes.
- [ ] Add canonical serialization and manifest-signature test vectors.

### Events and output

- [ ] Reuse `ProgressUpdate` for queue/load/denoise progress when information is not output commitment material (`crates/phase-protocol/src/worker.rs:394-404`).
- [ ] Define approved stable chunk kinds only where a verifier/client must reconstruct committed output.
- [ ] Prefer a committed artifact-reference chunk for final outputs rather than sending multi-megabyte images through control/event frames.
- [ ] Specify canonical artifact-reference encoding, order, MIME/media metadata, dimensions, and content CID.
- [ ] Ensure `JobResult.output_commitment` and chunk count bind exactly what clients receive (`crates/phase-protocol/src/job_spec.rs:267-312`).

### Resource contract

- [ ] Add job-requested maxima/defaults needed by LUMEN without making the substrate interpret diffusion concepts.
- [ ] Define worker/operator caps as authoritative clamps.
- [ ] Reject multiplication overflow and impossible dimensions before allocation.
- [ ] Distinguish dispatch-time invalid manifest/capacity from in-flight backend errors through existing `WorkerError`/`Completion` contracts.

### Receipt and provenance

- [ ] Bind job-spec hash, committed output references, completion, duration, and worker identity through existing receipt types.
- [ ] Decide which model/auxiliary CIDs and backend/version metadata must be present in signed versus observational fields.
- [ ] Never treat backend-attested metrics as proof; `JobMetrics` is observational (`crates/phase-protocol/src/job_spec.rs:306-311`).
- [ ] Define partial/multiple output behavior on cancellation or backend error.

## Compatibility tests

- [ ] Canonical round-trip and signed-manifest vectors on all supported architectures.
- [ ] Older downstream exhaustive matches remain protected by `#[non_exhaustive]`.
- [ ] Unknown optional fields follow the approved compatibility policy.
- [ ] Unsupported job kinds are refused before expensive artifact fetch/load.
- [ ] Oversized inline artifacts, invalid dimensions, excessive steps/batches, invalid enum/options, and malformed artifact references fail deterministically.
- [ ] Output chunk ordering, duplicate/missing references, terminal uniqueness, cancellation, and receipt commitment tests.
- [ ] LUCID inference/embedding and Plasm WASM tests remain unchanged and passing.

## Acceptance criteria

- [ ] Approved protocol ADR and updated `phase-protocol/SPEC.md`.
- [ ] Existing `Worker` trait signature is unchanged.
- [ ] New job spec can represent the canonical LUMEN use case without backend/local-path leakage.
- [ ] Large final outputs are content-addressed artifacts, not unbounded relay frames.
- [ ] Preview/progress commitment semantics are explicit.
- [ ] Signed receipt binds job and final artifact reference(s).
- [ ] Cross-platform test vectors and full workspace QA pass.

## Explicit non-goals

- Encoding an entire third-party client ecosystem into Phase protocol.
- Adding diffusion logic to `phase-net`, `phase-identity`, or receipt cryptography.
- Claiming deterministic image equality across all hardware/backends.
- Defining distributed denoise/tensor protocols; that belongs to the research gate.

## Completion evidence required in the tracker

- Approved schema/compatibility ADR.
- Protocol diff and canonical test vectors.
- Proof all existing workloads remain passing.
- Resource-bound and malformed-input QA.
- Exact commit/PR and task documentation.
