# November 2026 — Task Summary

## Tasks Completed

### 2026-11-26 → 2026-11-27: Physical 5-Node Fleet Qualification (`v0.2.0-rc8`), WAN MLX (`71.3 tok/s`), & Real Sliced 2-Mac LLM Inference (`21.3 tok/s`)
- Deployed `v0.2.0-rc8` (`3eec1de`) + catalog resilience (`a1849f0`) + loopback RPC slicing (`b97f93e`) across all 5 physical fleet nodes (**Cloud Relay**, **Ubuntu Desktop**, **Ubuntu Laptop**, **Mac M1 Max**, **Mac M5 Max**) with **`120/120` (100%) verified Ed25519 receipts** across Runs #1–#12.
- Verified `10.18 GB` P2P pull + `172 MiB` interrupt & exact-offset resume, uninvited DCUtR peer default-deny, 2012 Ivy Bridge baseline `x86-64` fix (`0 SIGILL`), `2,194×` startup catalog restore speedup (`20.19s → 9.2 ms`), `ring` SHA-256 (`7.8×` faster), `model_peers_cache` (`123×` WAN MLX TTFT speedup: `10,458 → 85 ms`), WAN DHT enrollment `QUERY_DEADLINE` fix (`5s → 15s`), `98.0 tok/s` simultaneous 2-Mac burst (Cellular+ProtonVPN M5 Max MLX `71.3 tok/s` + Home M1 Max Metal `28.8 tok/s`), and **first real sliced 2-Mac LLM inference** (`6.98 GB` `gemma-4-12B` across `135.6 GiB` pooled Metal 4: `5.7 → 16.4 → 21.3 tok/s` across Cellular+VPN → 10G+VPN → Direct LAN).
- Created the parallel [multi-agent sprint handoff plan](../../releases/lucid-v0.2/multi-agent-sprint-plan.md) for Tracks A, B, and C.
- See: [261126_physical-fleet-qualification-and-real-sliced-inference.md](./261126_physical-fleet-qualification-and-real-sliced-inference.md).

### 2026-11-25 (Part 2): Tier-1 `ShardWorker` libp2p Transport (`/phase/shard-stage/1.0.0`), GGUF Reader, & Cluster UI
- Added workload-neutral `/phase/shard-stage/1.0.0` opaque substream transport in `phase-net`, bounded pure-Rust GGUF v2/v3 layer-count scanner (`inspect_gguf_shard_metadata`), and Tier-1 Trusted-Circle `ShardWorker` coordinator + default-deny inbound stage handler (`crates/lucidd/src/shard_worker.rs`).
- Added `cluster_enabled` opt-in (`node.toml`), `/api/lucid/cluster` + `/api/lucid/cluster/plan`, `lucidd cluster {status,inspect,plan}`, Desktop **Trusted-Circle Cluster** modal, and real 3-node libp2p integration test (`crates/lucidd/tests/shard_worker.rs`).
- See: [261125_shardworker-transport-and-cluster-ui.md](./261125_shardworker-transport-and-cluster-ui.md).

### 2026-11-25: Ollama/Multi-Folder Serving, Control API Tests, `lucidd bench`, & `ShardWorker` Foundation
- Wired Ollama blob (`sha256-*`), LM Studio, and multi-folder GGUF verification/serving (`import_verified_gguf_file`, `approved_model_sources`, hot-import on `POST /api/lucid/provide`) plus 1-click `POST /api/lucid/pull` in `apps/lucid-desktop`.
- Added real-process HTTP contract tests for `/api/lucid/*` (`401`/`428`/folder sandbox/hot-import/settings) and fixed the `fake_llama_server` `AddrInUse` teardown flake (5/5 repeats green).
- Implemented `lucidd bench` (`crates/lucidd/src/bench.rs`) for the whole-model multi-peer baseline and the Tier-1 `ShardWorker` planner + composite stage-receipt verifier (`crates/lucidd/src/shard_plan.rs`) + full 7-question ADR draft (`memory-bank/releases/lucid-v0.2/shardworker-adr.md`).
- See: [261125_shardworker-foundation-and-bench.md](./261125_shardworker-foundation-and-bench.md).

