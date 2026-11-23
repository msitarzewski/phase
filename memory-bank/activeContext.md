# Active Context: Current Sprint

**Last Updated**: 2026-11-23
**Sprint**: Execute soup-to-nuts physical qualification across Cloud Relay (sole static-IP relay), Ubuntu Desktop (5060ti 16GB, 32GB RAM), Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM), Mac M1 Max (32GB RAM), and Mac M5 Max (128GB RAM, cellular daily driver) using the new self-configuring `lucidd` + cross-platform Tauri v2 desktop app (`.dmg` + `.deb`)
**Status**: Phase Core, LUCID v0.1, and v0.1.1 remain shipped. On `feat/lucid-consumer-experience`, LUCID v0.2 now includes the self-configuring `lucidd` core & permission-first probe (`node_config.rs`, `runtime_manager.rs`, CLI subcommands `probe`/`models`/`peers`/`invite`/`join`) and the sandboxed cross-platform Tauri v2 + Svelte 5 desktop app (`apps/lucid-desktop/`, producing macOS `.dmg` and Ubuntu `.deb` installers; [261123 task record](tasks/2026-11/261123_apple-simple-lucid-desktop.md)), alongside the 2026-11-22 efficiency pass and `libp2p 0.57` security upgrades (0 vulnerabilities across both root and desktop `Cargo.lock`). This is still not a v0.2 release: bringing up Cloud Relay + `bootstrap.phasebased.net` TXT record, physical two-network/NAT intended stream, real multi-GB pull/resume, real Apple Silicon MLX, Linux x86_64/ARM64 reruns on current source, public rendezvous quotas, correctness-evidence validator, whole-model baseline, ShardWorker research/ADR, and multi-site operations remain open.

---

## Current Focus

The immediate focus is executing the [physical fleet soup-to-nuts test plan](releases/lucid-v0.2/intended-stream-vertical-slice.md#soup-to-nuts-physical-fleet-test-plan--2026-11-22) across the active fleet using installable GitHub Release bundles (`.github/workflows/release.yml`, `scripts/install-lucid.sh`, `scripts/fleet-install.sh`) and gitignored local config (`.phase-fleet.local.env`) to convert implemented v0.2 code into release acceptance evidence without weakening the tracker. Seven v0.2 milestones are in progress, the physical intended-stream slice and ShardWorker are blocked on named gates, and Core AI remains not started/non-blocking. The authoritative state and evidence links are in `releases/lucid-v0.2/README.md`; LUMEN remains a separate planned node under `releases/lumen/`.

### Workspace/runtime efficiency & physical fleet readiness (2026-11-22)

- Completed the workspace, build/CI, and runtime efficiency pass ([261122 task record](tasks/2026-11/261122_workspace-and-runtime-efficiency.md)): reclaimed ~140 GB net of stale build/cross-target/boot/validation blob bloat, removed committed v0.1.0 binaries under `dist/`, unified `[workspace.dependencies]` (eliminating duplicate `tower 0.4`, `tower-http 0.5`, and `native-tls`), extracted `crates/lucidd/src/backend_io.rs`, added lock-free `DashMap` warm-hit fast path in `LlamaCppWorker::ensure_loaded`, shared `wasmtime::Engine`/module cache + `StoreLimits` in `plasm`, zero-clone `CompiledPolicy` + reusable `sysinfo::Components` in `PolicyEngine`, `O(n)` log sanitization + async `tokio::fs::metadata` in `ollama`, `hex::encode` in `ModelCid::to_hex`, zero-clone canonical JSON sorting, concurrent `async let` + event-driven shutdown + background polling backoff in `apps/lucid-macos`, and split 7 oversized Rust modules into `<mod>/mod.rs` + `<mod>/tests.rs`.
- Added installable release packaging & fleet automation: `.github/workflows/release.yml` (Linux x86_64/ARM64 tarballs + macOS Apple Silicon CLI tarball + `LUCID.app` zip + `SHA256SUMS.txt`), `scripts/install-lucid.sh`, `scripts/fleet-install.sh`, and `.phase-fleet.env.example` (gitignored `.phase-fleet.local.env`). Scrubbed all private hostnames, SSH aliases, usernames, and real IPs from repository documentation.
- **Active physical test fleet (Cloud Relay is the sole host with a direct static external IPv4; all others sit behind NAT; existing web ingress is `Cloud Relay (Caddy) → Tailscale → Ubuntu Desktop (Caddy)` and must remain untouched):**
  - **Cloud Relay** (DigitalOcean Ubuntu x86_64) — sole direct static public IPv4 host: infrastructure relay/bootstrap (`--mode infrastructure`, direct public TCP `4001`, separate from Caddy `80/443`).
  - **Ubuntu Desktop (5060ti 16GB, 32GB RAM)** — Ubuntu 26.04 LTS x86_64 (behind home NAT; runs Caddy for existing web origins behind Cloud Relay via Tailscale): CUDA Contributor #1 + Content Provider #1 (`--worker llama-cpp --serve-model-content`, loopback-only HTTP `:11435` never exposed via Caddy), invitation issuer.
  - **Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)** — Ubuntu 26.04 LTS x86_64 (behind home NAT): Linux x86_64 qualification host (`scripts/phase-validate.sh linux`) + Content Provider #2 (`--serve-model-content`).
  - **Mac M1 Max (32GB RAM)** — macOS 27.x (Apple Silicon, behind home NAT): Apple Silicon Contributor #2 (`llama-cpp` Metal + real `--worker mlx`).
  - **Mac M5 Max (128GB RAM)** — macOS 27.3 b2 (Apple Silicon, daily driver on cellular hotspot): clean consumer running installed `LUCID.app` (`--mode consume-only --outbound-only`) + CLI pull/resume/DCUtR tests, plus macOS QA lead and Ubuntu ARM64 VM host for `PHASE_LINUX_AARCH64_EVIDENCE`.

