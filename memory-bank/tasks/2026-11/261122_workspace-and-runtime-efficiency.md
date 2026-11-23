# 261122_workspace-and-runtime-efficiency

## Objective

Execute the workspace, build/CI, runtime hot-path, and structural efficiency plan across the Phase monorepo without breaking any public crate APIs, security gates, or existing qualification evidence:

1. Reclaim stale build/validation/binary disk bloat while preserving qualification logs and model assets.
2. Unify `[workspace.dependencies]` and package metadata across all 8 Rust crates, eliminating duplicate `tower`/`tower-http`/`native-tls` stacks, unused crate edges, and overly broad `tokio = ["full"]` features.
3. Optimize `scripts/phase-validate.sh` and `.github/workflows/security.yml` to remove redundant test execution and per-file hashing overhead.
4. Fix concrete runtime hot-path bottlenecks in `LlamaCppWorker`, `Wasm3Runtime`, `PolicyEngine`, `ollama`, `registry`, `canonical`, and the macOS SwiftUI `DaemonController`.
5. Deduplicate backend HTTP/SSE framing into `crates/lucidd/src/backend_io.rs`, split the 7 largest 2.8K–5K line Rust monoliths into `<mod>/mod.rs` + `<mod>/tests.rs`, and prune stale November 2025 blockers from `memory-bank/progress.md`.

## Outcome

