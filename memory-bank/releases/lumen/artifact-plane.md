# Milestone: Artifact Plane

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Product and Protocol Contracts
**Tracker:** [README.md](./README.md)

## Outcome

LUMEN resolves, verifies, stores, serves, and records provenance for every model, auxiliary weight, input, preview retained as an artifact, and final image/video output through the generic Phase content plane. No worker executes an unverified artifact and no final result is reported before atomic storage.

Phase already has a generic content-addressed blob namespace at `crates/phase-artifact-server/src/dht.rs:93-101`, a content store at `crates/phase-artifact-server/src/artifacts.rs:87-109` and `crates/phase-artifact-server/src/artifacts.rs:278-323`, range-capable streaming at `crates/phase-artifact-server/src/server.rs:356-477`, and a CID-validating blob route at `crates/phase-artifact-server/src/server.rs:510-545`. LUMEN should exercise and extend these components, not create a diffusion-only file server.

## Relationship to LUCID v0.2

Reuse generic decisions from `../lucid-v0.2/content-plane.md` for hashing, chunking/Merkle roots, provider identity, resumable transfer, atomic install, and local storage. LUMEN adds workload-specific bundle and provenance schemas but must not fork generic CID semantics.

## Artifact classes

The approved schema must distinguish:

- Base model/checkpoint.
- Text encoder(s), VAE, tokenizer/configuration.
- LoRA/adapters, embeddings, ControlNet or other optional auxiliaries.
- Input image, mask, control/reference image.
- Intermediate preview if retained.
- Final image(s), metadata sidecar, and future video artifact.
- Bundle manifest describing exact compatible members and backend requirements.

## Required artifact ADR

- [ ] Generic CID/chunk representation reused from LUCID Content Plane.
- [ ] Canonical LUMEN bundle manifest and member roles.
- [ ] Format/MIME identifiers, dimensions, precision/quantization, model architecture/revision, and backend compatibility.
- [ ] Publisher signature, source/provenance, license metadata, and mutable alias mapping.
- [ ] Input privacy/retention defaults and output ownership/retention behavior.
- [ ] Preview retention: ephemeral versus stored content.
- [ ] Garbage collection roots, pinning, quotas, deduplication, and reference counts.
- [ ] Metadata stripping/preservation policy for EXIF and embedded workflow data.

## Work packages

### Bundle manifests

- [ ] Define canonical signed manifest bytes and validation.
- [ ] Require exact member CIDs, roles, sizes, formats, and compatibility constraints.
- [ ] Reject duplicate/conflicting roles and dependency cycles.
- [ ] Support optional auxiliaries without making bundles ambiguous.
- [ ] Record publisher/provenance/license fields without claiming the protocol adjudicates their truth.

### Acquisition and installation

- [ ] Reuse resumable, multi-provider content transfer.
- [ ] Stage and verify every member before exposing the bundle to a worker.
- [ ] Commit a bundle atomically only when required members and compatibility checks pass.
- [ ] Bound total bundle/member size, concurrent transfers, disk quota, and idle/total time.
- [ ] Resume safely after daemon restart and provider switching.
- [ ] Never fetch arbitrary job-supplied URLs or filesystem paths.

### Input ingestion

- [ ] Validate media type, dimensions, decoded pixel count, file size, and parser limits before backend use.
- [ ] Decode in a bounded/process-isolated path if the selected libraries warrant it.
- [ ] Strip or preserve metadata according to explicit policy.
- [ ] Store only with the configured retention/pin policy.
- [ ] Make remote input visibility and retention clear to users.

### Final output commit

- [ ] Encode final output deterministically enough to hash the exact returned bytes; do not claim cross-backend image determinism.
- [ ] Validate output media and metadata before storage.
- [ ] Atomically add output blob(s), then emit committed artifact-reference chunk(s).
- [ ] Bind content CID, dimensions, MIME type, and ordinal to the job receipt.
- [ ] Support range fetch and exact CID verification by the client.
- [ ] Define cleanup if receipt signing fails after artifact commit.

### Store operations

- [ ] Pin active models and in-flight inputs/outputs.
- [ ] Protect referenced data from garbage collection.
- [ ] Implement quota/eviction ordering that never removes active artifacts.
- [ ] Expose store usage and artifact state without leaking private file paths/content.
- [ ] Recover indexes from content-addressed disk state after crash/corruption.

## Security and failure tests

- [ ] Tampered member, manifest, publisher signature, alias, and final output are rejected.
- [ ] Path traversal, archive bomb if archives are permitted, image decompression bomb, malformed media, oversized dimensions, metadata bomb, and parser crash are bounded.
- [ ] Incomplete bundle cannot become loadable.
- [ ] Concurrent install of the same bundle converges without corruption.
- [ ] Disk full during member download, atomic commit, output encode, output store, and receipt signing leaves consistent state.
- [ ] Garbage collection races with active job/input/output are prevented.
- [ ] Input/output retention and deletion behavior is tested.
- [ ] Cross-node fetch verifies exact final output CID.

## Acceptance criteria

- [ ] Canonical local job uses only verified bundle and input artifacts.
- [ ] Final output is committed atomically and independently verifiable by CID.
- [ ] A second Phase node fetches the output through range serving and recomputes the same CID.
- [ ] Bundle acquisition resumes across interruption/provider switch.
- [ ] Malicious media and resource-amplification tests remain bounded.
- [ ] GC/quota/restart tests preserve active and pinned content.
- [ ] Provenance/license/retention metadata is visible and honest.
- [ ] No LUMEN-specific duplicate artifact server or DHT client is created.

## Completion evidence required in the tracker

- Approved bundle/provenance/retention ADR.
- Multi-member pull/resume/tamper evidence.
- Cross-node final artifact retrieval/CID verification.
- Media-parser and quota/GC failure QA.
- Exact commit/PR and task documentation.