### CI security and test reliability refresh (2026-11-09)

The owner approved updating dependencies and repairing CI after the new branch runs failed. The repair at `313565e` updates compatible dependency families, scopes audit check-write permission to the advisory job, fixes the relay timeout test's valid peer-failure outcome, and isolates macOS fixture processes after captured socket-inheritance failures. Final local qualification passes 460 tests / 2 ignored, formatting, strict Clippy, actionlint, dependency audit and cargo-deny; there are zero reported vulnerabilities or yanked packages under the existing policy, with four transitive unmaintained warnings retained. The relay test passed 40 repeats and the backend suite passed 16 repeats. See [the dated qualification record](progress.md#ci-security-and-test-reliability-refresh-2026-11-09) for source/evidence details. This supersedes the November 7 local test-investigation blocker; public-network and release-acceptance gates remain open.

### Competitive and operational reassessment — 2026-11-07

- The [current competitive review](research/2026-06-21-ai-sovereignty-and-local-inference-ecosystem.md#competitive-reassessment--2026-11-07) identifies OpenHydra as a direct inference-network peer and KwaaiNet as a close nonprofit/technical peer. Petals, exo, AI Horde, Hyperspace, Cocoon and the Public AI utility cover other parts of the intended end state. The category is not empty; differentiation must rest on demonstrated protocol, governance and evidence guarantees.
- Both READMEs now distinguish shipped v0.1/v0.1.1, implemented-but-unreleased v0.2, and open acceptance gates. Mission copy no longer treats signatures as proof of model execution or onion routing as serving-operator confidentiality. Historical June provider/political/model-ranking claims remain clearly marked as unvalidated history, not current positioning evidence.
- The existing [trust milestone](releases/lucid-v0.2/reputation-and-redundant-verification.md#method-comparison--2026-11-07) now compares TOPLOC, redundant replay, log-probability challenges and TEE assumptions, with an experiment/go-no-go record. No new validator or correctness guarantee is claimed.
- Read-only host preflight found Cloud Relay reachable but without a Phase service/toolchain or TCP 4001 ingress; historical historical Apple Silicon host timed out; Ubuntu Desktop (5060ti 16GB, 32GB RAM) is now a reachable NVIDIA/Linux contributor candidate without Phase/llama-server tooling. Reconfirm the physical/NAT topology rather than assuming the August M1 assignment still holds. The [operator runbook](releases/lucid-v0.2/intended-stream-vertical-slice.md#operator-runbook--2026-11-07) records prerequisites and concrete actor commands.
- Fresh QA is separate from August's passing checkpoint. Initial failures are retained at `target/phase-validation/20261107T182908Z-24926/`. Internal HTTP error boxing and narrow annotations for async_trait-generated attributes resolve Rust 1.99 strict-Clippy diagnostics; the existing test allocator now probes whole disjoint ranges below standard ephemeral ports. A full workspace run passed 460 tests / 2 ignored, formatting and strict Clippy at `target/phase-validation/20261107T184728Z-43089/`.
- Repeated backend suites still expose an intermittent subprocess startup bind failure (diagnostics report `Address already in use`), including after several successful runs. Fixture stderr can now be retained with `PHASE_TEST_DIAGNOSTICS_DIR`; the six-run diagnostic campaign passed five times and failed once. This remains an open QA issue; the required retry/time-budget extension was requested before further investigation or public-host setup. One full green run is not a waiver for the repeated failure.
- The real installed llama-server 0.8.0/build 11960 and 6,975,879,008-byte Gemma GGUF passed the existing explicit hardware smoke separately. This is local backend evidence only; no public-network, multi-GB pull/resume, MLX or independent-validator gate is promoted.

```
crates/
├── phase-identity/         # Ed25519 persistent identity (substrate)
├── phase-net/              # libp2p / Kademlia / mDNS / Noise+QUIC (substrate)
├── phase-manifest/         # SignedManifest<T> (substrate)
├── phase-receipt/          # SignedReceipt<T> (substrate)
├── phase-protocol/         # Worker trait + JobSpec / JobStream (substrate)
├── phase-artifact-server/  # Content-addressed HTTP server (substrate)
├── plasm/                  # WasmtimeWorker — reference WASM Phase node
└── lucidd/                 # LUCID daemon — inference Phase node (in progress)
```

### Verification and scale sequencing — 2026-08-24

- Existing receipt verification binds signer, dispatched job, serving PeerId, chunk sequence/count, and delivered output commitment. That is attributable transcript integrity, not proof that the peer executed the claimed model/backend correctly (`crates/lucidd/src/router.rs:1550-1678`).
- The next trust upgrade is a bounded, versioned, workload-specific evidence seam: serving backends generate exact-bound opaque evidence; independently selected LUCID validators interpret it and sign explicit empirical/probabilistic trust labels. The generic Phase stream may carry and commit the bytes without learning inference semantics.
- Before `ShardWorker`, benchmark heterogeneous peers executing complete exact-CID jobs independently. Reuse router eligibility, `ModelCapabilities`, verified content pull, worker lifecycle, commitments, and receipts; measure aggregate throughput, latency, content fan-out, validator overhead, fairness, and failure recovery.
- ShardWorker advances only if that baseline leaves a concrete workload requirement unmet and the partial-compute verification ADR passes. Open authorization is no longer tied to sharding; its gate is correctness evidence plus public-load, amplification, Sybil, abuse, and network qualification.

---

## LUCID v0.2 approved implementation checkpoint (2026-08-09)

- **Content Plane**: content-derived GGUF CIDs, canonical MLX bundle roots, signed alias/provider records, durable rollback protection, bounded resumable transfer, verification-before-commit, catalog restore, and real `/api/pull` coordination are implemented. Real multi-gigabyte physical transfer/resume remains.
- **Live Relay Plane**: v2 ordered substreams, cancellation, terminal receipts, shared v1/v2 admission/replay gates, backpressure, and strict frame/lifecycle validation are implemented. Linux qualification found and fixed the negotiation-vs-idle timing bug.
- **Reachability**: explicit peer/infrastructure roles, relay server/client, AutoNAT, rendezvous client/optional-server surfaces, DCUtR, path observation, and exact relay-loopback tests are implemented. The production daemon deliberately keeps the public rendezvous server disabled until global/per-peer/per-namespace quotas exist (`crates/lucidd/src/main.rs:756-765`). Real consumer NAT/DCUtR remains.
- **Trust**: privacy-minimal local evidence, decay/confidence/cold-start assessment, operator precedence, truthful outcome taxonomy, and bounded deterministic redundancy are implemented. Receipts prove transcript integrity; the correctness-evidence/independent-validator seam remains unimplemented, and no global-truth or partial-compute claim is made.
- **MLX**: a pinned-bundle loopback subprocess adapter reuses the shared Worker/router/API/receipt path and fails closed on unsupported platforms or mutable runtime/content. Real Apple Silicon model acceptance remains.
- **Qualification**: macOS ARM64 460 passed / 2 ignored; Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) Linux x86_64 450 passed / 2 ignored plus all-target release and strict Clippy; live isolated HTTP generate/stream/embed passed; zero vulnerabilities. Evidence: `target/phase-validation/linux-x86_64-ubuntu/1356ef6520ce7ad7dab6369ed40e50cd7507bfff570df87f1881db41fbb7d847/20260809T183940Z-634076/`.
- **Production safety**: Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) qualification used separate fingerprinted source/evidence and transient units. The standing relay stayed on PID `106521`, zero restarts, and its existing ports/config/Caddy were untouched.
- **Next executable gate**: finish the Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) backup/Ubuntu 26.04 LTS upgrade and post-upgrade audit, provision the independent public foundation node, enroll Mac M1 Max (32GB RAM) as the Apple Silicon contributor candidate, then run the physical consumer/contributor/relay intended stream. Independently rerun Linux ARM64 qualification and real MLX acceptance. Trust/scale research starts with the correctness-evidence contract and whole-model independent-job baseline; ShardWorker cannot begin until that baseline and its verification research/ADR are approved.

