# Phase

Phase is the open verifiable compute protocol — a credibly-neutral, vendor-agnostic substrate for advertising, discovering, and verifying compute across machines that do not trust each other, without payments, without KYC, without lock-in. The protocol is workload-neutral: WASM today, GPU inference now (LUCID), anything that benefits from verifiable distributed compute after that. For everyone: smart computing, on the public's hardware, that nobody can turn off.

See [memory-bank/MISSION.md](memory-bank/MISSION.md) for the long-form rationale.

---

## Status

- **Phase Core** — **complete** (Nov 2025 → May 2026). The libp2p + Ed25519 substrate, extracted from the legacy `daemon/` tree into six publishable Apache-2.0 library crates plus the repositioned Plasm reference node.
- **LUCID v0.1** — **functional and security-hardened.** The inference flagship (`crates/lucidd`) speaks the Ollama API on `:11434`, runs real GPU inference and embeddings through llama.cpp, routes locally or across the Phase DHT with multi-peer failover, and returns cryptographically verifiable signed receipts. A 24/7 foundation relay runs on the public internet.
- **Security hardening** — **shipped & live** (PR #9). Signer authorization, DoS caps, receipt verify+bind, patched dependencies, and a `cargo audit` / `cargo deny` CI gate.

**Validated on real hardware:**
- Two-node, DHT-routed inference demo — one machine loads a model, another `curl`s `localhost:11434/api/chat` and gets a signed result routed over libp2p.
- Real embeddings on Apple Metal — `nomic-embed-text` via `/api/embed`, 768-dim vectors with correct semantics (`cos(cat,kitten)=0.74`, `cos(cat,finance)=0.30`).
- An authz-gate demo — default-deny rejects an unauthorized routed job; the operator escape hatch accepts it with `X-Lucid-Receipt-Verified: true`.

**Workspace state:** 266 tests passing · `cargo build --workspace` clean · `clippy --workspace --all-targets -- -D warnings` clean · `cargo audit` 0 vulnerabilities · `cargo deny check` ok. ~20k lines of Rust across eight crates.

---

## What works today (v0.1)

- **Ollama-compatible API** on `:11434` — every existing AI tool (Open WebUI, Continue, Cursor, LangChain, `curl`) just works, no new SDK:
  `/api/chat`, `/api/generate` (NDJSON streaming) · `/api/embed`, `/api/embeddings` (vectors) · `/api/pull` (registers a local model) · `/api/tags`, `/api/show`, `/api/version`.
- **Workers:** `llama.cpp` (real inference + embeddings via `llama-server`, any of its Metal/CUDA/ROCm/Vulkan backends) and an in-tree `echo` worker (no GPU, for dev/CI/demos).
- **Local-or-DHT routing** — per-request: serve locally if the model is loaded, else discover a peer on the Kademlia DHT and relay the job, with **multi-peer failover**. Response headers report where it ran (`X-Lucid-Routed-Via`) and whether the peer's signed receipt verified (`X-Lucid-Receipt-Verified`).
- **Verifiable execution** — every result carries a streaming SHA-256 commitment in an Ed25519 `SignedReceipt`; a peer-served receipt is verified and bound (signature → job-id → worker-pubkey→PeerId → commitment replay) before it is trusted.
- **Operator policy** — a hot-reloadable `policy.toml`: serve-model allowlist, auto-pause on battery / thermal / time-of-day / concurrency, a signer allowlist, and a `manual_pause`. Self-initiated requests bypass the donation-protection gates so the operator can always use their own GPU.
- **WAN bootstrap** — stable libp2p ports, IPv6 listen, persistent identity, explicit `--bootstrap-peer`, and DNS TXT-record bootstrap (`--bootstrap-dns`, PeerID-pinned).
- **Plasm** — the reference WASM node (`plasmd`): signed-manifest WASM execution in a sandboxed wasmtime runtime, content-addressed artifact serving, and the Phase Boot netboot/kexec pipeline.

---

## Roadmap (aspirational — not yet built)

These are committed directions, not shipped features. The protocol is deliberately workload-neutral so each lands without specializing the substrate.

**v0.2 — substrate & scale**
- Real `/api/pull`: network model download with **content-hashed CIDs** (replacing the v0.1 deterministic name→CID placeholder) and a verified cross-peer `name → cid` index.
- Token-by-token **streaming over the relay** (v0.1 peer-relay is batch-shaped).
- libp2p **circuit-relay server + DCUtR hole-punching** + rendezvous, for the "coffee-shop" NAT-traversal scenario.
- **Reputation** — harden peer-receipt verification from the v0.1 "friend's GPU" trust posture to redundant-execution + reputation spot-checking.
- **ShardWorker** — sharded inference across heterogeneous devices (can proxy to exo-style clusters). Gated on the open problem of verifying sharded/partial computation.

**v0.3 — privacy**
- Cryptographic prompt privacy: onion-routing / split-prompt so a serving peer never sees a plaintext prompt. (v0.1 is honest about per-request prompt visibility and ships a `local-only` toggle.)

**Backends (LUCID)**
- **MLX worker** — Apple Silicon native inference (GPU/Metal via turnkey `mlx-lm`). Deferred LUCID M3; needs an Apple Silicon rig.
- **CoreAIWorker** — Apple Neural Engine backend for our own converted open-weights models (the "donate your idle Mac overnight" efficiency profile). After MLX; gated on confirming the public Core AI API exposes KV-cache / stateful decode. (Apple **Foundation Models** is explicitly a non-goal — see [decisions.md](memory-bank/decisions.md).)

**Second flagship & future workloads**
- **LUMEN** — the diffusion / image-synthesis flagship, a *separate* Phase node (not a LUCID mode), to prove the workload-agnostic bet.
- **phase-render**, **phase-science** — further reference nodes for any compute that benefits from verifiable distribution.

**Reach & governance**
- More geographically-distributed foundation relays; a consumer install site.
- A real mDNS implementation in `phase-artifact-server` (currently a stub).
- A Mozilla/Tor-shaped **501(c)(3)** that holds the trademark and accepts grants without capture.

---

## Repository structure

```
phase/
├── Cargo.toml                 # Workspace root, resolver = 2
├── crates/
│   ├── phase-identity/        # Persistent Ed25519 node identity        Apache-2.0
│   ├── phase-net/             # libp2p 0.56 / Kademlia / mDNS / Noise+QUIC + job-relay   Apache-2.0
│   ├── phase-manifest/        # SignedManifest<T>, generic over payload  Apache-2.0
│   ├── phase-receipt/         # SignedReceipt<T> + commitment accumulator Apache-2.0
│   ├── phase-protocol/        # JobSpec {Wasm,Inference,Embedding} + Worker trait + JobStream  Apache-2.0
│   ├── phase-artifact-server/ # Content-addressed HTTP server            Apache-2.0
│   ├── plasm/                 # Reference WASM Phase node (plasmd)        Apache-2.0
│   └── lucidd/                # LUCID inference Phase node (lucidd)       AGPL-3.0-or-later
├── php-sdk/                   # PHP client SDK (legacy + phase-receipt:v1: signing)
├── wasm-examples/             # Source for hello.wasm
├── examples/                  # hello.wasm artifact + PHP demos
├── boot/                      # Phase Boot (USB/netboot initramfs)
├── dist/                      # Pre-built binaries per target triple + demo asciinema
├── docs/                      # Additional documentation
├── memory-bank/               # Project documentation (MISSION, decisions, progress, releases, tasks)
└── CLAUDE.md / AGENTS.md      # AI-assist development workflow
```

### Crate dependency graph

```
phase-identity   ← leaf
  ├── phase-net
  ├── phase-manifest
  └── phase-receipt
          └── phase-protocol
                  ├── phase-artifact-server  (also depends on phase-net)
                  ├── plasm                  (WASM reference node)
                  └── lucidd                 (inference flagship)
```

No upward references. Build order is top to bottom.

---

## Quick start

Build and test everything:

```bash
cargo build --workspace
cargo test --workspace          # 266 tests
```

**Plasm** — run the reference WASM node and execute the hello.wasm sample (reverses `Hello, World` → `dlroW ,olleH`, emitting a signed receipt):

```bash
cargo run -p plasm --bin plasmd -- start
cargo run -p plasm --bin plasmd -- run examples/hello.wasm
```

**LUCID** — run the inference daemon. With the GPU-less echo worker (no model needed):

```bash
cargo run -p lucidd -- --worker echo
curl -s localhost:11434/api/embed -d '{"model":"echo","input":["hello","world"]}'
```

With real inference/embeddings via llama.cpp (point `--model-dir` at a folder of `.gguf` files; needs the `llama-server` binary):

```bash
cargo run -p lucidd -- --worker llama-cpp \
  --model-dir ~/models --llama-server-binary "$(command -v llama-server)"
curl -s localhost:11434/api/chat \
  -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}]}'
```

Any Ollama-compatible client works unchanged — point it at `http://localhost:11434`. See [crates/lucidd/README.md](crates/lucidd/README.md) for the full daemon reference.

---

## Documentation

- [memory-bank/MISSION.md](memory-bank/MISSION.md) — what we are building and why
- [crates/lucidd/README.md](crates/lucidd/README.md) — the LUCID daemon (API, workers, CLI, security)
- [crates/plasm/README.md](crates/plasm/README.md) — the Plasm WASM node
- [memory-bank/releases/phase-core/](memory-bank/releases/phase-core/) — substrate extraction release plan
- [memory-bank/releases/lucid/](memory-bank/releases/lucid/) — inference flagship release plan
- [memory-bank/decisions.md](memory-bank/decisions.md) — architectural decision record
- [memory-bank/progress.md](memory-bank/progress.md) — milestone log
- [memory-bank/projectRules.md](memory-bank/projectRules.md) — coding and contribution conventions
- [CLAUDE.md](CLAUDE.md) — AI-assist development workflow (AGENTS.md-compatible)

---

## License

- `phase-identity`, `phase-net`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`, `plasm` — **Apache-2.0**. The substrate must be adoptable without legal friction by anyone, including parties hostile to one another.
- `lucidd` — **AGPL-3.0-or-later**. The flagship application is copyleft so no party can fork it closed.

See per-crate `Cargo.toml` for canonical license declarations and the top-level [LICENSE](LICENSE) file for the Apache-2.0 text.
