# LUCID Desktop (Tauri v2)

Cross-platform desktop application for LUCID targeting **macOS Apple Silicon** (`.dmg` / `.app`) and **Ubuntu Linux x86_64 & ARM64** (`.deb` / `.AppImage`).

## Security & Sandbox Architecture

- **Zero root**: refuses to run as `uid == 0`.
- **Token isolation**: the WebView never receives the `lucidd` local control bearer token; all authenticated `/api/lucid/*` calls happen strictly inside the Rust backend over `127.0.0.1` (`no_proxy()`, redirects disabled).
- **Permission-first onboarding**: on first run, `lucidd` executes a strictly read-only hardware and local-model probe (Ollama / LM Studio / local `.gguf` folders) and asks explicit permission before sharing any model or installing the SHA-256-pinned `llama-server` runtime.
- **Minimal Tauri v2 capabilities**: `capabilities/default.json` allows only window controls, OS folder selection, and local chat-app URL opening (validated in Rust). No JS shell execution or arbitrary filesystem access.
- **Strict CSP**: blocks external scripts, remote fetches, and framing.

## Local Development & Packaging

```bash
# 1. Build lucidd (release) and stage it as the Tauri sidecar
cargo build -p lucidd --release --locked
bash apps/lucid-desktop/stage-sidecar.sh target/release/lucidd

# 2. Install frontend dependencies & check types
cd apps/lucid-desktop
npm install
npm run check

# 3. Run in development or build native installers (.dmg on macOS, .deb/AppImage on Ubuntu)
npm run tauri dev
npm run tauri build
```