See `tasks/2026-08/260809_lucid-v0.2-implementation-linux-qualification.md`.

## Approved external test topology (2026-08-10)

- **Public foundation node**: provision a small DigitalOcean Ubuntu x86_64 host with a Reserved IPv4 and run `lucidd --mode infrastructure` locally. The recommended starting envelope is 1 vCPU, 2 GB RAM, and 50 GB disk; the Phase service remains capped at 1 GB and 200% CPU by `crates/lucidd/systemd/lucidd-relay.service:35-40`.
- **Traffic separation**: expose native Phase/libp2p TCP `4001` directly to the foundation `lucidd`. Terminate web TCP `80/443` in Caddy on the public host and proxy web origins to Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) over Tailscale, not to the DHCP-assigned Sonic public address. The LUCID HTTP API remains loopback-only; Ollama is never public.
- **Contributor**: Mac M1 Max (32GB RAM), the M1 iMac with 16 GB RAM, is the Apple Silicon inference/MLX acceptance candidate. Its Tailscale address is private operational data and is used for administration, not as proof of Phase relay reachability.
- **Requester**: the current Apple Silicon development machine on a physically different network is the consumer. Proton VPN/Tailscale may remain available for administration, but acceptance traffic must traverse the Phase public relay path rather than a Tailscale data path.
- **Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)**: remains the existing foundation/HTTP origin during transition. A backup snapshot is in progress before its Ubuntu 26.04 LTS upgrade; it must pass SSH, Tailscale, Caddy, Docker/Ollama, `lucidd`, firewall, and TCP `4001` post-upgrade checks before re-entering the test matrix.
- **Acceptance truth**: the DigitalOcean plan supplies the missing independent public actor and stable ingress, but it does not complete the milestone until real pull/resume, relay-only inference, first-token timing, cancellation, receipt verification, DCUtR/fallback, and resource evidence are recorded.

---

## Phase Core COMPLETE (May 2026)

