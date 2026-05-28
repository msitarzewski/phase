# May 2026 — Phase Core Substrate Extraction

## Month Overview

The repository had been dormant since 2025-11-30. May 2026 reopened it with a
single goal: take the working `daemon/` tree from the November MVP and refactor
it in place into a Cargo workspace of publishable library crates plus a
repositioned reference WASM node, so the LUCID inference flagship can build on
the same substrate as a peer crate rather than a fork.

No new functional code in phase-core. Every line either moved or generalized.
Eight crates, 152 tests passing at the close of phase-core, zero clippy warnings,
legacy `daemon/` gone. A spike on the LUCID inference daemon (`crates/lucidd`)
landed at the same time to validate the streaming Worker trait against a real
Ollama client.

The same sprint then carried straight into LUCID: M2 (`LlamaCppWorker`), M5
(local-or-DHT router), M6 (model registry on DHT), and M7 (policy + auto-pause)
all shipped on May 27. M4 (Ollama API surface) is demo-sufficient via the spike
plus router integration. M3 (`MlxWorker`) deferred. M8 (live two-node demo) is
hardware-blocked. At the end of the sprint: **210 tests passing,
`cargo clippy --workspace --all-targets -- -D warnings` clean, ~20K LOC across
eight crates.**

---

## Pre-work

### Design streaming Worker trait for phase-protocol

