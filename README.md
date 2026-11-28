# Phase & LUCID

**Private, peer-to-peer AI on hardware you trust — built on an open verifiable compute protocol.**

- **Phase** is a workload-neutral, Apache-2.0 peer-to-peer substrate (`libp2p 0.57`, Ed25519 identities, signed job manifests, streaming execution receipts, and content-addressed blob transfer) with no accounts, no billing, and no vendor lock-in.
- **LUCID** (`lucidd` + Desktop app, AGPL-3.0-or-later) is the GPU inference flagship built on Phase. It runs open-weight models locally or across friends you explicitly invite, speaks the standard Ollama HTTP API on `127.0.0.1`, and verifies signed Ed25519 receipts on every remote reply.

**Website & Full Documentation:** [msitarzewski.github.io/phase](https://msitarzewski.github.io/phase/) · **[Download & Releases](https://github.com/msitarzewski/phase/releases)** · **[60-Second Quickstart](docs/friends-and-family-quickstart.md)** · **[HTTP API](site/help/http-api.md)** · **[Network Protocols](site/help/protocols.md)**

---

## Install in 60 seconds

Every GitHub Release ships a unified `SHA256SUMS.txt` verified fail-closed before unpacking.

- **macOS Apple Silicon (Desktop `.dmg` + CLI):** Download `LUCID_0.2.0_aarch64.dmg` from [Releases](https://github.com/msitarzewski/phase/releases), or run:
  ```bash
  curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh | bash -s -- --app
  ```
- **Linux (`x86_64` & `aarch64`, Desktop `.deb` + CLI):** Install the `.deb` from [Releases](https://github.com/msitarzewski/phase/releases), or install the CLI (baseline `x86-64`, zero AVX2/BMI2 requirements):
  ```bash
  curl -fsSL https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.sh | bash
  ```
- **Windows (`x86_64` CLI):**
  ```powershell
  irm https://raw.githubusercontent.com/msitarzewski/phase/main/scripts/install-lucid.ps1 | iex
  ```

See [site/help/install.md](site/help/install.md) for manual checksum verification, offline installs, and building from source.

---

## What LUCID does (simple on the surface, deep underneath)

| Capability | Plain-English Summary | Deep Dive |
|---|---|---|
| **Built-in Chat & Ollama API** | Token-by-token streaming Desktop Chat (Stop, Copy, memory-only history, **Local Only** toggle) plus Ollama-compatible `/api/chat`, `/api/generate`, `/api/embed`, `/api/tags`, `/api/show`, `/api/ps`, and `/api/pull` on `127.0.0.1`. | [First chat](site/help/first-chat.md) · [HTTP API](site/help/http-api.md) |
| **Default-Deny Private Circles** | Serving is off by default (`providing_enabled = false`). Share access via single-use `lucid://join/...` links or `.lucidinvite` files; uninvited peers are refused automatically. | [Joining a circle](site/help/first-chat.md#joining-a-trusted-circle) |
| **Safe GPU Sharing** | Hot-reloadable `policy.toml` (+ `SIGHUP`) auto-pauses remote jobs on battery, over thermal ceiling, outside your hours, or above concurrency caps—while your own prompts always run. | [Operator policy](site/help/operator-policy.md) |
| **Zero-Copy Discovery & P2P Pull** | Read-only discovery of Ollama and LM Studio GGUFs, SHA-256-pinned `llama.cpp b12651` runtime, and resumable `/phase/blob/1.0.0` P2P model downloads with whole-file SHA-256 verification. | [Models & sharing](site/help/models-and-sharing.md) |
| **Hardened GPU Backends** | `llama.cpp` (Metal, CUDA, ROCm, Vulkan, CPU) with per-spawn 256-bit `LLAMA_API_KEY`, cross-tenant KV-cache stripping (`--no-cache-prompt --cache-ram 0 --cache-reuse 0`), hang kill, deferred LRU eviction, and honest `/api/ps` residency; plus Apple Silicon `MLX` and dev `echo` workers. | [Workers & backends](site/help/workers-and-backends.md) |
| **Tier-1 GPU Pooling** | Pool memory across your own trusted machines over Noise-encrypted `/phase/shard-rpc/1.0.0` with auto-managed SHA-256-pinned loopback `ggml-rpc-server`, RPC-first `--llama-tensor-split`, and refusal of silent local fallback. | [Trusted-Circle sharding](site/help/trusted-circle-sharding.md) |
| **Self-Hostable Relay & Tor/Tails** | One-command unprivileged Linux Cloud Relay (`scripts/setup-cloud-relay.sh --port 443\|4001`), PeerID-pinned DNS TXT bootstrap, `--outbound-only` mode, and TailsOS/Tor guidance. | [Cloud Relay & Tor](site/help/cloud-relay-and-privacy.md) |
| **Verifiable Substrate & Protocols** | Seven `/phase/*` libp2p protocols, ten Kademlia DHT namespaces, Ed25519 `SignedManifest`/`SignedReceipt` + streaming `CommitmentAccumulator`, and `plasmd` WASM reference node. | [Protocols](site/help/protocols.md) · [Cryptography](site/help/cryptography-and-receipts.md) |

---

## Current status (reviewed 2026-11-27)

- **Phase Core** — **shipped.** Six publishable Apache-2.0 crates (`phase-identity`, `phase-net`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`) plus the `plasm` WASM reference node, all with `#![deny(unsafe_code)]`.
- **LUCID v0.1 / v0.1.1** — **shipped & security-hardened.** Default-deny authorization, Ed25519 `PeerId` binding, four-way receipt verification, `libp2p 0.57` / `wasmtime 36 LTS` / `hickory 0.26`, and `cargo audit` / `cargo deny` CI gates.
- **LUCID v0.2 beta (`Friends & Family`)** — **active physical 5-node qualification (`v0.2.0-rc12` dual-signed & live fleet-wide).**
  - **Verified on physical hardware:** `126/126` (`100%`) remote peer-verified Ed25519 receipts across live WAN/relay/LAN runs (plus `12` local commitments, `25` coordinator-sharded runs, and `2` chained stage receipts); **NVIDIA RTX 5060 Ti 16 GB CUDA 12.8** (`50.9 tok/s` `c=2`, `30.0 tok/s` @ `160 ms` TTFT `c=1`, live `/api/ps` residency, and `4/4` remote receipts to a 2012 Ivy Bridge laptop); **3-GPU heterogeneous cross-OS/cross-vendor sliced `gemma-4-12B`** across Mac M5 Max (Metal 4) + Mac M1 Max (Metal 4) + Ubuntu Desktop (RTX 5060 Ti CUDA 12.8, `1,034 MiB` live VRAM) over zero-SSH, zero-CLI auto-managed `/phase/shard-rpc/1.0.0` (`13.0 tok/s` `c=1`, **`37.5 tok/s`** `c=4`); `10.18 GB` P2P pull with interrupt & exact-offset resume; Apple Silicon MLX at `71.3 tok/s` (`124 ms` TTFT) over Cellular + VPN + Cloud Relay and `98.0 tok/s` combined 2-Mac burst; 2-Mac sliced `gemma-4-12B` at `22.0 tok/s` (`c=1`) and `57.5 tok/s` (`c=4`); empirical KV-cache isolation verification; baseline `x86-64` zero-`SIGILL` run on 2012 Ivy Bridge; **Apple Developer ID notarized & stapled macOS builds**; and **Azure Artifact Signed Windows `lucidd.exe` via zero-secret GitHub OIDC**.
  - **Honest open gates before v0.2.0 final:** fail-closed native `--tor-socks5` transport, public rendezvous registration quotas, Linux ARM64 VM qualification, and multi-site relay redundancy.
  - **Honest security boundaries:** Receipts prove attributable output integrity (`PeerId` + exact streamed bytes), not hidden weight quantization; serving peers see plaintext prompts (use `X-Lucid-Local-Only: true` for local-only prompts); Tier-1 GPU pooling carries SSH-equivalent peer trust and Tier-2 `UntrustedSpotChecked` sharding remains fail-closed.

See the [authoritative v0.2 tracker](memory-bank/releases/lucid-v0.2/README.md) and [physical fleet progress report](memory-bank/releases/lucid-v0.2/fleet-progress-report.md).

---

## Repository structure

```text
phase/
├── Cargo.toml                     # Workspace root (edition 2021, rust-version 1.88)
├── apps/
│   └── lucid-desktop/             # Cross-platform Tauri v2 + Svelte 5 app (macOS .dmg, Linux .deb)
├── crates/
│   ├── phase-identity/            # Persistent Ed25519 identity (0600)          Apache-2.0
│   ├── phase-net/                 # libp2p 0.57, Kademlia, Relay v2, /phase/*   Apache-2.0
│   ├── phase-manifest/            # Generic SignedManifest<T>                   Apache-2.0
│   ├── phase-receipt/             # Generic SignedReceipt<T>                    Apache-2.0
│   ├── phase-protocol/            # Worker trait, JobSpec, CommitmentAccumulator Apache-2.0
│   ├── phase-artifact-server/     # Content-addressed HTTP Range server         Apache-2.0
│   ├── plasm/                     # Reference WASM node (plasmd) + Phase Boot   Apache-2.0
│   └── lucidd/                    # LUCID GPU inference daemon (lucidd)         AGPL-3.0-or-later
├── site/                          # GitHub Pages landing page, releases & searchable help/API docs
├── scripts/
│   ├── build_site.py              # Builds & verifies _site/ via pandoc (links, anchors, opsec)
│   ├── install-lucid.sh           # Verified macOS & Linux installer
│   ├── install-lucid.ps1          # Verified Windows x86_64 installer
│   ├── setup-cloud-relay.sh       # One-command unprivileged Linux Cloud Relay setup
│   ├── sign-macos-release.sh      # Local Mac Keychain Developer ID + notarization + staple
│   ├── sign-windows-binary.ps1    # Shared CI + local Azure Artifact Signing script
│   └── phase-validate.sh          # Fingerprinted build/test/security/release validation harness
├── docs/                          # Quickstart, Tor/Tails, and platform guides
└── memory-bank/                   # Architecture decisions, mission, release trackers & fleet logs
```

---

## Build and run from source

```bash
# Build and test all 8 workspace crates
cargo build --workspace --locked
cargo test --workspace --locked

# Run LUCID with the GPU-less echo worker (no model required)
cargo run -p lucidd -- --mode worker --worker echo
curl -s http://127.0.0.1:11434/api/embed -d '{"model":"echo","input":["hello","world"]}'

# Run LUCID with real GGUF inference via llama.cpp
cargo run -p lucidd -- --mode worker --worker llama-cpp \
  --model-dir ~/Models --llama-server-binary /absolute/path/to/llama-server
curl -s http://127.0.0.1:11434/api/chat \
  -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'

# Run the Plasm WASM reference node
cargo run -p plasm --bin plasmd -- start
cargo run -p plasm --bin plasmd -- run examples/hello.wasm

# Build and preview the GitHub Pages documentation site locally (requires pandoc)
python3 scripts/build_site.py
python3 -m http.server -d _site 8090
```

---

## Documentation index

- **GitHub Pages Site:** [msitarzewski.github.io/phase](https://msitarzewski.github.io/phase/) (`site/pages/*.html`, `site/help/*.md`)
- **Guides:** [Install](site/help/install.md) · [First Chat & Circles](site/help/first-chat.md) · [Models & Sharing](site/help/models-and-sharing.md) · [Operator Policy](site/help/operator-policy.md) · [Workers & Backends](site/help/workers-and-backends.md) · [Tier-1 GPU Pooling](site/help/trusted-circle-sharding.md) · [Cloud Relay & Tor/Tails](site/help/cloud-relay-and-privacy.md)
- **Technical Reference:** [HTTP API](site/help/http-api.md) · [CLI & Env Vars](site/help/cli-and-env.md) · [Phase Protocols & DHT](site/help/protocols.md) · [Cryptography & Receipts](site/help/cryptography-and-receipts.md) · [Architecture & Crates](site/help/architecture-and-crates.md)
- **Project Records:** [MISSION.md](memory-bank/MISSION.md) · [LUCID v0.2 Tracker](memory-bank/releases/lucid-v0.2/README.md) · [crates/lucidd/README.md](crates/lucidd/README.md) · [crates/plasm/README.md](crates/plasm/README.md) · [decisions.md](memory-bank/decisions.md)

---

## License

- **Phase substrate & Plasm** (`phase-identity`, `phase-net`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`, `plasm`): **Apache-2.0**
- **LUCID daemon & Desktop app** (`lucidd`, `apps/lucid-desktop`): **AGPL-3.0-or-later**

See per-crate `Cargo.toml` manifests and the top-level [LICENSE](LICENSE) file.