- [x] **M1**: Workspace scaffold — root Cargo.toml, eight crate skeletons, SPDX headers, empty `cargo build --workspace` green.
- [x] **M2**: Extract `phase-net` — libp2p upgraded to the current workspace 0.56 line, Kademlia + mDNS + Noise+QUIC, generic peer capabilities decoupled from WASM-specific fields.
- [x] **M3**: Extract `phase-identity` — persistent Ed25519 keypair on disk via `dirs` crate, fixes the ephemeral-key bug carried over from the November MVP.
- [x] **M4**: `phase-protocol` — `JobSpec` enum with `Wasm` and `Inference` variants, streaming `Worker` trait with `JobHandle` + `JobStream`, `DynWorker` object-safe shim.
- [x] **M5**: Extract `phase-manifest` + `phase-receipt` — generic `SignedManifest<T>` / `SignedReceipt<T>` envelopes, Ed25519 signing reused from `phase-identity`, commitment-accumulator chunk hashing for streamed results.
- [x] **M6**: Extract `phase-artifact-server` — blob-id keyed layout, range request preserved, signed manifest integration.
- [x] **M7**: Reposition Plasm — `daemon/` deleted via `git mv`, `WasmtimeWorker` impls `phase_protocol::Worker`, PHP SDK migrated to dual-format signing (legacy + `phase-receipt:v1:`), hello.wasm output byte-identical.
- [x] **M8**: Verification, docs, daemon removal — `cargo build --workspace` clean, 152 tests pass, zero clippy warnings across phase-* and plasm, `cargo publish --dry-run -p phase-identity` packages cleanly, README rewritten for post-M7 layout, Memory Bank updated.

---

## Security Hardening SHIPPED TO PRODUCTION (PR #9 merged 2026-05-29/31)

**Merged to `main` (commit `73fa907`, PR #9) and deployed live.** Three-environment validation before merge: Mac native arm64, Docker cross (amd64 + arm64), and a vanilla Ubuntu 26.04 ARM64 box (Ubuntu ARM64 VM) — build + 246 tests + clippy `-D warnings` green on all three. `cargo audit` 0 vulnerabilities; `cargo deny check` ok.

**Live deploy:** Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) foundation relay swapped from the pre-hardening binary to the hardened build via `lucidd-relay.service`. Critical success: **peer id `12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB` preserved across the swap** (SEC-08 persistent identity), so `bootstrap.phasebased.net` TXT still resolves. Listening v4+v6 on 4001, healthy. The entire audit surface (signer authz, DoS caps, receipt verify+bind, patched wasmtime/hickory, postcard) is now the production substrate.

**Operational note:** the relay is now on postcard/schema-v2 model advertisements (SEC-12). It's the only foundation node, so no live mismatch — but any other node joining now needs the hardened binary (v1 bincode ↔ v2 postcard advertisements don't interop, by clean-break design).

### Standing infrastructure note
- Private SSH targets, static relay IPs, and host-specific paths are kept exclusively in gitignored `.phase-fleet.local.env` (template: `.phase-fleet.env.example`) and never committed to the repository.
- Hardware roles used for qualification: **Cloud Relay** (DigitalOcean Ubuntu x86_64, sole static public IPv4), **Ubuntu Desktop (5060ti 16GB, 32GB RAM)**, **Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)**, **Mac M1 Max (32GB RAM)**, **Mac M5 Max (128GB RAM)**, and an **Ubuntu ARM64 VM**.

### Original hardening detail (for reference)

A 5-agent security audit (`SECURITY-AUDIT-2026-05-28.md`) found sound crypto primitives but an unenforced trust model, 20 dependency advisories (incl. wasmtime sandbox escapes), and a PHP SDK verification bypass. The 14-task remediation plan (`memory-bank/plans/security-hardening/`) was executed in 5 agent-team waves, all on branch `security-hardening`:

- **C1** (keystone): signer authorization — allowlist + PeerID-bind gate on the lucidd relay handler and plasm WASM worker; `verify()` now actually called before dispatch; manifest resource caps clamped server-side. Default-deny with `allow_unauthenticated_jobs=false` escape hatch (hybrid policy — Michael's call delegated, recommended option taken).
- **C2**: wasmtime 27→36 (clears 16 advisories incl. aarch64/Winch sandbox escapes), hickory 0.24→0.26. `cargo audit` 20→0 vulnerabilities. hello.wasm byte-identical.
- **C3**: PHP SDK — removed dual-format downgrade + `local_execution` magic-string bypass, pinned expected-key now mandatory.
- **H1/H2**: model-path traversal confined; peer-served receipts verified + bound (job_id/worker-key/commitment) with `X-Lucid-Receipt-Verified` header.
- **M1/M2/M3/M4/M6**: inbound DoS caps (size + semaphore + off-driver exec), worker model-LRU + port tracking, atomic 0600 key write, DNS-bootstrap caps + /p2p/ pin + fail-closed, URI log sanitization.
- **Hygiene**: deny(unsafe_code) on all crates, NodeIdentity Debug redaction, streamed artifact ranges, lock-poison recovery, bincode→postcard (unmaintained).
- **Process**: first CI workflow + `cargo audit`/`cargo deny` gate (`.github/workflows/security.yml`, `deny.toml`, `.cargo/audit.toml`).

Three dependency advisories documented-and-accepted (upstream-pinned hickory via libp2p-mdns; BSD-only nix). v0.1 content-CID hardening (SEC-13/L6) deferred to v0.2.

**Final gate:** 246 tests pass, clippy `-D warnings` clean, `cargo audit` 0 vulns, `cargo deny check` ok. **MERGED (PR #9, `73fa907`) and deployed live — see the "SHIPPED TO PRODUCTION" header above.**

## v0.1.1 gap-closure SHIPPED (PR #10, merged `14ae8d6`, 2026-06-09)

`/api/embed` + legacy `/api/embeddings`, multi-peer relay failover (`RouteDecision.fallback_peers`), self-traffic policy path (`should_serve_self` — bypasses donation-protection gates, honors only `manual_pause`), `/api/pull` stub (registers an already-present local GGUF; network pull + content CIDs are v0.2), and the corrected `find_peers_by_model_id` docstring. Embeddings ride the existing `OutputChunk` commitment/receipt machinery (no new `JobResult`); SEC-05 verify+bind covers embedding relays unchanged. 263 tests / clippy `-D warnings` / `cargo audit` 0 vulns; validated live on Ubuntu ARM64 VM. See `tasks/2026-06/260609_lucid-v0.1.1-gaps.md` + `decisions.md` 2026-06-09.

**Post-merge (2026-06-09):** Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) relay redeployed to v0.1.1 (x86_64 cross-build, peer-id `12D3KooWJ6vTjo6yFgEc…` preserved, old binary backed up `~/bin/lucidd.pre-v0.1.1`). Two-node authz demo run on Ubuntu ARM64 VM (default-deny reject → escape-hatch accept w/ `receipt-verified: true`). Demo surfaced + fixed two latent bugs on branch **`lucid-sec05-receipt-binding`** (workers now sign receipts with the node identity → SEC-05 peer-receipt binding actually works; echo model advertised under `from_model_id` → cross-peer discoverable). Deferred: policy notify-watcher dir-scope reload loop (see `decisions.md` 2026-06-09).

## Historical candidate threads (2026-06-09; superseded by Current Focus)

v0.1.1 is shipped; no forced next step. Candidate threads, roughly by leverage:

- **Remaining LUCID gaps:** real network `/api/pull` (content-hashed CIDs), token streaming over the relay, a content-hashed cross-peer name→CID index (today the deterministic `from_model_id` placeholder stands in). All v0.2-flavored. Ubuntu ARM64 VM is the clean test target; Mac M5 Max for GPU/real-embedding-model validation.
- **Reach / "anyone, anywhere":** make `--bootstrap-dns` the default for fresh installs; stand up 2-3 more geographically-distributed foundation relays (Hetzner CAX11s ~$4/mo); the consumer install page at `phasebased.com` (mom-language one-liner).
- **v0.2 frontier:** strengthen LUCID trust with an exact-bound correctness-evidence/independent-validator seam, then measure whole-model independent-job parallelism across heterogeneous peers. `ShardWorker` remains gated on both that control baseline and the **partial-compute verification open problem** (`decisions.md#2026-05-29-verifying-sharded--partial-computation-is-an-unsolved-open-problem-recorded-risk`). Open authorization has a separate correctness/load/abuse/public-network trigger and is not a ShardWorker deliverable.
- **LUMEN:** stand up the diffusion-node skeleton against the substrate — proves the second flagship compiles, validates the workload-agnostic bet. Post-v0.1.
- **MLX worker (LUCID M3, deferred):** Apple Silicon native inference (GPU/Metal, via turnkey `mlx-lm`) — the M5 Max can validate it. Faster path to an Apple backend.
- **`CoreAIWorker` (Apple Neural Engine backend, candidate):** license-clean path to run *our own* converted open-weights models on the ANE — the efficiency play for always-on contributor Macs (pairs with M7 battery/thermal auto-pause). After MLX; more harness work (you supply tokenizer/KV-cache/sampling); **gated on verifying the public Core AI API exposes KV-cache / stateful decode.** Full analysis: `research/2026-06-09-apple-coreai-foundation-models.md`. NOTE: Apple **Foundation Models** is a hard no for a LUCID backend (ToS + unenforceable-AUP-for-a-relay) — see `decisions.md` 2026-06-09.
- **Offered-but-not-run demo:** the authz-gate-in-action three-curl demo (default-deny rejects an unauthorized routed job → escape-hatch accepts it with `X-Lucid-Receipt-Verified: true`). Needs a small two-node setup on Ubuntu ARM64 VM + a workaround for the echo-worker CID-mismatch quirk.

---

## LUCID Software COMPLETE (May 2026)

- [x] **LUCID M1**: `crates/lucidd/` scaffold with AGPL-3.0 license. Echo worker spike validated the streaming Worker trait against real Ollama clients (`ollama` CLI v0.24 streamed `dlrow olleh` against EchoWorker via `/api/chat`).
- [x] **LUCID M2**: `LlamaCppWorker` — `llama-server` subprocess management, supervisor task with 3-crash/60s circuit-break, hang detection (30s no-progress timeout), per-request idle timeout, fake-llama-server stub binary for tests so CI doesn't need a real GGUF model. CLI flag `--worker echo|llama-cpp`.
- [ ] **LUCID M3**: `MlxWorker` — deferred to v0.1.1. Apple Silicon native via mlx-lm subprocess. *Requires Apple Silicon test rig for full validation.*
- [x] **LUCID M4** (demo-sufficient): Ollama API surface on `:11434` covers `/api/chat`, `/api/generate`, `/api/tags`, `/api/show`, `/api/version`. NDJSON streaming format, `x_phase_commitment` carried in terminal frame. **`/api/embeddings` and `/api/pull` deferred to v0.1.1** — not on the demo critical path.
- [x] **LUCID M5**: Local-or-DHT router — per-request decision (local-only flag → policy → local-loaded → DHT lookup → refused), `X-Lucid-Local-Only` request header parsed, `X-Lucid-Routed-Via` response header set, 503 + reason on refused. Peer relay via libp2p `/phase/job-relay/1.0.0` (CBOR codec, 5min timeout). **Peer-relay is batch (not token-streaming) in v0.1; streaming-via-peer is v0.2.**
- [x] **LUCID M6**: Model registry on DHT — signed `ModelAdvertisement` (bincode + Ed25519), key `b"phase/model/" || model_cid`, 5-minute TTL refresh task, persistent identity carry-through so advertisements survive daemon restart with the same peer_id.
- [x] **LUCID M7**: Policy + auto-pause — declarative `lucid-policy.toml` (default at `~/.config/lucidd/policy.toml`), `PauseReason::{Manual, OnBattery, ThermalLimit, OutsideTimeWindow, ConcurrencyLimit, ModelNotInAllowlist, SystemPaused}`. Battery via the `battery` crate (macOS IOKit / Linux sysfs), thermals via `sysinfo`. Config reload via `notify` filesystem watch + SIGHUP. 24 policy tests cover every decision branch.
- [ ] **LUCID M8**: Two-node end-to-end demo — *Hardware-blocked.* Software is ready. Requires Machine A (Linux + 24GB+ NVIDIA GPU + real llama-server binary + a GGUF model) and Machine B (any Linux), Tailscale-bridged or real WAN.

## Historical v0.1 limitations (superseded by the v0.2 tracker)

- Peer-relay is batch-shaped; token streaming across the relay is v0.2
- No multi-peer retry on first-peer failure
- `find_peers_by_model_id` resolves names via the local loaded set only — cross-peer name→CID registry is v0.2
- Peer-served full `SignedReceipt<JobResult>` doesn't propagate back through the relay (only the output commitment rides in the events)
- `/api/embeddings` and `/api/pull` not implemented
- `PolicyEngine::should_serve` refuses self-traffic when on battery by default — needs a "self-traffic always allowed" config option for laptop UX

## Stack snapshot (post-LUCID M5)

```
crates/
├── phase-identity/         # Ed25519 persistent identity         Apache-2.0
├── phase-net/              # libp2p / Kademlia / mDNS / Noise     Apache-2.0
├── phase-manifest/         # SignedManifest<T>                    Apache-2.0
├── phase-receipt/          # SignedReceipt<T> + commitment chain  Apache-2.0
├── phase-protocol/         # Worker trait + JobSpec + JobStream   Apache-2.0
├── phase-artifact-server/  # Content-addressed HTTP server        Apache-2.0
├── plasm/                  # WasmtimeWorker reference node        Apache-2.0
└── lucidd/                 # LUCID inference daemon               AGPL-3.0-or-later
```

Workspace: 210 tests passing, ~20K LOC of Rust, `cargo clippy --workspace --all-targets -- -D warnings` clean.

---

## LUCID M8 — DONE 2026-05-28

The two-node demo ran end-to-end on real hardware. Mac M5 Max (128GB) hosting Qwen3-Next 35B-A3B on Metal via `llama-server`; Parallels Ubuntu ARM64 VM at `<vm-local-ip>` with `lucidd --no-local-worker`. `curl http://localhost:11434/api/chat` from inside the VM routed via libp2p + Kademlia DHT, served on the Mac, streamed back NDJSON with `x-lucid-routed-via: peer:ctCUGwkd` and `x_phase_commitment: <sha256>` in the terminal frame. Asciinema captured at `dist/demos/lucid-2node-demo.cast`.

## v0.2 substrate prep — first foundation relay live (2026-05-28 late)

The two-node demo proved the protocol works on a LAN. The follow-on session that same evening took the substrate from "works on a LAN" to "works across the public internet" and stood up the first 24/7 foundation-operated relay node.

### Code changes (all in this commit)

- **`--bootstrap-peer <multiaddr>`** (repeatable) — `phase-net`'s `DiscoveryConfig::bootstrap_peers` field existed since November but was just logged. Now properly parses the multiaddr, extracts the `/p2p/<peer-id>` component, adds the address to Kademlia's routing table, and queues a swarm dial. Connection in tens of ms once the SYN/ACK completes.
- **`--libp2p-port <N>`** — was hardcoded `/ip4/0.0.0.0/tcp/0` (ephemeral random). Now configurable so home routers can forward a known port and DNS records can encode it.
- **IPv6 listen** — lucidd now binds `/ip4/0.0.0.0/tcp/<port>` AND `/ip6/::/tcp/<port>`. Public IPv6 nodes (e.g. Sonic fiber) can be reached without any router port-forwarding.
- **Persistent identity by default** — main.rs was calling `NodeIdentity::generate()` every startup (fresh ephemeral key every restart). Now uses `NodeIdentity::load_or_create(path)` with a platform-aware default (`~/.config/phase/identity.key` on Linux). Same peer-id across restarts.
- **`--identity-path <path>`** — override for the above. Required when running two lucidd instances on the same host.
- **`--mode {worker,relay}`** — clearer alias semantics over the older `--no-local-worker` flag.

### First foundation relay

```
host:           Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) (Ubuntu 24.04 x86_64, Intel i7-3720QM, 15GB RAM, 10Gb sync fiber)
peer_id:        12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB
public mAddr:   /ip4/203.0.113.10/tcp/4001/p2p/12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB
service:        lucidd-relay.service (user-level systemd)
                ├── enabled (boot-start)
                ├── linger=yes (survives logout)
                └── auto-restart on failure
identity file:  ~/.config/phase/identity.key  (32 bytes, mode 600)
DNS:            added 2026-05-28 (subdomain pointing at 203.0.113.10)
```

### What still needs to happen for the "coffee-shop scenario"

The relay is **directly dialable** today, which is enough for any peer to use it as a bootstrap-peer. The full coffee-shop scenario (user behind NAT/VPN reachable through the relay) still needs the libp2p circuit-relay server protocol and DCUtR for hole-punching. That's v0.2 substantive engineering — ~100-200 lines in phase-net plus a real protocol decision about what relay traffic the foundation is willing to carry.

### Next-session checklist for substrate v0.2

- [ ] Wire libp2p `relay::server::Behaviour` into phase-net's `CombinedBehaviour`
- [ ] Wire libp2p `dcutr::Behaviour` for hole-punching
- [ ] Wire libp2p `rendezvous::server::Behaviour` for the model-rendezvous design from MISSION discussion
- [x] **DNS-based bootstrap shipped** — lucidd has `--bootstrap-dns <domain>` (repeatable) that resolves TXT records and dials each as a multiaddr. Validated with `bootstrap.phasebased.net TXT "/ip4/203.0.113.10/tcp/4001/p2p/12D3KooWJ6vTjo6yFgEc..."`. A fresh Mac peer with `--bootstrap-dns bootstrap.phasebased.net` (no explicit `--bootstrap-peer`) dialed Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) in ~70 ms. `hickory-resolver` 0.24 with `system-config` for `/etc/resolv.conf`, Cloudflare/Google fallback for sandboxed envs.
- [ ] Stand up 2-3 more relays in geographically distinct regions (Hetzner CAX11s, $3.79/mo each)
- [ ] Document the relay-operator setup (this systemd unit + DNS-record format)
- [ ] Sonic IPv6 firewall investigation (Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM) listens on IPv6 but Mac→IPv6 currently fails — separate debugging)
- [ ] Decide whether `bootstrap.phasebased.net` should be the *default* `--bootstrap-dns` value for new installs (currently no default — flag must be explicit). Trade-off: defaulting on means "lucidd just works" out of the box but creates soft foundation-DNS dependency.