- ✅ Rust workspace tests: **492 passed, 0 failed, 2 ignored** (`cargo test --workspace --offline`), including 5 new regression tests covering lock-free warm model hits, cross-chunk SSE seam framing, and shared Wasmtime module caching.
- ✅ macOS Swift app tests & build: **11 passed, 0 failed** (`LocalAPITests` 8/8, `LocalHTTPContractTests` 3/3) and `Lucid-product` built cleanly with `-warnings-as-errors`.
- ✅ Formatting & Linting: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --offline -- -D warnings` passed with **0 warnings** (including 0 Cargo manifest warnings).
- ✅ Security & Supply Chain: `cargo audit --no-fetch --stale` reported **0 vulnerabilities** (4 existing transitive unmaintained warnings retained); `cargo deny --offline check --disable-fetch` passed (`advisories ok, bans ok, licenses ok, sources ok`).
- ✅ Validation script syntax: `bash -n scripts/phase-validate.sh` passed.
- ✅ Dependency graph cleanup: `tower 0.4.13`, `tower-http 0.5.2`, and `native-tls` removed completely from `Cargo.lock`; unused `phase-net`/`phase-manifest` dependencies removed from `phase-artifact-server` (making it an independent leaf crate); `futures-util` moved to `dev-dependencies` in `phase-protocol`.
- ✅ Disk space reclaimed: **~140 GB net** (even after rebuilding full debug test binaries): `target/` 151 GB → 17 GB; `target-linux-amd64` + `target-linux-arm64` 4.2 GB → 0; `boot/` 1.3 GB → 532 KB; `build/` 135 MB → 0; `dist/` 33 MB → 92 KB. All `.log`/`.json`/`.tsv`/`.txt` evidence files in `target/phase-validation/` were preserved.

## Files Modified

### Workspace, Build & CI
- `Cargo.toml` — bumped workspace `rust-version = "1.88"` and populated `[workspace.dependencies]`.
- `crates/phase-identity/Cargo.toml`, `crates/phase-manifest/Cargo.toml`, `crates/phase-receipt/Cargo.toml`, `crates/phase-protocol/Cargo.toml`, `crates/phase-net/Cargo.toml`, `crates/phase-artifact-server/Cargo.toml`, `crates/plasm/Cargo.toml`, `crates/lucidd/Cargo.toml` — migrated to workspace metadata and dependencies, unified `tower 0.5` / `tower-http 0.6` / `thiserror 2` / `reqwest` (`rustls-tls`), narrowed `tokio` features, dropped unused `phase-net`/`phase-manifest` from `phase-artifact-server` and unused `tower`/`tower-http` from `plasm`, fixed repository URL typo in `phase-receipt/Cargo.toml`.
- `Cargo.lock` — refreshed offline against already-cached crates (removed `tower 0.4`, `tower-http 0.5`, `native-tls`).
- `.gitignore`, `dist/README.md`, `README.md` — removed committed v0.1.0 `dist/lucidd-0.1.0-*/lucidd` binaries and ignored `dist/lucidd-*/lucidd`.
- `scripts/phase-validate.sh` — batched `source_fingerprint()` hashing via `xargs -0` instead of per-file subshells; avoided duplicating identical `cargo test` targets between `run_workspace` and `qualification`/`all` while keeping `adversarial` standalone.
- `.github/workflows/security.yml` — split `rustfmt` into a fast cache-free `fmt` job and added `--locked` to `cargo clippy`.

### Runtime Hot Paths & Shared Utilities
- `crates/lucidd/src/backend_io.rs` — new shared crate-internal module consolidating bounded HTTP body reading (`BackendBodyReadError`, `extend_limited_body`, `read_limited_body`) and zero-extra-allocation SSE framing (`find_sse_boundary`, `append_sse_chunk`, `take_next_sse_frame`) across `worker_llama` and `worker_mlx`, eliminating `worker_mlx`'s per-chunk `BytesMut` copy-allocate overhead.
- `crates/lucidd/src/lib.rs` — wired `pub(crate) mod backend_io`.
- `crates/lucidd/src/worker_llama/mod.rs` — added lock-free `DashMap` fast path in `LlamaCppWorker::ensure_loaded` before acquiring `load_gate` (double-checked locking) so warm inference never blocks behind unrelated cold model loads; reused `backend_io`; added warm-hit regression test in `worker_llama/tests.rs`.
- `crates/lucidd/src/worker_mlx/mod.rs` — reused `backend_io` SSE framing.
- `crates/lucidd/src/policy.rs` — introduced `CompiledPolicy` (`Arc<RwLock<Arc<CompiledPolicy>>>`) with precompiled `Vec<glob::Pattern>` for zero-clone hot-path reads in `should_serve` / `should_serve_self`, and `SystemProber` to reuse `sysinfo::Components` across 30s watcher ticks instead of double-refreshing.
- `crates/lucidd/src/ollama/mod.rs` — fixed `O(n²)` `out.chars().count()` in `sanitize_for_log`; switched `handle_tags` model-size lookup from blocking `std::fs::metadata` to async `tokio::fs::metadata`.
- `crates/lucidd/src/registry/mod.rs` — replaced 32x per-byte `format!("{b:02x}")` allocations in `ModelCid::to_hex` with `hex::encode(self.0)`.
- `crates/phase-manifest/src/canonical.rs` and `crates/phase-receipt/src/canonical.rs` — drained `map.into_iter()` and sorted entries in place instead of cloning every key and value.
- `crates/plasm/src/wasm/runtime.rs` — added process-wide `SharedWasmEngine` reusing a single `wasmtime::Engine`, pre-populated WASI `Linker`, and bounded 64-entry SHA-256 compiled-module cache; wired `StoreLimitsBuilder` to enforce `max_memory_bytes`; added cache verification unit test.
- `crates/phase-artifact-server/src/server.rs` — adjusted rustdoc link after removing unused `phase-manifest` dependency.
- `crates/phase-net/src/discovery/mod.rs` — scoped `#[allow(unreachable_code)]` to a private `combined_behaviour` module wrapping `#[derive(NetworkBehaviour)]` for Rust 1.100 compatibility under `-D warnings`.
- `apps/lucid-macos/Sources/LucidApp/DaemonController.swift` and `apps/lucid-macos/Sources/LucidApp/LucidApp.swift` — ran `api.status()` and `api.catalog()` concurrently via `async let`; replaced the 50ms `Thread.sleep` shutdown busy-wait with event-driven `terminationHandler` continuation + 3s escalation timer; added active/inactive polling backoff (15s active, 60s backgrounded) hooked into `NSApplicationDelegate`.

