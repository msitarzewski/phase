# Milestone: Release Qualification

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Every required LUMEN milestone
**Tracker:** [README.md](./README.md)

## Outcome

Prove that the first LUMEN release is an independent, secure, interoperable, operable, recoverable, license/trademark-reviewed Phase node whose product claims exactly match its demonstrated local and remote single-worker behavior.

## Entry criteria

- Product, protocol, artifact, worker, API, routing/policy, and intended-flow milestones are in review or complete.
- Product version, milestone IDs, license, and trademark decision are approved.
- Distributed diffusion remains explicitly research-only unless a separately approved scope change and verification ADR promote it.
- Source/dependency lock and supported client/backend/model matrix are frozen except for approved fixes.
- No unresolved critical/high security issue.

## Qualification matrix

### Independence and architecture

- [ ] LUMEN builds/runs as its own daemon/application boundary.
- [ ] `lucidd` has no diffusion endpoints, workers, model types, or application dependencies.
- [ ] LUMEN depends on `phase-*` substrate crates; any shared extraction is demonstrably workload-neutral.
- [ ] Existing Plasm and LUCID tests/behavior remain passing.
- [ ] New protocol variant does not change the `Worker` trait.

### Build and supply chain

- [ ] Format, full workspace tests, clippy `-D warnings`, dependency/security audit, and release builds.
- [ ] Supported platform/backend hardware build and smoke tests.
- [ ] SPDX, application license, backend/runtime/client integration, model/tool attribution, and bundle license review.
- [ ] Trademark/name approval and public attribution/notice.
- [ ] No credentials, private keys, raw private media/prompts, copyrighted model weights, or sensitive evidence committed.
- [ ] Release artifacts have checksums/signatures and source traceability.

### Client and API compatibility

- [ ] Exact approved unmodified client/version completes canonical local and remote flows.
- [ ] Every advertised supported feature passes; every unsupported feature fails clearly.
- [ ] Upload/reference, queue/status, progress, previews, cancellation, final result, and artifact fetch.
- [ ] HTTP authentication/bind/CORS/CSRF policy as applicable.
- [ ] Malformed, oversized, deeply nested, unsupported-node, injection, SSRF, path, and cross-job access tests.

### Artifact security and lifecycle

- [ ] Model/bundle/auxiliary/input/output CID and signature/provenance validation.
- [ ] Multi-provider resume, atomic install/commit, deduplication, pinning, quota, garbage collection, retention, and deletion.
- [ ] Malicious media, decompression/resource bombs, metadata limits, parser failures, and disk exhaustion.
- [ ] Cross-node final output fetch and independent CID recomputation.
- [ ] Receipt binds exact committed artifact reference(s).

### Worker and policy

- [ ] Real backend/model on supported hardware.
- [ ] Queue, load, preview, finalization, cancellation, error, and receipt lifecycle.
- [ ] Resolution/pixels, steps, batch/count, duration, input/output/preview bytes, concurrent jobs, memory/VRAM, and disk caps.
- [ ] Manual pause, local/self traffic, remote authorization, battery/thermal/interactive-load policy where supported.
- [ ] Backend missing/wrong version, hang, crash, crash loop, OOM, device loss, and graceful restart.
- [ ] No arbitrary client plugins/scripts/paths execute.

### Remote Phase path

- [ ] Signed capability advertisement, expiry, withdrawal, and invalid-record rejection.
- [ ] Different-network physical nodes, including relay-only reachability where required.
- [ ] Live progress/previews with bounded backpressure.
- [ ] Cancellation across the peer path.
- [ ] Manifest, signer/PeerId, event sequence, commitment/chunk count, receipt, and final artifact verification.
- [ ] Peer loss at every lifecycle boundary produces safe client state and cleanup.
- [ ] Relays remain workload-neutral and retain no raw job content under approved policy.

### Performance and recovery

- [ ] Clean/warm start, model pull/verify/load, queue, first preview, final output, receipt, and artifact fetch measurements.
- [ ] Local versus remote/relay overhead.
- [ ] Idle/loaded/repeated-job CPU/GPU/memory/VRAM/disk/network/power/thermal envelopes.
- [ ] Restart/recovery of API node, worker, content provider, relay, and interrupted transfers.
- [ ] Upgrade/rollback preserves identity, compatible config, and verified store.
- [ ] Thresholds derive from recorded baselines and regressions are investigated.

### Documentation and claims

- [ ] Consumer and operator clean-machine runbooks.
- [ ] Exact supported client/backend/model/feature/platform matrix.
- [ ] API, config, policy, artifacts, retention, privacy, security, troubleshooting, upgrade, rollback, and removal.
- [ ] Remote prompt/input visibility disclosed.
- [ ] Provenance and receipts described without claiming universal authenticity/correctness.
- [ ] Distributed diffusion clearly labeled research-only unless separately approved.
- [ ] LUMEN’s separate relationship to LUCID and shared Phase substrate is clear.

## Release blockers

- Any unverified model/input/output reaches execution or a successful response.
- Arbitrary custom node/plugin/script/path execution through the compatibility API.
- Unbounded image decode, resolution, preview, output, queue, transfer, or GPU resource growth.
- Final artifact reported before atomic commit/CID verification.
- Remote success without receipt/PeerId/manifest/artifact binding.
- Cancellation or failure leaks GPU work, process, queue, temp artifact, or pin.
- Product depends on `lucidd` or embeds diffusion into LUCID.
- License/trademark uncertainty remains unresolved.
- Public claims imply distributed partial computation is verified when research does not support it.
- Clean-machine client/operator flow cannot be reproduced.

## Evidence bundle

- Source commit, dependency lock hash, artifacts, checksums/signatures, build environment.
- QA logs and supported platform/backend/client/model matrix.
- Local/remote intended-image-flow topology and sanitized evidence.
- Artifact/security/failure-injection report.
- Performance/resource/thermal report.
- Clean-machine consumer/operator results.
- Accepted ADRs, licenses/attributions, trademark decision, waivers/deferrals.
- Public documentation and claims audit.

Raw prompts, private images, outputs not cleared for publication, private keys, credentials, and model weights are excluded.

## Acceptance criteria

- [ ] All required qualification items pass or have explicit approved waiver with risk, mitigation, owner, and expiry.
- [ ] No critical/high security issue is waived for public peer execution.
- [ ] Independent reviewer reproduces canonical local and remote flows from clean machines.
- [ ] Release artifacts and source are traceable and rollback has been tested.
- [ ] Tracker/index/Memory Bank/public docs agree on status and limitations.
- [ ] Human approval authorizes release and documentation completion.

## Completion evidence required in the tracker

- Final qualification/evidence bundle.
- Approved version/license/trademark decisions.
- Release commit/tag/artifact checksums.
- Human approval and task documentation.