### Three bugs the demo found that the test suite missed

These were genuine v0.1 gaps that landed during the demo session, not test-only hacks:

1. **`registry.rs` — name→CID lookup couldn't cross peers.** `find_peers_by_model_id("qwen3")` only checked the local loaded set; if a node had no local copy of the model, it couldn't translate the name into a DHT key. Fixed with `ModelCid::from_model_id`, a deterministic SHA-256 with `b"phase/model-id-v1:"` domain separation. Two peers compute the same CID for the same name without coordinating. v0.2 will replace this placeholder with real content-hashed CIDs from `/api/pull` verification.

2. **`phase-net/discovery.rs` — Kademlia was Client-mode by default.** libp2p-kad 0.48 ships new nodes as `Mode::Client`, meaning they can issue queries but refuse to serve them. `GetRecord` requests came back as "protocol not supported". Fixed with `kad_behaviour.set_mode(Some(KademliaMode::Server))`. **Every v0.1 deployment hit this — the only reason internal tests passed is they all used the mock `DhtTransport` and never exercised real libp2p Kademlia between peers.**

3. **`lucidd/router.rs` — bincode 1.x can't roundtrip `Option<DateTime<Utc>>` with `skip_serializing_if`.** The peer-relay request was `bincode(SignedManifest<JobSpec>)`; bincode bailed on the optional `expires_at` field. Switched to `serde_json` for both request and response payloads (CBOR-wrapped on the wire). Costs us ~1KB more per relay; gains us a transport that handles every serde type cleanly.

