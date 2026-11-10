# Milestone: Client API and Preview Stream

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Product Contract and Local Diffusion Worker
**Tracker:** [README.md](./README.md)

## Outcome

The chosen diffusion-native client drives LUMEN through a faithful, bounded compatibility API. Requests translate into signed Phase jobs; progress and optional previews arrive live; cancellation reaches the worker; and final artifacts are returned/retrieved exactly as the client expects.

This is a product edge, not a protocol layer. Third-party API concepts remain in the LUMEN application just as Ollama translation remains inside LUCID. Generic Phase crates see only signed jobs, job events, and artifact references (`memory-bank/decisions.md:861-875`).

## API contract

The product ADR must define:

- Supported endpoint(s), methods, content types, auth, and version compatibility.
- Supported request fields and exact behavior for unsupported fields/nodes.
- Upload versus artifact-reference flows.
- Job submit, queue/status, progress/preview, cancel, final result, history/lookup, and artifact fetch behavior.
- Error mapping and retry semantics.
- Local/remote route disclosure and privacy warning.
- Limits and rate behavior visible to clients.

## Work packages

### Request translation

- [ ] Parse with strict body/field/depth/count/size limits.
- [ ] Validate client identifiers, filenames, paths, graph/node types, parameters, dimensions, steps, batch, and seeds.
- [ ] Reject arbitrary executable/custom plugin nodes unless individually approved and sandboxed.
- [ ] Convert uploads to verified content artifacts before creating the job manifest.
- [ ] Resolve human model aliases to verified bundle CIDs.
- [ ] Construct the approved LUMEN job spec; do not pass opaque unvalidated client graphs straight to a backend.
- [ ] Sign/submit through the same internal path used by remote routing.

### Progress and previews

- [ ] Map worker `ProgressUpdate` to the client’s expected queue/load/step state.
- [ ] Rate-limit/coalesce progress so denoise-step chatter cannot exhaust the API or relay.
- [ ] Encode previews at bounded resolution, quality, frequency, and total bytes.
- [ ] State explicitly whether previews are ephemeral informational frames or committed artifacts.
- [ ] Handle clients that do not consume previews without buffering indefinitely.
- [ ] Preserve ordered sequence and terminal uniqueness.

### Cancellation

- [ ] Map client cancel/disconnect to `JobHandle::cancel()`.
- [ ] Make repeated cancellation idempotent.
- [ ] Distinguish queued, running, finalizing, completed, cancelled, and failed states.
- [ ] Define whether completed final artifacts remain retained after late cancellation.
- [ ] Ensure cancellation cannot target another user/job through identifier guessing.

### Final results

- [ ] Return client-compatible result metadata and final artifact handle/URL.
- [ ] Authorize artifact retrieval according to the chosen local/remote API policy.
- [ ] Verify artifact CID before response and optionally on client fetch.
- [ ] Include receipt/provenance extension data without breaking the client’s schema.
- [ ] Support multiple ordered outputs if in the feature envelope.
- [ ] Never return a staging path or host filesystem location.

### API security

- [ ] Bind to safe defaults and require explicit configuration for remote control access.
- [ ] Define authentication separate from Phase peer identity for HTTP clients.
- [ ] Apply request, upload, concurrency, queue, and IP/client rate limits.
- [ ] Prevent SSRF: no arbitrary backend/provider URL fetch from client fields.
- [ ] Prevent path traversal and command/template injection.
- [ ] Sanitize logs and errors; prompts and input images are not operational telemetry.
- [ ] Define CORS/CSRF behavior if browser clients are supported.

## Client test matrix

- Chosen unmodified client/version canonical flow.
- Direct API/curl fixture for deterministic regression coverage.
- Supported and unsupported feature cases.
- Local and remote worker routes.
- Upload/reference, progress, previews enabled/disabled, cancel, multi-output if supported, and final fetch.
- Client disconnect, reconnect/status lookup if supported, malformed/oversized request, unsupported node, invalid artifact, queue full, policy refusal, worker crash, and artifact fetch failure.
- Slow/no-preview consumer under bounded memory.
- API auth, cross-job cancellation/access, CORS/CSRF where relevant, SSRF/path/injection fuzzing.

## Acceptance criteria

- [ ] The designated unmodified client completes the canonical generation flow.
- [ ] API claims match the supported subset; unsupported features fail clearly.
- [ ] Progress and previews arrive before final completion and stay inside approved bounds.
- [ ] Cancellation reaches the real backend and frees capacity.
- [ ] Final artifact fetch returns bytes matching the receipt-bound CID.
- [ ] Local and remote route behavior is visible and privacy disclosure is accurate.
- [ ] HTTP access controls and resource limits pass abuse testing.
- [ ] No client/backend-specific assumptions leak into `phase-*` crates.

## Explicit non-goals

- Cloning an entire third-party UI/backend platform.
- Supporting arbitrary plugin execution for compatibility.
- Using Ollama merely because its server already exists in LUCID.
- Treating previews as final verified output unless explicitly committed.
- Exposing raw local paths to clients or peers.

## Completion evidence required in the tracker

- Approved API compatibility contract.
- Unmodified-client recording and exact version/configuration.
- Preview/cancellation/backpressure measurements.
- HTTP security QA report.
- Exact commit/PR and task documentation.
