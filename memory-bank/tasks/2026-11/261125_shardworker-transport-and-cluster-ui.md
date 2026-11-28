# 261125_shardworker-transport-and-cluster-ui

## Objective
1. Refresh all core Memory Bank files (`toc.md`, `progress.md`, `activeContext.md`, `techContext.md`, `releases/lucid-v0.2/README.md`) to reflect the `2026-11-23`/`2026-11-25` state accurately before building.
2. Implement the Tier-1 Trusted-Circle `ShardWorker` network transport, pure-Rust GGUF header layer-count reader, multi-peer coordinator/stage executor, `/api/lucid/cluster` control API, `lucidd cluster` CLI, Desktop Cluster UI, and real 3-node libp2p integration tests while keeping untrusted sharding fail-closed and `phase-net` strictly workload-neutral.

## Outcome
- ✅ Full workspace tests (`cargo test --workspace --locked --offline`): PASS (0 failures), including new 3-node libp2p `shard_worker` integration test and `/api/lucid/cluster` + `/api/lucid/cluster/plan` real-process HTTP tests.
- ✅ `cargo fmt --all --check`, strict `cargo clippy --workspace --all-targets -- -D warnings`, `svelte-check` (0 errors/warnings), `vite build`, and Tauri `src-tauri` tests + strict Clippy: PASS.
- ✅ Local `~/.local/bin/lucidd`, `~/Applications/LUCID.app` (`codesign --verify --deep --strict`), and `LUCID_0.2.0_aarch64.dmg` rebuilt and verified.
- ✅ Tracker honesty preserved: `ShardWorker` milestone remains `🟨 Blocked` in `memory-bank/releases/lucid-v0.2/README.md` pending physical `lucidd bench` baseline + human ADR approval + real `llama.cpp` RPC multi-machine weights run.

## Files Modified
- `crates/phase-net/src/protocol.rs`, `crates/phase-net/src/discovery/mod.rs`, `crates/phase-net/src/lib.rs` — Added workload-neutral bounded `/phase/shard-stage/1.0.0` opaque substream protocol (`ShardStageRequest`, `ShardStageResponse`, `ShardStageHandler`, `Discovery::execute_shard_stage`, `Discovery::set_shard_stage_handler`) reusing `InboundStreamAdmission` and postcard framing.
- `crates/lucidd/src/shard_plan.rs` — Added bounded pure-Rust GGUF v2/v3 header metadata scanner (`inspect_gguf_shard_metadata`, `shard_spec_from_gguf`) using `BufReader::seek_relative` + early-exit after `architecture`/`block_count`/`embedding_length` so multi-GB GGUF files parse in sub-millisecond time without scanning 150k+ tokenizer arrays.
- `crates/lucidd/src/shard_worker.rs` — Added Tier-1 Trusted-Circle `ShardStageExecutor` trait, `LoopbackStageBridgeExecutor`, `make_inbound_shard_stage_handler` (enforcing `PolicyEngine` pause/default-deny caller authz via `is_authorized_submitter`, coordinator signature/expiry, stage identity binding, and SHA-256 input/output receipt signing), and `execute_sharded_pipeline` (chaining local + remote libp2p stages and verifying the complete composite receipt chain).
- `crates/lucidd/src/node_config.rs` — Added default-off `cluster_enabled: bool` opt-in to `NodeConfig`.
- `crates/lucidd/src/ollama/mod.rs` — Added auth-gated `GET /api/lucid/cluster` and `POST /api/lucid/cluster/plan` (fail-closed `403` on `UntrustedSpotChecked`, runs GGUF inspection on `spawn_blocking`).
- `crates/lucidd/src/main.rs` — Added `lucidd cluster status`, `lucidd cluster inspect <gguf>`, and `lucidd cluster plan` subcommands.
- `apps/lucid-desktop/src-tauri/src/lib.rs`, `apps/lucid-desktop/src/lib/api.ts`, `apps/lucid-desktop/src/App.svelte` — Added **Cluster** button and **Trusted-Circle Cluster** modal with permission-first opt-in toggle and coordinator-signed layer-split preview.
- `crates/lucidd/tests/shard_worker.rs` — Added real 3-node libp2p integration test exercising proportional 80-layer partitioning, activation handoff chaining, composite receipt verification, `manual_pause` rejection, default-deny uninvited-peer rejection, and mid-pipeline failure fail-closed behavior.
- `crates/lucidd/tests/http_api.rs` — Added real-process assertions for `/api/lucid/cluster` and `/api/lucid/cluster/plan` (`401`, `403` untrusted fail-closed, `200` verified plan).

## Patterns Applied
- `memory-bank/decisions.md#workload-neutral-substrate`: `phase-net` carries strictly bounded opaque bytes on `/phase/shard-stage/1.0.0` with zero model/layer/tensor semantics.
- `memory-bank/releases/lucid-v0.2/shardworker-adr.md`: Tier-1 `TrustedCircle` (`trusted-circle-sharded`) vs fail-closed Tier-2 `UntrustedSpotChecked`.
- Permission-first opt-in (`cluster_enabled = false` by default) and token isolation from WebView in `apps/lucid-desktop`.