### Honest postmortem

The first "M8 complete" claim earlier in the day was premature — the user got lucidd starting cleanly on both sides and I jumped to "done." The actual end-to-end curl was never run that round. The follow-up session caught all three real bugs above. Lesson worth remembering: **"daemon starts cleanly" is not the same as "demo works."** Demo verification means running the actual user-facing command and seeing a real response. The protocol stack has multiple silent-failure modes (peer discovered but Kademlia in client mode; CID lookup mismatch; serialization incompatibility) that only show under real cross-peer load.
- **LUCID M3 (MlxWorker)** — deferred to v0.1.1. Apple Silicon test rig needed for MLX path.
- **Phase Boot hardware boot loop** — pre-existing November 2025 work; carry-forward from `tasks/2025-11/`. Real-hardware kexec on 2009 MacBook x86_64 was demonstrated; broader hardware matrix remains aspirational.

---

## Blockers & Risks

### Current Blockers
- Repeated backend startup: the full workspace passes, but stress repeats still intermittently fail while binding the fixture server. Resolve the race and repeat qualification; diagnostics and failed attempts are retained. Further investigation is at the AGENTS.md retry/time-budget gate.
- Physical acceptance: install fingerprinted test actors/backends, arrange scoped native Phase ingress, verify the actual consumer-NAT topology and record the complete intended stream. The former M1 contributor is not currently reachable through its documented alias.
- Correctness validation, whole-model control measurements, real MLX, Linux ARM64 and remaining operational qualification remain open as described by the authoritative release tracker.

### Risks
- **First-token latency over WAN**: DHT-routed inference may have unacceptable TTFT. Mitigation: stream tokens, local-only fast path.
- **Prompt visibility**: Serving operators can read remote prompts. Mitigation: honest disclosure and local-only operation; stronger confidentiality requires a separate construction and qualification, not transport encryption alone.
- **Ollama API drift**: Upstream API moves faster than LUCID can track. Mitigation: pin to a specific API version, document drift.

---

## Recent Achievements

### 2026-05-27: Phase Core M8 — verification and docs

