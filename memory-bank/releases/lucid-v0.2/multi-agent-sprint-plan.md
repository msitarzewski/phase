# LUCID v0.2 — Multi-Agent Divide & Conquer Sprint Plan

**Updated:** 2026-11-27 (re-sequenced after Claude Opus 5.5's security, lifecycle, and architecture audit)
**Base Branch:** `feat/lucid-consumer-experience`
**Core Principle:** **Fix P0 browser-reachable security holes, P1 process-lifecycle/routing bugs, and scorecard honesty FIRST (Wave 1)** before landing new feature work (Wave 2) or tagging `v0.2.0-rc9` / opening the branch PR.

---

## Non-Negotiable Ground Rules (Every Agent Must Follow)

1. **Privacy / Opsec Hard Rule**:
   - **NEVER** commit private hostnames, SSH aliases, usernames, Tailscale/private IPs, tokens, or raw prompts/completions to any tracked file or commit message. Use only hardware role names (**Cloud Relay**, **Ubuntu Desktop**, **Ubuntu Laptop**, **Mac M1 Max**, **Mac M5 Max**).
2. **Substrate Neutrality (`memory-bank/decisions.md:854-875`)**:
   - `crates/phase-*` crates (`phase-net`, `phase-protocol`, `phase-manifest`, `phase-receipt`, `phase-identity`, `phase-artifact-server`) MUST remain workload-neutral (opaque bytes only — no model/token/Ollama/backend-name hardcoding).
3. **Security & Honesty Invariants (Fail-Closed)**:
   - `#![deny(unsafe_code)]` across all crates and Tauri backend.
   - `ShardTrustTier::UntrustedSpotChecked` remains fail-closed pending ADR sign-off.
   - `cluster_enabled = false` and `allow_unauthenticated_jobs = false` by default.
   - All local inference subprocesses (`llama-server`, `ggml-rpc-server`, `mlx_lm.server`) MUST bind strictly to `127.0.0.1` inside `env_clear()`, with per-process API auth / CORS lockdown so browsers cannot bypass `lucidd`.
   - Never count local runs as remote peer-verified receipts; distinguish `peer-verified receipts`, `local commitments`, and `chained stage receipts` explicitly.
4. **Verification Gate Before Merging Any Track**:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   # If touching apps/lucid-desktop:
   (cd apps/lucid-desktop && npm run check && cargo test --manifest-path src-tauri/Cargo.toml --locked)
   ```

---

## WAVE 1 — P0 Security, P1 Lifecycle/Routing Fixes, & Sharding Guardrails (EXECUTE FIRST!)

Wave 1 splits into **two strictly disjoint code tracks** (zero overlapping files) plus **Track C** (docs/honesty reconciliation & fleet re-qualification).

### Track A — Subprocess Security, Worker Lifecycle, RPC Guardrails, & `phase-net` Stream Lock
**Assigned Agent:** **Claude Opus 5.5**
**Branch:** `fix/wave1-worker-net-lifecycle`

#### Exclusive File Ownership
- `crates/lucidd/src/worker_llama/**`
- `crates/lucidd/tests/llama_worker.rs`
- `crates/lucidd/src/worker_mlx/**`
- `crates/phase-net/src/discovery/**`
- `crates/lucidd/src/main.rs` *(CLI flags & startup validation only)*

#### Deliverables
1. **A1 (P0 Security) — Lock Down `llama-server` Against Browser Access (`worker_llama/mod.rs:836-860`)**:
   - Generate a cryptographically random per-spawn API key (`ring::rand::SystemRandom`), pass via `LLAMA_API_KEY` env var (never argv!), and attach `Authorization: Bearer <key>` on `LlamaCppWorker`'s internal `reqwest::Client`.
   - Pass verified `llama-b12651` hardening flags: `--cors-origins localhost --no-cors-credentials --no-webui --no-slots`.
   - Add integration test proving an unauthenticated request to `llama-server`'s loopback port is rejected (`401`).
2. **A2 (P1 Lifecycle) — Kill Hung `llama-server` Processes & Fix Port/Eviction Leaks (`worker_llama/mod.rs:1495-1513`)**:
   - Wire an explicit shutdown/kill signal (`Notify` / `watch`) into the model supervisor task so 30s hang detection immediately kills the hung `llama-server` child process and releases its port cleanly (preventing silent VRAM/RAM leaks to OOM).
   - Replace manual `release_port` error arms with an RAII `PortGuard` so aborted/cancelled cold loads NEVER leak ports from `18080..18200`.
   - Defer LRU warm-model eviction until cold-load admission is committed so failed/cancelled cold loads don't evict healthy warm models.
3. **A3 (P1 RPC Guardrails) — Gate `--llama-rpc-servers` Safely (`worker_llama/mod.rs`, `main.rs`)**:
   - Fail fast at startup if `--llama-tensor-split` is passed without `--llama-rpc-servers`.
   - Gate RPC layer offload behind `cluster_enabled = true` (and refuse RPC-backed models when a request sets `X-Lucid-Local-Only`), reporting the route/trust label honestly as `trusted-circle-sharded` rather than `local`.
4. **A4 (P1 Transport) — Remove Global Stream Lock & Substrate Leak (`phase-net/src/discovery/mod.rs:1055, 1478-1489, 3723`)**:
   - Drop the shared `Mutex` serializing `libp2p_stream::Control::open_stream` (`Control` is `Clone`) so one dead peer cannot stall outbound dials to healthy peers for up to 10s.
   - Remove hardcoded `"llama" | "mlx" | "ollama"` backend strings from `phase-net/src/discovery/mod.rs:3723` (keep `phase-net` workload-neutral).
5. **A5 (P1 MLX Lifecycle)**:
   - Update `worker_mlx/mod.rs:62` stale `"unverified: no real Metal/model acceptance run is recorded"` receipt metadata to accurately reflect the hardware-verified MLX adapter (while still never claiming third-party compute proof).
   - Avoid unnecessary full 36s re-hash on clean request cancellation/restart where immutable bundle identity/inode checks already hold.

---

### Track B — Browser DNS Rebinding Guard, Consumer Authz, `:latest` Local-First Routing, & `bench.rs` Honesty
**Assigned Agent:** **GPT 6.2 Sol**
**Branch:** `fix/wave1-http-routing-consumer-authz`

#### Exclusive File Ownership
- `crates/lucidd/src/ollama/**`
- `crates/lucidd/src/router/**`
- `crates/lucidd/src/consumer.rs`
- `crates/lucidd/src/bench.rs`
- `crates/lucidd/tests/http_api.rs`

#### Deliverables
1. **B1 (P0 Security) — Strict `Host`-Header Validation Against DNS Rebinding (`ollama/mod.rs`)**:
   - Add Axum middleware on `ollama::router` rejecting any request whose `Host` header is not `localhost`, `127.0.0.1`, or `[::1]` (with optional `:port`) with `403 Forbidden` (when bound to loopback). Blocks DNS-rebinding pages from calling `/api/pull` (up to 128 GiB download!) or `/api/chat`.
   - Add boundary tests in `crates/lucidd/tests/http_api.rs` proving `Host: evil.example:11434` fails closed (`403`) while `Host: 127.0.0.1:11434` and `Host: localhost:11434` pass.
2. **B2 (P0 Security) — Default-Deny Consumer Remote Routing (`consumer.rs`, `router/mod.rs:457`)**:
   - Persist enrolled/approved issuer PeerIDs in `ConsumerControl` / `EnrollmentStore` (`authorized_issuers`), and restrict `LucidRouter` so consumers route prompts **only to explicitly enrolled or `--pin-peer` peers by default** — never leaking prompts to arbitrary un-enrolled DHT advertisers!
   - Provide an explicit opt-in flag (`--allow-open-peers` / config bool, default `false`) for open-network testing. Add unit tests proving un-enrolled DHT peers are excluded by default.
3. **B3 (P1 Routing) — Fix `:latest` Local-First Routing & Cold-Load Keepalives (`ollama/mod.rs:2096`, `router/mod.rs`)**:
   - Check locally loaded models (matching both `<model>` and `<model>:latest`) BEFORE stripping `:latest` or consulting the DHT, so locally loaded models stay local and bare aliases published by remote peers cannot hijack fleet traffic.
   - Emit periodic accepted/loading keepalive frames on `/phase/job-relay/2.0.0` while a serving worker cold-loads a large or RPC-sliced model so remote cold loads > 30s succeed cleanly without timing out.
4. **B4 (P1 Honesty & Flaky Test Fix)**:
   - Fix `crates/lucidd/src/bench.rs:244` so local-route runs report `local_commitments` instead of incrementing `verified_receipts` (and note self-reported `eval_count` clearly).
   - Fix the intermittent `AddrInUse` flake in `crates/lucidd/tests/http_api.rs:108` using the disjoint port allocator pattern from `crates/lucidd/tests/llama_worker.rs:50-80`; verify 15/15 consecutive passes.

---

### Track C — Honesty Reconciliation, Sharding Decision, Fleet Verification, & PR Cut
**Assigned Agent:** **Antigravity / Operator**
**Owned Files:** `memory-bank/**`, `README.md`, `crates/lucidd/README.md`, release scripts, private fleet dashboard.

1. **Completed immediately (before Wave 1 starts)**:
   - Reconcile exact receipt sums (`122/122` remote peer-verified receipts across Runs #1–#5B, #7A–#8, #10A–#12D; `6/6` local commitments in Run #6; `2/2` chained stage receipts in Run #9 protocol stub), remove misleading CPU-fallback headline multipliers, fix historical rename glitches (`activeContext.md`), and explicitly clarify Run #9 (protocol stub) vs Runs #11–#12 (real sliced compute).
2. **After Wave 1 (Tracks A + B) merge**:
   - Run full workspace CI, cut **`v0.2.0-rc9`**, deploy fleet-wide across all 5 machines, and **open a clean reviewed PR** for `feat/lucid-consumer-experience`!

---

## WAVE 2 — Architecture & Release-Gate Features (AFTER Wave 1 + PR Review)

Once Wave 1 is merged and `v0.2.0-rc9` is green:
1. **Consolidated Tier-1 Sharding Design (ADR Decision)**:
   - Unify Tier-1 Trusted Circle sharding on **Noise-encrypted libp2p `/phase/shard-rpc/1.0.0`** carrying `llama.cpp` `ggml-rpc` strictly between enrolled peers (`cluster_enabled = true`, loopback-only `ggml-rpc-server -c` inside `env_clear()`, explicit full-trust disclosure & `llama.cpp` CVE posture, coordinator-signed `trusted-circle-sharded` receipts).
   - Explicitly decide via ADR whether `/phase/shard-stage/1.0.0` remains reserved for Tier-2 research or is superseded.
2. **Public Rendezvous Hard Quotas (`phase-net`)**:
   - Global / per-peer / per-namespace admission caps + flood tests so Cloud Relay can safely enable rendezvous discovery.
3. **Independent Correctness-Evidence Validator (`phase-protocol`, `reputation.rs`, `router`)**:
   - Bounded opaque evidence envelope + independent deterministic second-peer spot-check validator (`X-Lucid-Trust-Label`).
4. **Honest `/api/ps` Residency + `apps/lucid-desktop` Cluster UX**:
   - Report real resident worker models in `/api/ps`; surface pooled GPU memory and `trusted-circle-sharded` badges in Tauri v2 Desktop (and decide whether to deprecate duplicate `apps/lucid-macos` SwiftUI app).
5. **Physical Fleet Closure**:
   - Reboot Ubuntu Desktop to clear `NVRM Status 62` → RTX 5060 Ti CUDA 12.8 bench + 3-GPU load spread + sliced `80.76 GB` `DeepSeek-V4-Flash` across all 3 GPUs + Linux ARM64 VM qualification (`PHASE_LINUX_AARCH64_EVIDENCE`) + 100-stream Cloud Relay saturation test.
