# 261125_shardworker-foundation-and-bench

## Objective
Close remaining real-world usability gaps in `v0.2.0-rc3` and lay the tested foundation for multi-machine sharding (`ShardWorker`):
1. Support direct verification/serving of Ollama blobs (`~/.ollama/models/blobs/sha256-*`), LM Studio nested directories, and multi-folder GGUFs via `ModelRegistry::import_verified_gguf_file`, `approved_model_sources`, and hot-import on `POST /api/lucid/provide`.
2. Add authenticated `POST /api/lucid/pull` and 1-click **Download Locally (P2P)** in `apps/lucid-desktop`.
3. Add real-process HTTP boundary tests for all `/api/lucid/*` permission-first & sandbox endpoints (`crates/lucidd/tests/http_api.rs`) and resolve the intermittent `AddrInUse` fixture teardown flake (`crates/lucidd/tests/fixtures/fake_llama_server.rs`).
4. Implement the whole-model independent-job baseline benchmark harness (`crates/lucidd/src/bench.rs`, `lucidd bench`).
5. Implement the Tier-1 Trusted-Circle `ShardWorker` pipeline planner, coordinator-signed `ShardPartitionPlan`, and chained `ShardStageReceipt` verifier (`crates/lucidd/src/shard_plan.rs`) alongside the complete 7-question `ShardWorker` ADR draft (`memory-bank/releases/lucid-v0.2/shardworker-adr.md`).

## Outcome
- ✅ Rust workspace: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, full `cargo test --workspace` (0 failures), plus **5/5 consecutive repeats** of `llama_worker` passing with zero `AddrInUse` flakes.
- ✅ Real-process HTTP control boundary test (`permission_first_control_api_enforces_auth_consent_and_folder_sandbox`): verified `401` unauthenticated rejection, `428` missing `confirm_permission` rejection on `/api/lucid/provide` and `/api/lucid/runtime/setup`, `400` folder sandbox blocking of `/etc`, GGUF magic discovery, hot-import CID verification, and thermal bounds enforcement.
- ✅ Tauri v2 + Svelte 5: `svelte-check` (0 errors/warnings), `vite build`, `src-tauri` unit tests & strict Clippy (`-D warnings`), and rebuilt/re-signed local `LUCID.app` + `.dmg`.
- ✅ Tracker honesty preserved: `ShardWorker` remains `🟨 Blocked` pending physical `lucidd bench` whole-model baseline and human ADR approval; `ShardTrustTier::UntrustedSpotChecked` fails closed.

## Files Added / Modified
- `crates/lucidd/src/node_config.rs` — Added `ApprovedModelSource` persistence (`approved_model_sources`).
- `crates/lucidd/src/registry/mod.rs` — Added `ModelRegistry::import_verified_gguf_file` (non-symlink root containment + GGUF magic header check for Ollama blobs & nested LM Studio stores).
- `crates/lucidd/src/ollama/mod.rs` — Hot-import of approved models in `POST /api/lucid/provide` + authenticated `POST /api/lucid/pull`.
- `crates/lucidd/src/main.rs` — Auto-import all `model_dirs` & `approved_model_sources` on `NodeMode::Auto` startup + added `lucidd bench` CLI subcommand.
- `crates/lucidd/src/bench.rs` — Whole-model independent-job benchmark harness (p50/p95 TTFT & duration, route distribution, receipt verification rate, sanitized JSON output).
- `crates/lucidd/src/shard_plan.rs` — Memory-proportional heterogeneous layer planner, `SignedShardPartitionPlan`, `SignedShardStageReceipt`, and `verify_composite_shard_chain`.
- `crates/lucidd/tests/http_api.rs` — Added `permission_first_control_api_enforces_auth_consent_and_folder_sandbox`.
- `crates/lucidd/tests/fixtures/fake_llama_server.rs` — Bounded `AddrInUse` bind retry (10×25ms) eliminating teardown-race flakes.
- `apps/lucid-desktop/` — Wired `local_path` approval and 1-click `Download Locally (P2P)` (`pull_model` IPC).
- `memory-bank/releases/lucid-v0.2/shardworker-adr.md` — Draft ADR answering all 7 hard-stop questions from `shardworker.md:28-37`.