- Clippy clean across all seven phase-* crates and plasm. Ten warnings carried over in `crates/plasm/src/bin/` from the legacy daemon (unused imports, `is_multiple_of` idiom, collapsible-match in the discovery event loop) all resolved.
- README rewritten to reflect the post-M7 layout: eight crates, dep graph, quick-start with `plasmd` and `hello.wasm`.
- `cargo publish --dry-run -p phase-identity` succeeds. Other phase-* crates have path deps pinned with `version = "0.1.0"` ready for sequential crates.io publication.
- Memory Bank updated: activeContext, progress, decisions, May 2026 task summary.

### 2026-05-26: Phase Core M7 — Plasm repositioned, daemon/ removed

- Top-level `daemon/` directory deleted via `git mv` (history preserved).
- All daemon source moved to `crates/plasm/src/`; new `WasmtimeWorker` in `crates/plasm/src/worker.rs` impls `phase_protocol::Worker`.
- PHP SDK dual-format migration: new canonical `phase-receipt:v1:` + canonical JSON of `{completed_at, job_id, result, schema_version}`, with legacy SHA-256 over `version|module_hash|exit_code|wall_time_ms|timestamp` still accepted.
- Hello.wasm output `dlroW ,olleH` verified byte-identical.

### 2026-05 earlier: M1–M6 substrate extraction

- M1 workspace scaffold (eight crate skeletons).
- M2 phase-net with libp2p 0.57 upgrade.
- M3 phase-identity with on-disk persistent Ed25519.
- M4 phase-protocol Worker trait + JobSpec, validated against fake streaming worker and real Ollama client before extraction.
- M5 phase-manifest and phase-receipt as generic `SignedManifest<T>` / `SignedReceipt<T>`.
- M6 phase-artifact-server extraction.

See [memory-bank/releases/phase-core/](releases/phase-core/) for the full release plan and [memory-bank/tasks/2026-05/](tasks/2026-05/) for per-milestone notes.

---

## Completed Releases

### Phase Open MVP COMPLETE (Nov 2025)
Local WASM execution, peer discovery via Kademlia DHT, remote execution with signed receipts, Debian packaging. 80 tests, 5,743 lines Rust. See [progress.md](progress.md#release-milestones).

### Phase Boot IMPLEMENTED (Nov 2025)
Bootable USB/VM netboot system. M1–M7 complete: boot stub, discovery, verification, kexec, packaging, plasm integration, documentation. Self-hosting loop proven on Fedora kernel in QEMU ARM64 + x86_64 and on 2009 MacBook hardware. See [progress.md](progress.md).

### Netboot Provider COMPLETE (Nov 2025)
HTTP boot artifact server with DHT/mDNS advertisement. M1–M6 complete, 2,510 lines Rust. Now lives in `crates/plasm/src/provider/`.

### Phase Core COMPLETE (May 2026)
Substrate extraction. Seven publishable Phase library crates (Apache-2.0) + Plasm reference WASM node. Documented in [memory-bank/releases/phase-core/](releases/phase-core/).

---

## Next Actions (Priority Order)

1. Review and commit the current `feat/lucid-consumer-experience` working tree, then cut a prerelease candidate (e.g. `v0.2.0-rc1`) via `.github/workflows/release.yml` so every machine installs verified release archives with zero manual copying.
2. Copy `.phase-fleet.env.example` to `.phase-fleet.local.env` (gitignored), run `scripts/fleet-install.sh --tag v0.2.0-rc1` (or `scripts/install-lucid.sh` per host), and collect **Stage 0** qualification evidence on **Mac M5 Max (128GB RAM)**, **Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)** (`PHASE_LINUX_X86_64_EVIDENCE`), and an **Ubuntu ARM64 VM** on Mac M5 Max (`PHASE_LINUX_AARCH64_EVIDENCE`).
3. Run **Stages 1–3** of the [physical fleet soup-to-nuts plan](releases/lucid-v0.2/intended-stream-vertical-slice.md#soup-to-nuts-physical-fleet-test-plan--2026-11-22): open scoped TCP `4001` and start `--mode infrastructure` on **Cloud Relay**, start CUDA `llama-cpp` contributor + invitation issuance on **Ubuntu Desktop (5060ti 16GB, 32GB RAM)** (behind NAT), and put **Mac M5 Max (128GB RAM)** on cellular hotspot to test `LUCID.app` enrollment, forced relay-only live streaming, verified receipt, and multi-GB `/api/pull` interrupt+resume (with **Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)** as Content Provider #2).
4. Run **Stage 4** on **Mac M1 Max (32GB RAM)** (behind home NAT): execute real Apple Silicon MLX acceptance (`--worker mlx`) locally and remotely from Mac M5 Max via Cloud Relay, then run Ubuntu Desktop (CUDA) + Mac M1 Max (Metal) together for two-contributor failover and the whole-model independent-job baseline.
5. Run **Stages 5–6**: complete failure/cancellation/policy/security injection drills, verify clean teardown and healthy standing `Cloud Relay (Caddy) → Tailscale → Ubuntu Desktop (Caddy)` web ingress, and archive sanitized evidence under `target/phase-validation/` plus a new task doc in `memory-bank/tasks/`.
6. Provision a second independent static-IP cloud relay site for multi-site loss and DNS-rotation drills (`network-operations.md`), then specify/implement the bounded correctness-evidence validator seam before revisiting `ShardWorker`.

---

## Team Context

**Role**: Solo developer (Rust + Swift + PHP).
**Availability**: Part-time.
**Timeline**: No hard deadlines; quality over speed.

**This document is updated at milestone boundaries. Last review: 2026-11-22.**