### 2026-11-23: Apple-Simple Self-Configuring `lucidd` & Cross-Platform Tauri v2 Desktop App
- Built Phase 1 (`crates/lucidd/src/{node_config.rs,runtime_manager.rs,ollama/mod.rs,policy.rs,main.rs}`): read-only hardware/GPU/Ollama/LM-Studio/GGUF probe, persistent `0600` `node.toml`, SHA-256-pinned `llama.cpp b12651` runtime installer, explicit permission-first `/api/lucid/*` control endpoints, zero-flag safe startup (`NodeMode::Auto`), and CLI subcommands (`probe`, `models`, `peers`, `invite`, `join`).
- Built Phase 2 (`apps/lucid-desktop/`): sandboxed cross-platform Tauri v2 + Svelte 5 desktop app (`#![deny(unsafe_code)]`, token isolation from WebView, strict CSP, minimal capabilities, permission-first onboarding, `lucid://` deep links, system tray) producing `.dmg` (macOS Apple Silicon, 14.06 MiB) and `.deb` (Ubuntu x86_64 & ARM64) installers in `.github/workflows/release.yml`.
- Updated `scripts/install-lucid.sh` (`ExecStart` prefix rewrite + `--setup-relay <IP>`) and `.github/workflows/security.yml` (`desktop-tauri` CI job).
- Verification: Rust workspace tests/clippy/audit/deny clean; Tauri `src-tauri` tests/clippy/audit clean; `svelte-check` 0 errors/warnings; signed `LUCID.app` + `LUCID_0.2.0_aarch64.dmg` verified locally.
- See: [261123_apple-simple-lucid-desktop.md](./261123_apple-simple-lucid-desktop.md).

### 2026-11-22: Workspace, Build/CI, and Runtime Efficiency
- Reclaimed ~140 GB net of stale build, cross-target, boot-image, and duplicate validation blob bloat while preserving all qualification logs and source weights; stopped tracking stale v0.1.0 binaries under `dist/`.
- Populated `[workspace.dependencies]` across all 8 Rust crates, eliminating duplicate `tower 0.4`, `tower-http 0.5`, and `native-tls` stacks, removing unused `phase-net`/`phase-manifest` dependencies from `phase-artifact-server`, and narrowing `tokio` feature sets.
- Optimized `scripts/phase-validate.sh` (`xargs -0` batched fingerprinting, non-duplicated `qualification` test steps) and `.github/workflows/security.yml` (dedicated fast `fmt` job, `--locked` Clippy).
- Resolved concrete hot-path bottlenecks: lock-free `DashMap` fast path in `LlamaCppWorker::ensure_loaded`, shared `wasmtime::Engine`/`Linker` + 64-module SHA-256 cache + `StoreLimits` memory cap in `Wasm3Runtime`, `Arc<CompiledPolicy>` precompiled globs + reusable `sysinfo::Components` in `PolicyEngine`, `O(n)` `sanitize_for_log` + async `tokio::fs::metadata` in `ollama`, single-allocation `hex::encode` in `ModelCid::to_hex`, zero-clone `sort_value` in `canonical.rs`, and concurrent `async let` refresh + event-driven shutdown + background polling backoff in macOS `DaemonController`.
- Extracted shared `crates/lucidd/src/backend_io.rs` (eliminating `worker_mlx`'s per-chunk `BytesMut` copy-allocate overhead) and split 7 oversized monoliths (`registry`, `discovery`, `router`, `ollama`, `content`, `worker_mlx`, `worker_llama`) into `<mod>/mod.rs` + `<mod>/tests.rs`.
- Pruned stale November 2025 blockers from `memory-bank/progress.md`.
- Verification: Rust 492 passed / 0 failed / 2 ignored; Swift 11 passed / 0 failed; `cargo fmt`, strict `cargo clippy -D warnings`, `cargo audit`, and `cargo deny` all clean.
- See: [261122_workspace-and-runtime-efficiency.md](./261122_workspace-and-runtime-efficiency.md).
