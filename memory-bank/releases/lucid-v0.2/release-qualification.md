# Milestone: Release Qualification

**Status:** 🟦 In progress — macOS ARM64 and Linux x86_64 checkpoint green; full matrix incomplete
**Track:** Release gate
**Blocking:** Yes
**Depends on:** Every required LUCID v0.2 milestone
**Tracker:** [README.md](./README.md)

## Outcome

Prove that LUCID v0.2 is secure, interoperable, operable, recoverable, correctly documented, and honest about every claim. This milestone integrates evidence; it does not waive incomplete upstream milestones or introduce late substitute implementations.

## Entry criteria

- Content Plane, Live Relay Plane, Reachability Plane, Network Operations, Intended-Stream Vertical Slice, Reputation and Redundant Verification, and MLX Backend are in review or complete.
- ShardWorker has either passed its approved experimental/implementation gate or has an explicit approved deferral with public claims corrected.
- All required ADRs are accepted and linked.
- No open critical/high security issue is hidden behind a future milestone.
- Release candidate source and dependency lockfile are frozen except for approved fixes.

## Qualification matrix

### Build and static quality

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Dependency/security audit with zero unwaived known vulnerabilities.
- [ ] Release builds for supported macOS ARM64, Linux x86_64, and Linux ARM64 targets.
- [ ] License/SPDX and third-party attribution audit, including MLX and any sharding integration.
- [ ] No committed credentials, private keys, personal tokens, raw prompts, model weights, or sensitive evidence.
- [ ] Public APIs/wire schemas have versioning and compatibility tests.

### Protocol interoperability

- [ ] v0.2 ↔ v0.2 content lookup, pull, streaming relay, receipt verification, relay reservation, rendezvous, and reputation evidence.
- [ ] Documented v0.1 compatibility/refusal behavior for content and relay protocols.
- [ ] Mixed operating systems and architectures.
- [ ] Local, LAN, direct WAN, relay-only WAN, and DCUtR-upgraded paths.
- [ ] Multiple content providers and multiple serving peers.
- [ ] MLX serving peer and llama.cpp serving peer through the same requester path.
- [ ] Unmodified Ollama-compatible client coverage for chat, generate, embeddings where supported, tags, show, pull, and version.

### Security regression

- [ ] Manifest signature/expiry/schema validation.
- [ ] Signer authorization and resource caps.
- [ ] Receipt signature, job/manifest, serving PeerId, commitment, sequence, and chunk-count binding.
- [ ] Content CID, alias signature/conflict/expiry/rollback, provider identity, and atomic-install validation.
- [ ] Path traversal, unsafe alias, command injection, log injection, oversized input/frame/DNS record, and malformed serialization.
- [ ] Replay and duplicate execution boundaries.
- [ ] Slowloris, slow consumer, stream flood, pull amplification, redundant-execution amplification, reservation flood, and connection churn.
- [ ] Reputation Sybil/poisoning/replay limitations match the ADR.
- [ ] Sharding deviations match only the claims permitted by its accepted gate.

### Reliability and recovery

- [ ] Consumer, contributor, content provider, and relay restart independently.
- [ ] Interrupted pull resumes and installed content remains valid.
- [ ] Worker/adapter crash recovery is bounded; no crash loop.
- [ ] Client cancellation and peer loss release resources.
- [ ] Relay/site loss recovers for subsequent requests.
- [ ] DNS rotation and stale record expiry.
- [ ] Disk full, corrupt evidence store, corrupt partial download, missing model, incompatible format, and identity permission failure.
- [ ] Upgrade and rollback preserve identity, verified content, and compatible configuration.

### Performance and resource envelopes

- [ ] Publish measured baseline hardware/network configurations.
- [ ] Bootstrap, alias resolution, transfer, verification, first-token, decode, receipt, relay overhead, and DCUtR timing.
- [ ] Memory/CPU/GPU/ANE/disk/network use for idle and loaded states.
- [ ] Concurrent stream and pull limits.
- [ ] Relay reservation/stream/bandwidth saturation points.
- [ ] Reputation storage growth and compaction behavior.
- [ ] Sharding overhead only if that capability ships.
- [ ] Set regression thresholds from measured baselines and rerun after fixes.

### Operations

- [ ] Consumer clean install, configuration, bootstrap, pull, chat, cancellation, update, rollback, and removal.
- [ ] Contributor clean install, model provision, policy, reachability, service management, monitoring, and pause.
- [ ] Relay/rendezvous clean-host runbook executed by an independent reviewer.
- [ ] Identity backup/restore and compromise rotation drill.
- [ ] Multi-site inventory, DNS, status, alert, retention, and incident procedures.
- [ ] No operational procedure depends on undocumented shell history or private author knowledge.

### Documentation and claims

- [ ] Root README shipped/current state clearly separates shipped, experimental, deferred, and aspirational behavior.
- [ ] `crates/lucidd/README.md` documents exact endpoints, protocols, config, policy, content store, backends, and limitations.
- [ ] Protocol specs cover content, alias, streaming, reachability, evidence, and any sharding additions.
- [ ] Privacy text explains remote prompt visibility and what relays do/do not retain.
- [ ] Security text explains reputation limitations and any probabilistic verification.
- [ ] LUMEN remains separate and is not implied to ship with LUCID.
- [ ] Core AI status matches its approved go/defer/reject decision.
- [ ] No marketing statement says “verified” where only identity, commitment, redundancy, or reputation exists.

