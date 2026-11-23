# 261123_apple-simple-lucid-desktop

## Objective
Eliminate manual CLI/env/venv setup friction by building:
1. **Phase 1 — Self-configuring `lucidd` core & CLI (`crates/lucidd`)**: read-only hardware/Ollama/LM-Studio/GGUF discovery, explicit permission-first providing, SHA-256-pinned `ggml-org/llama.cpp` `b12651` runtime setup, zero-flag default startup (`NodeMode::Auto`), expanded authenticated `/api/lucid/*` loopback control plane, and ergonomic CLI subcommands (`probe`, `models`, `peers`, `invite`, `join`).
2. **Phase 2 — Sandboxed cross-platform Tauri v2 + Svelte 5 desktop app (`apps/lucid-desktop/`)**: single-window Apple-simple UI shipping native macOS Apple Silicon (`.dmg` / `.app`) and Ubuntu x86_64 & ARM64 (`.deb`) installers via GitHub Actions.

## Outcome
- ✅ Rust workspace: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` (0 failures), `cargo audit` (0 vulnerabilities), `cargo deny` (`advisories/bans/licenses/sources ok`).
- ✅ Live hardware probe on Mac M5 Max (128GB RAM): detected `Apple M5 Max · 128 GB unified memory (Metal)` and `/opt/homebrew/Cellar/llama.cpp/1.0.0/bin/llama-server` strictly read-only (`detected_unpinned`).
- ✅ Tauri v2 + Svelte 5 desktop app: `svelte-check` (0 errors, 0 warnings), `vite build` (25.65 kB gzipped JS), `src-tauri` unit tests + strict Clippy (`-D warnings`) + `cargo audit` (0 vulnerabilities).
- ✅ Native macOS bundles built and verified locally: `LUCID.app` (30.98 MiB, `codesign --verify --deep --strict`) and `LUCID_0.2.0_aarch64.dmg` (14.06 MiB).
- ✅ Workflows & scripts: `actionlint` and `shellcheck` clean.

## Files Added / Modified
- `crates/lucidd/src/node_config.rs` — Persistent `0600` `node.toml` with default-off providing and sensitive-directory blocking (`validate_model_dir`).
- `crates/lucidd/src/runtime_manager.rs` — Read-only hardware/GPU/Ollama/LM-Studio/GGUF probe + fail-closed SHA-256-pinned `llama.cpp` `b12651` download/extraction manager (archive traversal + symlink escape validation).
- `crates/lucidd/src/ollama/mod.rs` — Authenticated loopback control endpoints (`/api/lucid/{peers,probe,probe/refresh,models,provide,folders,settings,runtime,runtime/setup}`), requiring `confirm_permission: true` before providing or installing a runtime.
- `crates/lucidd/src/policy.rs` — Added atomic `0600` `PolicyEngine::update_config`.
- `crates/lucidd/src/main.rs` — Added `NodeMode::Auto` safe zero-flag startup + CLI subcommands (`probe`, `models`, `peers`, `invite`, `join`) with loopback-only control guard.
- `crates/lucidd/src/worker_llama/mod.rs` — Added `Stdio::null()` alongside `env_clear()`.
- `apps/lucid-desktop/` — Cross-platform Tauri v2 (`#![deny(unsafe_code)]`, root refusal, token isolation from WebView, strict CSP, minimal capabilities, system tray, `lucid://` deep links) + Svelte 5 UI with permission-first onboarding.
- `scripts/install-lucid.sh` — Rewrites `lucidd-relay.service` `ExecStart` to match `--prefix` + adds `--setup-relay <IP>`.
- `.github/workflows/security.yml`, `.github/workflows/release.yml`, `.gitignore` — Added `desktop-tauri` CI job and cross-platform `.dmg` + `.deb` release packaging.

## Security & Sandbox Invariants Preserved
- Default-deny remote job authorization (`allow_unauthenticated_jobs = false`) unchanged.
- Permission-first: hardware/model discovery is strictly read-only; providing or runtime installation fails closed (`428 Precondition Required`) without explicit `confirm_permission: true`.
- Token isolation: WebView JS never receives the `lucidd` local control token; Rust backend talks exclusively to `127.0.0.1` (`no_proxy()`, redirects disabled).
- Strict CSP (`default-src 'self'`) and minimal Tauri v2 capabilities (no JS shell or arbitrary fs access).
- Whole-model serving language kept distinct from research-gated `ShardWorker` sharded slices.