### Monolith Decomposition & Memory Bank Cleanup
- Split 7 oversized modules into `<mod>/mod.rs` + `<mod>/tests.rs` preserving 100% of public APIs and re-exports:
  - `crates/lucidd/src/registry/{mod,tests}.rs` (3,067 / 1,922 lines)
  - `crates/phase-net/src/discovery/{mod,tests}.rs` (3,620 / 1,340 lines)
  - `crates/lucidd/src/router/{mod,tests}.rs` (2,371 / 1,914 lines)
  - `crates/lucidd/src/ollama/{mod,tests}.rs` (2,605 / 640 lines)
  - `crates/lucidd/src/content/{mod,tests}.rs` (1,805 / 1,154 lines)
  - `crates/lucidd/src/worker_mlx/{mod,tests}.rs` (2,017 / 889 lines)
  - `crates/lucidd/src/worker_llama/{mod,tests}.rs` (2,007 / 730 lines)
- `memory-bank/progress.md` — pruned the contradictory historical November 2025 "Active Work / Current Blockers" block (referencing `libp2p 0.53` and `wasm3`).
- `memory-bank/tasks/2026-11/261122_workspace-and-runtime-efficiency.md`, `memory-bank/tasks/2026-11/README.md`, and `memory-bank/toc.md` — documented this task and indexed November 2026 tasks.

## Patterns Applied

- **Reuse over creation (`AGENTS.md#1`)**: Extracted duplicated HTTP/SSE framing between `worker_llama` and `worker_mlx` into `crates/lucidd/src/backend_io.rs`; consolidated crate dependency declarations into `[workspace.dependencies]`.
- **Async/Await hygiene (`memory-bank/projectRules.md#Async/Await Patterns`)**: Removed blocking `std::fs::metadata` from the async `/api/tags` handler and replaced blocking `Thread.sleep` shutdown polling in Swift with structured async continuations.
- **Zero-clone hot-path reads (`memory-bank/projectRules.md#Performance`)**: Switched `PolicyEngine` to `Arc<CompiledPolicy>` with precompiled globs and double-checked `DashMap` fast path in `LlamaCppWorker::ensure_loaded`.
- **Library + Binary modularity (`memory-bank/systemPatterns.md#Library + Binary Pattern`)**: Separated unit test suites into `<mod>/tests.rs` across the 7 largest modules without changing external crate boundaries.

## Integration Points

- `worker_llama/mod.rs` and `worker_mlx/mod.rs` both call `crate::backend_io::{append_sse_chunk, take_next_sse_frame}` (and `worker_llama` also uses `read_limited_body`).
- `WasmtimeWorker::execute` (`crates/plasm/src/worker.rs:235`) transparently inherits the shared `SharedWasmEngine` and compiled module cache via `Wasm3Runtime`.
- `LucidApp.swift` (`ApplicationDelegate`) notifies `DaemonController.setAppActive(_:)` on `applicationDidBecomeActive` / `applicationDidResignActive`.
- `scripts/phase-validate.sh` `qualification`/`all` continues to enforce workspace fmt/tests/clippy, release bundle verification, security audits, and Linux evidence gates with no duplicate workspace test runs.

## Architectural Decisions

- **Keep `canonical.rs` decoupled between `phase-manifest` and `phase-receipt`**: Preserved the existing crate DAG (`phase-manifest` and `phase-receipt` remain independent siblings) while removing the redundant key/value clones in both implementations.
- **Preserve validation logs and model weights during disk cleanup**: Deleted disposable build artifacts and duplicate staged artifact blobs (`target/debug`, cross-targets, `boot/build`, `build/`, duplicate `artifacts/{blobs,verified-models}`) while retaining all `.log`/`.json`/`.tsv`/`.txt` evidence files and source GGUF weights.
- **Decompose monoliths by extracting `tests.rs` first**: Separated ~8,589 lines of inline tests out of production modules without altering public symbols or colliding with active in-flight work on `feat/lucid-consumer-experience`.

## Artifacts

- Branch: `feat/lucid-consumer-experience` (working tree edits, uncommitted pending user review/commit).
- Test summary: Rust `492 passed / 0 failed / 2 ignored`; Swift `11 passed / 0 failed`; `cargo audit` 0 vulnerabilities; `cargo deny` all checks ok; `cargo clippy --workspace --all-targets -- -D warnings` 0 warnings.