## Release-blocking defects

- Any path that executes or installs unverified content.
- Any remote-success path without mandatory receipt binding under the v0.2 policy.
- Unbounded stream, pull, relay, or redundant-execution resource growth.
- A way for attacker-controlled input to escape model/artifact directories or become a command argument unsafely.
- A relay/rendezvous role that cannot enforce the approved operator caps.
- Data loss/corruption on interrupted pull, upgrade, rollback, or identity migration.
- Silent downgrade from live streaming to batch if the client/operator cannot tell.
- Reputation used as proof or open-auth justification outside the ADR.
- ShardWorker claims exceeding measured verification.
- Missing clean-machine reproducibility.

## Evidence bundle

The release candidate evidence bundle must contain:

- Source commit, dependency lockfile hash, build environment, artifacts, checksums, and signatures.
- QA command output and test totals.
- Platform/hardware/network matrix.
- Intended-stream recordings/logs/topology.
- Security and failure-injection report.
- Performance/resource report.
- Clean-machine consumer, contributor, and operator run results.
- All accepted ADRs and any approved waivers/deferrals.
- Public documentation diff and claims audit.

Sensitive raw prompts, tokens, private keys, credentials, and model weights are excluded.

## Acceptance criteria

- [ ] Every required matrix item passes or has an explicit human-approved waiver with scope, risk, mitigation, and expiration.
- [ ] No critical/high security finding is waived for public-network release.
- [ ] The canonical intended stream is reproduced from clean machines by a reviewer other than the implementer.
- [ ] Release artifacts are reproducible enough to trace source and verify checksums/signatures.
- [ ] Rollback is tested before publication.
- [ ] Tracker, index, Memory Bank, crate docs, and root README agree.
- [ ] Human approval authorizes release and documentation completion.

## Completion evidence required in the tracker

- Final qualification report and evidence bundle.
- Approved waivers/deferrals, if any.
- Release commit/tag/artifact checksums.
- Human approval record and task documentation.

## 2026-08-09 approved qualification checkpoint

- Approved source fingerprint: `1356ef6520ce7ad7dab6369ed40e50cd7507bfff570df87f1881db41fbb7d847`; Cargo.lock SHA-256: `107edfbf1e0dc7ab0c780fe9a156eb3df5c69020ca5f386bbe59acbeac3e8329`.
- macOS ARM64: 460 tests passed, 2 hardware-only tests ignored; strict Clippy, formatting, diff hygiene, and native release bundle checks passed.
- UMBP Ubuntu x86_64: offline locked optimized all-target build passed in 1,339s; 450 tests passed with 2 hardware-only ignores in 241s; strict Clippy passed in 40s. Evidence metadata binds Linux/x86_64, Rust/Cargo 1.91.1, source fingerprint, and lock hash.
- Isolated release-binary HTTP smoke passed version, tags, non-stream generation, streaming generation, embeddings, headers, commitment, teardown, and production-service isolation. The temporary port closed; the production relay stayed active on PID `106521` with zero restarts.
- Security checkpoint: zero known vulnerabilities, four allowed unmaintained transitive warnings, bans/licenses/sources policy passed, no high/critical review finding, and secret/incomplete-marker scans passed.
- Remaining before completion: Linux ARM64 resource/toolchain rerun, physical interoperability/NAT matrix, real MLX, full intended stream, clean-machine/rollback/operator drills, release commit/tag, and public claims audit. The unavailable interactive browser controller is recorded; the native compiled real-process browser-contract test passed.
- Evidence location and review details: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).

## 2026-08-10 pre-publication revalidation

- Recomputed the exact source fingerprint as `1356ef6520ce7ad7dab6369ed40e50cd7507bfff570df87f1881db41fbb7d847` and Cargo.lock SHA-256 as `107edfbf1e0dc7ab0c780fe9a156eb3df5c69020ca5f386bbe59acbeac3e8329`; both match the approved checkpoint.
- `scripts/phase-validate.sh workspace` passed formatting, all 460 host-native tests with 2 explicit hardware/fixture ignores, and workspace/all-target strict Clippy with `-D warnings`.
- The first managed-sandbox attempt could not bind loopback sockets and therefore failed 12 socket-dependent tests with OS permission errors. The identical locked/offline command was rerun with local-loopback permission and passed; no code change or test waiver was used.
- Evidence: `target/phase-validation/20260810T201819Z-38743/`. This evidence is local generated output and is not committed.
- The offline security profile passed: zero vulnerabilities; five allowed dependency warnings (four unmaintained crates plus yanked transitive `spin 0.9.8` through `postcard`/`heapless`); Cargo-deny advisories, bans, licenses, and sources passed with its documented duplicate/unmatched-license warnings. Evidence: `target/phase-validation/20260810T202203Z-41037/`.
- This revalidation authorizes checkpoint publication only. It does not satisfy Linux ARM64, real MLX, physical NAT/intended-stream, rollback, fleet, or release/tag gates.
