# Workspace architecture, crates and Plasm

The repository is a Cargo workspace (`edition = "2021"`, `rust-version = "1.88"`) cleanly split between the **Apache-2.0 Phase substrate** and the **AGPL-3.0-or-later LUCID flagship**, plus a cross-platform Tauri v2 Desktop app.

## Workspace crates

| Crate | Version | License | `#![deny(unsafe_code)]` | Role |
|---|---|---|---|---|
| `phase-identity` | `0.1.0` | Apache-2.0 | Yes | Persistent Ed25519 `NodeIdentity` with atomic `0600` storage. |
| `phase-manifest` | `0.1.0` | Apache-2.0 | Yes | Generic `SignedManifest<T>`, canonical JSON signing, remote replay-window policy. |
| `phase-receipt` | `0.1.0` | Apache-2.0 | Yes | Generic `SignedReceipt<T>` with canonical JSON signing. |
| `phase-protocol` | `0.1.0` | Apache-2.0 | Yes | Workload-agnostic `Worker`/`DynWorker` traits, `JobSpec` (`Wasm`, `Inference`, `Embedding`), `JobEvent`, `JobHandle`, and `CommitmentAccumulator`. |
| `phase-net` | `0.1.0` | Apache-2.0 | Yes | Workload-neutral `libp2p 0.57` transport, Kademlia, mDNS, Relay v2, DCUtR, Identify, AutoNAT v2, Rendezvous, and all bounded `/phase/*` stream codecs. |
| `phase-artifact-server` | `0.1.0` | Apache-2.0 | Yes | Content-addressed HTTP/Range server (`blobs/<aa>/<sha256>.bin`) and atomic `ArtifactStore`. |
| `plasm` | `0.1.0` | Apache-2.0 | Yes (lib) | Reference WASM node (`plasmd`, Wasmtime 36 LTS) + Phase Boot utilities (`phase-discover`, `phase-verify`, `phase-fetch`). |
| `lucidd` | `0.2.0` | AGPL-3.0-or-later | Yes (lib) | LUCID GPU inference daemon (`llama.cpp` + `MLX` + `echo` workers, Ollama & control HTTP APIs, DHT registry, P2P content plane, router, Tier-1 sharding, policy engine). |

Zero `unsafe` blocks exist anywhere inside `crates/` or `apps/lucid-desktop/src-tauri/src/` (third-party dependencies are checked in CI with `cargo audit` and `cargo deny`).

### Dependency graph

```text
phase-artifact-server  (standalone leaf)
phase-identity         (leaf)
  ├── phase-net
  ├── phase-manifest
  └── phase-receipt
          └── phase-protocol
                  ├── plasm   (also uses phase-artifact-server)
                  └── lucidd  (also uses phase-artifact-server)
```

No upward references exist; `phase-net` contains zero inference or LLM logic.

## Plasm: the WASM reference node (`crates/plasm`)

Plasm proves the Phase substrate is genuinely workload-neutral by running sandboxed WebAssembly modules (`WasmtimeWorker`, Wasmtime 36 LTS) with the same Ed25519 identity, manifests, and signed receipts:

```bash
cargo run -p plasm --bin plasmd -- start
cargo run -p plasm --bin plasmd -- run examples/hello.wasm
```

`crates/plasm` also includes Phase Boot CLI helpers (`phase-discover`, `phase-verify`, `phase-fetch`) for verifying and fetching channel/architecture images from `phase-artifact-server`.

## LUCID Desktop (`apps/lucid-desktop`)

Cross-platform **Tauri v2 + Svelte 5** app (`net.phasebased.lucid`, `#![deny(unsafe_code)]`) bundling `lucidd` via Tauri `externalBin` (`apps/lucid-desktop/stage-sidecar.sh`):

- **Strict WebView boundary:** Strict CSP (`default-src 'self'; script-src 'self'; connect-src ipc: http://ipc.localhost https://ipc.localhost; object-src 'none'; frame-ancestors 'none'`), minimal capabilities (`core:default`, `dialog:allow-open`, `opener:allow-open-url` — no shell or filesystem plugins).
- **Token isolation:** Per-session Bearer token (`control-<uuid>.token`, mode `0600`) stays exclusively inside Rust; Svelte calls 16 typed Tauri commands.
- **Sidecar lifecycle:** Refuses `root` (`uid == 0`), launches `lucidd` with `env_clear()`, hashes the sidecar binary at launch, integrates with the system tray (`Open LUCID`, `Quit LUCID`), and on exit sends `SIGTERM` (3-second grace window so `lucidd` reaps `llama-server` / `mlx_lm.server` / `ggml-rpc-server` cleanly) before falling back to `SIGKILL` and deleting the session token.
- **Deep links:** Handles `lucid://join/...` URLs and `.lucidinvite` files directly into the Join Invite modal.

## Validation and release signing scripts (`scripts/`)

| Script | Purpose |
|---|---|
| `scripts/phase-validate.sh` | Fingerprinted local/CI validation harness (`phase-net`, `lucid`, `artifact`, `adversarial`, `linux`, `release`, `security`, `workspace`, `qualification`). |
| `scripts/install-lucid.sh` / `install-lucid.ps1` | Fail-closed SHA-256-verified end-user installers for macOS, Linux, and Windows. |
| `scripts/setup-cloud-relay.sh` | One-command hardened Linux Cloud Relay installer (`User=lucid`, port `443` or `4001`). |
| `scripts/sign-macos-release.sh` | Local Mac Keychain Developer ID signing, Apple notarization, stapling, `spctl` verification, and optional GitHub Release republish + `SHA256SUMS.txt` refresh (zero Apple secrets in CI). |
| `scripts/sign-windows-binary.ps1` | Shared CI + local Windows VM Azure Artifact Signing script using SHA-256-pinned NuGet SDK tools, RFC 3161 timestamping, and `signtool verify /pa`. |
| `scripts/build_site.py` | Builds this GitHub Pages site from `site/` + `pandoc`, failing closed on broken links, missing anchors, or non-documentation IPv4s. |