Drafted the trait shape with explicit streaming primitives before any
extraction: `Worker::execute` returns `(JobHandle, JobStream)`,
`JobStream: Stream<Item = JobChunk>`, receipts produced at stream-close via a
commitment accumulator. Rationale captured in
[memory-bank/decisions.md#2026-05-22-streaming-worker-trait-with-commitment-accumulator-signing](../../decisions.md).

### Validate Worker trait against fake streaming worker + real Ollama client

Built a fake `Worker` impl that emits chunks on a timer, and exercised it from
a real Ollama-shaped HTTP client. Confirmed the trait shape supports both
synchronous one-shot (WASM) and asynchronous streaming (inference) workloads
without per-job-kind branching at the network layer.

### Research libp2p current stable, Ollama API surface, llama-server subprocess interface

Captured in [memory-bank/releases/phase-core/research-brief.md](../../releases/phase-core/research-brief.md).

### Audit existing 80 tests, categorize by integration vs unit vs boundary

Captured in [memory-bank/releases/phase-core/test-audit.md](../../releases/phase-core/test-audit.md).
Five new boundary tests added pre-M2 to protect the refactor; libp2p upgraded
0.54 → 0.57 in `daemon/` before extraction began so the upgrade and the
extraction did not interact.

---

## Phase Core Milestones

### M1: Workspace scaffold

Root `Cargo.toml` with `resolver = "2"` and eight workspace members. Per-crate
`Cargo.toml` skeletons with SPDX license headers (Apache-2.0 for phase-* and
plasm; AGPL-3.0-or-later reserved for lucidd). `cargo build --workspace`
succeeds against empty crates. Status: complete.

### M2: Extract `phase-net`

Moved `daemon/src/network/{discovery,peer,mod}.rs` into
`crates/phase-net/src/`. Generalized `PeerCapabilities` to drop WASM-specific
fields. Upgraded `rust-libp2p` 0.54 → 0.57 (the upgrade happened in `daemon/`
before extraction; see pre-work). All discovery tests pass against the new
crate location. Status: complete.

### M3: Extract `phase-identity` with persistent keys

New crate owning Ed25519 keypair load/save logic. Platform-aware default path
via the `dirs` crate (`$XDG_CONFIG_HOME/phase/identity.key` on Linux,
`~/Library/Application Support/phase/identity.key` on macOS,
`%APPDATA%\phase\identity.key` on Windows). Fixes the long-standing
ephemeral-key bug noted in `progress.md` — daemons now keep the same
`PeerId` across restarts. Status: complete.

### M4: `phase-protocol` crate with validated Worker trait

New crate defining `JobSpec::{Wasm, Inference}`, `JobSpecKind` for dispatch,
the streaming `Worker` trait, `JobStream` + `JobHandle`, `JobChunk`,
`WorkerError`, and the `DynWorker` object-safe shim. Trait shape used the
pre-work validation as ground truth. Plasm's `WasmtimeWorker` and LUCID's
echo spike both implement `Worker` cleanly. Status: complete.

### M5: Extract `phase-manifest` and `phase-receipt` as generic types

Consolidated `daemon/src/wasm/manifest.rs` and `daemon/src/provider/manifest.rs`
into `crates/phase-manifest/` as `SignedManifest<T>` generic over payload.
Receipts moved to `crates/phase-receipt/` as `SignedReceipt<T>` generic over
result. The commitment accumulator lives here; `SignedReceipt::sign_chunks(...)`
takes the accumulator state and produces an Ed25519 signature over
`(job_id, chunk_count, chunk_tree_root, exit_code, wall_time_ms, completed_at)`.
All pre-existing manifest/receipt tests pass. Status: complete.

### M6: Extract `phase-artifact-server`

Moved `daemon/src/provider/{server,artifacts,metrics}.rs` (and supporting
DHT/mDNS plumbing) into `crates/phase-artifact-server/`. Generalized artifact
directory layout from channel/arch-specific to blob-id keyed (the channel/arch
form is now one mounting strategy among possible others). Range request
support preserved. Manifest signing integrates via `phase-manifest`. Status:
complete.

### M7: Reposition Plasm as `crates/plasm/`

The big move. `daemon/` deleted; all source migrated to `crates/plasm/src/` via
`git mv` so history is preserved. New `crates/plasm/src/worker.rs` defines
`WasmtimeWorker` which impls `phase_protocol::Worker` and emits
`SignedReceipt<JobResult>`. Legacy job-shape code (`wasm::{runtime, manifest,
receipt}`) is retained for backward compatibility with the `plasmd execute-job`
CLI path. PHP SDK migrated to dual-format signing (`phase-receipt:v1:` +
canonical JSON for new, legacy SHA-256 still accepted) — see
[decisions.md#2026-05-26-php-sdk-dual-format-signing-migration](../../decisions.md).
Hello.wasm output `dlroW ,olleH` verified byte-identical to pre-refactor.
Status: complete.

### M8: Verification, docs, and old daemon/ removal

- `cargo build --workspace` clean.
- `cargo test --workspace` — 152 tests passing.
- `cargo clippy --workspace --exclude lucidd --all-targets -- -D warnings`
  clean. Ten clippy warnings carried over from the legacy `daemon/` bins were
  resolved in `crates/plasm/src/main.rs`,
  `crates/plasm/src/bin/phase_fetch.rs`, `crates/plasm/src/bin/phase_discover.rs`,
  plus housekeeping for `phase-identity`, `phase-manifest`, `phase-receipt`,
  `phase-protocol`, and `crates/plasm/src/{wasm/runtime.rs, provider/config.rs}`.
  `lucidd` has one pre-existing `explicit_counter_loop` warning held for the
  next release per the no-touch constraint on that crate.
- `cargo publish --dry-run -p phase-identity` packages cleanly. Path deps
  across the substrate are pinned with `version = "0.1.0"` so each crate is
  publish-ready.
- Top-level README rewritten for the post-M7 layout: eight crates, dep graph,
  quick-start.
- Memory Bank updated: `activeContext.md`, `progress.md`, `decisions.md`, and
  this monthly summary.
- Legacy `daemon/` directory confirmed removed from the working tree.

Status: complete.

---

## LUCID Milestones

### LUCID M1: `crates/lucidd/` scaffold + EchoWorker spike

Scaffolded AGPL-3.0-or-later with path deps on the six phase-* crates.
`EchoWorker` streams a reversed prompt one character per chunk through the
`phase-protocol::Worker` trait. Validated against the real `ollama` CLI v0.24
— `ollama run echo 'hello world'` returned `dlrow olleh` character-by-character.
This is also where the Ollama HTTP surface (`/api/chat`, `/api/generate`,
`/api/tags`, `/api/show`, `/api/version`) was first built and proven against a
production Ollama client. Status: complete.

### LUCID M2: `LlamaCppWorker`

`llama-server` subprocess management with:
- One supervisor task per loaded model, `tokio::select!` over `child.wait()` +
  30s `/health` poll
- 3-crash/60s circuit-break with backoff (1s, 2s, then giveup → evict)
- Per-request idle timeout (configurable; default for hang detection)
- Per-request cancellation between SSE frames via `JobHandleProducer::is_cancelled`
- stdout/stderr drain tasks so a chatty subprocess can't block on full pipe
- `kill_on_drop(true)` on every `Command`; supervisor handles aborted on Drop

Test strategy: `crates/lucidd/tests/fixtures/fake_llama_server.rs` is a real
Rust binary declared as `[[bin]]` so cargo provides its path via
`env!("CARGO_BIN_EXE_fake-llama-server")`. Env-var configurable per-spawn for
warmup, hang-after-N-tokens, crash-after-N-ms, fail-health behaviors. Tests
cover happy path, mid-stream cancellation, hang detection, load failure as
`WorkerError`, missing-model file as `ArtifactUnavailable`. Real-binary
integration test gated `#[ignore]` on `LLAMA_SERVER_PATH`.

Status: complete.

### LUCID M3: `MlxWorker`

**Deferred to v0.1.1.** Apple Silicon native inference via `mlx-lm`
subprocess. Requires Apple Silicon test rig for full validation. Not on the
v0.1 demo critical path.

### LUCID M4: Ollama API surface on `:11434`

**Demo-sufficient.** Covers `/api/chat`, `/api/generate`, `/api/tags`,
`/api/show`, `/api/version`, fallback 404 with warning log on anything else.
NDJSON streaming (`application/x-ndjson`), terminal frame carries
`x_phase_commitment` so the receipt commitment is delivered in-band (HTTP
headers can't carry post-stream data; this was caught and locked in during
the M1 spike validation).

**Deferred**: `/api/embeddings`, `/api/pull`. Not required for the demo;
v0.1.1 work alongside Open WebUI compat testing. Status: demo-sufficient.

### LUCID M5: Local-or-DHT Router

`Router::route` decision order:
1. `local_only && (no local worker || model not loaded locally)` → Refused
2. `PolicyEngine::should_serve == Pause` → Refused with PauseReason as reason
3. local worker has model loaded → Local
4. else `ModelRegistry::find_peers_by_model_id` → first valid peer → Peer
5. no peers → Refused

`X-Lucid-Local-Only` request header parsed; `X-Lucid-Routed-Via` response
header set (`local` or `peer:<8-char-suffix>`); 503 + reason on Refused.

Peer relay: libp2p `request_response::cbor::Behaviour` on
`/phase/job-relay/1.0.0` (5-minute timeout). Wire format: request =
`JobRelayRequest { payload: bincode(SignedManifest<JobSpec>) }`, response =
`JobRelayResponse::Ok { events: bincode(Vec<JobEvent>) }` or `Err { reason }`.

Also added to phase-net: `pub async fn get_kad_record(&self, key: Vec<u8>)`,
plus a refactor of `publish_kad_record` from `&mut self` to `&self` so the
registry can hold an `Arc<Discovery>`.

Receipt note: peer-served full `SignedReceipt<JobResult>` does NOT propagate
back through the relay in v0.1 — only the output commitment rides in
`JobEvent::Final.result.output_commitment`. Streamed peer receipts + multi-peer
retry both deferred to v0.2.

Status: complete.

### LUCID M6: Model Registry on DHT

`SignedModelAdvertisement` records, bincode-encoded. Key:
`b"phase/model/" || model_cid` (44 bytes total). Value: bincode-encoded
`{ schema_version, ModelCapabilities, pubkey, signature }` where signature
is Ed25519 over the canonical bincode of all non-signature fields.

`ADVERTISEMENT_SCHEMA_VERSION = 1`,
`ADVERTISEMENT_TTL = 15 min`, `TTL_REFRESH_INTERVAL = 5 min`.

13 tests: round-trip + verify, tamper detection on caps/pubkey/signature,
schema version mismatch rejection, `advertise_loaded` issues exactly one
put, `local_models` reflects advertise + withdraw, TTL refresh re-publishes,
withdraw stops the refresh task.

`DhtTransport` trait serves as the M5 wiring seam.

Status: complete.

### LUCID M7: Policy + Auto-Pause

Declarative `~/.config/lucidd/policy.toml` with full default file written on
first run. Defaults: `auto_pause_on_battery=true`,
`auto_pause_on_thermal_threshold_c=75.0`, `vram_reserve_gb=4.0`,
`serve_models=["*"]`, `manual_pause=false`, `max_concurrent_remote_jobs=4`,
no time-of-day window.

Decision order: Manual → OnBattery → ThermalLimit → OutsideTimeWindow →
ConcurrencyLimit → ModelNotInAllowlist → Allow.

State detection: battery via the `battery` crate (macOS IOKit / Linux sysfs;
None on Windows), thermals via `sysinfo`. State probed every 30s in a tokio
background task via `tokio::task::spawn_blocking`. Config reload via
`notify::recommended_watcher` on the parent directory + `tokio::signal::unix`
SIGHUP handler (Unix only).

24 unit tests cover every PauseReason variant + Allow + glob matching +
TimeWindow wrap-around + TOML round-trip + reload.

Status: complete.

### LUCID M8: Live two-node end-to-end demo

**Hardware-blocked.** Software ready. Acceptance requires Machine A
(Linux + 24GB+ NVIDIA GPU + real llama-server + a GGUF model) and Machine B
(any second machine), Tailscale-bridged or real WAN. Demo recipe:

```bash
# Machine A
lucidd --worker llama-cpp --model-dir /opt/models --port 11434

# Machine B
lucidd --no-local-worker --port 11434

# From anywhere on Machine B
curl -N http://localhost:11434/api/chat \
  -d '{"model":"qwen3-next-80b-q4","messages":[{"role":"user","content":"hi"}],"stream":true}'
# → routes through DHT to A, streams tokens back, terminal frame carries x_phase_commitment
```

Final asciinema recording is the artifact for the eventual launch.

---

### 2026-05-28 (late): v0.2 substrate prep + first foundation relay

After the live LAN demo earlier in the day, the same session brought the substrate from "works on a LAN" to "works on the public internet" and stood up the first 24/7 relay node.

**Changes**:
- Wired `phase-net::DiscoveryConfig::bootstrap_peers` to actually dial (was a logged no-op since November)
- Added lucidd flags: `--bootstrap-peer` (repeatable), `--libp2p-port`, `--identity-path`, `--mode {worker,relay}`
- Default identity is now persistent (`NodeIdentity::load_or_create` at `~/.config/phase/identity.key`) instead of fresh-per-restart
- Dual-stack libp2p listen: `/ip4/0.0.0.0/tcp/<port>` AND `/ip6/::/tcp/<port>`
- Added user-level systemd unit at `crates/lucidd/systemd/lucidd-relay.service`
- New `x86_64-unknown-linux-gnu` dist target (previously only had `aarch64-apple-darwin` and `aarch64-unknown-linux-gnu`)

**First foundation relay live on umbp** (Ubuntu 24.04 x86_64, 10Gb sync Sonic fiber):
- peer_id `12D3KooWJ6vTjo6yFgEc9YbFWp8hd3JYfpaE2CxhYKvWcPozaNJB`
- multiaddr `/ip4/76.191.195.7/tcp/4001/p2p/12D3KooWJ6vTjo6yFgEc...`
- managed by `lucidd-relay.service` (user systemd, `Linger=yes` for boot survival)
- DNS record added (subdomain → `76.191.195.7`)
- Verified reachable from a fresh lucidd instance via `--bootstrap-peer` in tens of ms

**What's NOT in this session**: libp2p circuit-relay server protocol (`relay::server::Behaviour`), DCUtR hole-punching, libp2p rendezvous, DNS-based bootstrap. Those are the real v0.2 substantive substrate work, scoped for the next dedicated session.

---

## Totals

- **Phase Core**: 8 of 8 milestones shipped (M1–M8).
- **LUCID software**: 5 of 7 milestones shipped (M1, M2, M4 demo-sufficient, M5,
  M6, M7); M3 deferred to v0.1.1; M8 hardware-blocked.
- 8 crates in the workspace at sprint end.
- **210 tests passing**; `cargo clippy --workspace --all-targets -- -D warnings`
  clean across every crate (including lucidd).
- 0 lines of legacy `daemon/` remaining (history preserved via `git mv`).
- ~20,000 lines of Rust across the workspace.
