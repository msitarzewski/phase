# Active Context: Current Sprint

**Last Updated**: 2026-05-27
**Sprint**: LUCID inference flagship — software complete, demo hardware-blocked
**Status**: Phase Core M1-M8 complete; LUCID M1, M2, M4 (demo-sufficient), M5, M6, M7 complete; M8 hardware-blocked.

---

## Current Focus

Phase Core and the LUCID software stack have both shipped in a single sprint. Eight crates build clean; 210 tests pass workspace-wide; zero clippy warnings on `cargo clippy --workspace --all-targets -- -D warnings`. The 0→1 live two-node demo (LUCID M8) is the only outstanding work, gated entirely on hardware acquisition (a Linux box with a 24GB+ NVIDIA card plus a second networked machine).

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

---

## Phase Core COMPLETE (May 2026)

- [x] **M1**: Workspace scaffold — root Cargo.toml, eight crate skeletons, SPDX headers, empty `cargo build --workspace` green.
- [x] **M2**: Extract `phase-net` — libp2p 0.54 → 0.57 upgrade, Kademlia + mDNS + Noise+QUIC, generic peer capabilities decoupled from WASM-specific fields.
- [x] **M3**: Extract `phase-identity` — persistent Ed25519 keypair on disk via `dirs` crate, fixes the ephemeral-key bug carried over from the November MVP.
- [x] **M4**: `phase-protocol` — `JobSpec` enum with `Wasm` and `Inference` variants, streaming `Worker` trait with `JobHandle` + `JobStream`, `DynWorker` object-safe shim.
- [x] **M5**: Extract `phase-manifest` + `phase-receipt` — generic `SignedManifest<T>` / `SignedReceipt<T>` envelopes, Ed25519 signing reused from `phase-identity`, commitment-accumulator chunk hashing for streamed results.
- [x] **M6**: Extract `phase-artifact-server` — blob-id keyed layout, range request preserved, signed manifest integration.
- [x] **M7**: Reposition Plasm — `daemon/` deleted via `git mv`, `WasmtimeWorker` impls `phase_protocol::Worker`, PHP SDK migrated to dual-format signing (legacy + `phase-receipt:v1:`), hello.wasm output byte-identical.
- [x] **M8**: Verification, docs, daemon removal — `cargo build --workspace` clean, 152 tests pass, zero clippy warnings across phase-* and plasm, `cargo publish --dry-run -p phase-identity` packages cleanly, README rewritten for post-M7 layout, Memory Bank updated.

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

## Honest v0.1 Limitations (all v0.2 work, none demo-blocking)

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

The two-node demo ran end-to-end on real hardware. Mac M5 Max (128GB) hosting Qwen3-Next 35B-A3B on Metal via `llama-server`; Parallels Ubuntu ARM64 VM at `10.211.55.5` with `lucidd --no-local-worker`. `curl http://localhost:11434/api/chat` from inside the VM routed via libp2p + Kademlia DHT, served on the Mac, streamed back NDJSON with `x-lucid-routed-via: peer:ctCUGwkd` and `x_phase_commitment: <sha256>` in the terminal frame. Asciinema captured at `dist/demos/lucid-2node-demo.cast`.

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
host:           umbp (Ubuntu 24.04 x86_64, Intel i7-3720QM, 15GB RAM, 10Gb sync fiber)
peer_id:        12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB
public mAddr:   /ip4/76.191.195.7/tcp/4001/p2p/12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB
service:        lucidd-relay.service (user-level systemd)
                ├── enabled (boot-start)
                ├── linger=yes (survives logout)
                └── auto-restart on failure
identity file:  ~/.config/phase/identity.key  (32 bytes, mode 600)
DNS:            added 2026-05-28 (subdomain pointing at 76.191.195.7)
```

### What still needs to happen for the "coffee-shop scenario"

The relay is **directly dialable** today, which is enough for any peer to use it as a bootstrap-peer. The full coffee-shop scenario (user behind NAT/VPN reachable through the relay) still needs the libp2p circuit-relay server protocol and DCUtR for hole-punching. That's v0.2 substantive engineering — ~100-200 lines in phase-net plus a real protocol decision about what relay traffic the foundation is willing to carry.

### Next-session checklist for substrate v0.2

- [ ] Wire libp2p `relay::server::Behaviour` into phase-net's `CombinedBehaviour`
- [ ] Wire libp2p `dcutr::Behaviour` for hole-punching
- [ ] Wire libp2p `rendezvous::server::Behaviour` for the model-rendezvous design from MISSION discussion
- [x] **DNS-based bootstrap shipped** — lucidd has `--bootstrap-dns <domain>` (repeatable) that resolves TXT records and dials each as a multiaddr. Validated with `bootstrap.phasebased.net TXT "/ip4/76.191.195.7/tcp/4001/p2p/12D3KooWJ6vTjo6yFgEc..."`. A fresh Mac peer with `--bootstrap-dns bootstrap.phasebased.net` (no explicit `--bootstrap-peer`) dialed umbp in ~70 ms. `hickory-resolver` 0.24 with `system-config` for `/etc/resolv.conf`, Cloudflare/Google fallback for sandboxed envs.
- [ ] Stand up 2-3 more relays in geographically distinct regions (Hetzner CAX11s, $3.79/mo each)
- [ ] Document the relay-operator setup (this systemd unit + DNS-record format)
- [ ] Sonic IPv6 firewall investigation (umbp listens on IPv6 but Mac→IPv6 currently fails — separate debugging)
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
None for LUCID M2 work; pure-software path on commodity Linux + llama.cpp.

### Risks
- **First-token latency over WAN**: DHT-routed inference may have unacceptable TTFT. Mitigation: stream tokens, local-only fast path.
- **Privacy perception**: Per-request prompts visible to serving peer in v0.1. Mitigation: honest disclosure, local-only toggle, v0.3 cryptographic privacy committed.
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

1. Open LUCID M2 — `LlamaCppWorker` subprocess management.
2. Stand up CI for the workspace (currently local-only verification).
3. Schedule LUCID M3 MLX work for next Apple Silicon dev session.
4. Begin sequential crates.io publication of phase-identity → phase-* → plasm once a publication policy is decided.

---

## Team Context

**Role**: Solo developer (Rust + PHP).
**Availability**: Part-time.
**Timeline**: No hard deadlines; quality over speed.

**Knowledge gaps under active development**:
- llama.cpp `llama-server` subprocess lifecycle and signal handling.
- Ollama API SSE stream semantics for `/api/chat`.
- DHT TTL refresh patterns for model registry advertisements.

---

**This document is updated at milestone boundaries. Last review: 2026-05-27.**
