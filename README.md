# Phase

Phase is the open verifiable compute protocol — a credibly-neutral, vendor-agnostic substrate for advertising and discovering compute, and checking signed execution evidence across independent machines, without payments, without KYC, without lock-in. The protocol is workload-neutral: WASM through Plasm, GPU inference through LUCID, and further workloads through the same substrate. The goal is public compute infrastructure that no single provider controls. Current receipts establish attributable output integrity; independent computation-correctness validation remains in development.

See [memory-bank/MISSION.md](memory-bank/MISSION.md) for the long-form rationale.

---

## Status

**Reviewed: 2026-11-07.** The release tracker distinguishes shipped software, implemented checkpoints, and pending acceptance. It is authoritative over historical roadmap notes below.

- **Phase Core** — **complete** (Nov 2025 → May 2026). The libp2p + Ed25519 substrate, extracted from the legacy `daemon/` tree into six publishable Apache-2.0 library crates plus the repositioned Plasm reference node.
- **LUCID v0.1 / v0.1.1** — **shipped and security-hardened.** The inference flagship (`crates/lucidd`) speaks the Ollama API on `:11434`, runs real GPU inference and embeddings through llama.cpp, routes locally or across the Phase DHT with multi-peer failover, and returns cryptographically verifiable signed receipts. A public foundation relay was deployed in May; current fleet availability requires fresh operational evidence.
- **LUCID v0.2** — **implemented checkpoint, unreleased.** Verified content pull/alias/provider records, live peer streams, relay/AutoNAT/DCUtR machinery, local reputation and bounded redundancy, and an experimental MLX adapter are implemented. Physical cross-network/NAT acceptance, real large-artifact resume, real MLX acceptance, independent correctness validation, and the remaining qualification matrix are open. [Authoritative tracker](memory-bank/releases/lucid-v0.2/README.md).
- **Security hardening** — **shipped in May 2026** (PR #9). Signer authorization, DoS caps, receipt verify+bind, patched dependencies, and a `cargo audit` / `cargo deny` CI gate. Current dependency security requires a fresh audit.

**Validated on real hardware:**
- Two-node, DHT-routed inference demo — one machine loads a model, another `curl`s `localhost:11434/api/chat` and gets a signed result routed over libp2p.
- Real embeddings on Apple Metal — `nomic-embed-text` via `/api/embed`, 768-dim vectors with correct semantics (`cos(cat,kitten)=0.74`, `cos(cat,finance)=0.30`).
- An authz-gate demo — default-deny rejects an unauthorized routed job; the operator escape hatch accepts it with `X-Lucid-Receipt-Verified: true`.

**Recorded checkpoint QA (August 2026):** macOS ARM64 460 tests passed / 2 ignored; Ubuntu x86_64 450 passed / 2 ignored, release build and strict Clippy passed. These are historical results for the source identified in the [checkpoint record](memory-bank/tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md), not a claim of fresh qualification or a released v0.2.

**November follow-up:** the November 7 macOS run and separate real llama-server/Gemma smoke passed, while repeated backend suites exposed an intermittent fixture startup failure. The November 9 CI repair updates the affected dependencies, isolates macOS fixture subprocesses and corrects the relay idle-timeout assertion. The macOS workspace passes 460 tests (2 ignored), formatting and strict Clippy; the dependency audit reports zero vulnerabilities and zero yanked packages under the existing policy. Four transitive unmaintained warnings remain visible. See [current qualification evidence](memory-bank/progress.md#ci-security-and-test-reliability-refresh-2026-11-09); physical-network and release acceptance gates remain open.

---

## What works today (v0.1)

- **Ollama-compatible API** on `:11434` — compatible clients can use the supported endpoints without a Phase SDK; qualify the actual client and options rather than assuming complete upstream API parity:
  `/api/chat`, `/api/generate` (NDJSON streaming) · `/api/embed`, `/api/embeddings` (vectors) · `/api/pull` (registers a local model) · `/api/tags`, `/api/show`, `/api/version`.
- **Workers:** `llama.cpp` (real inference + embeddings via `llama-server`, any of its Metal/CUDA/ROCm/Vulkan backends) and an in-tree `echo` worker (no GPU, for dev/CI/demos).
- **Local-or-DHT routing** — per-request: serve locally if the model is loaded, else discover a peer on the Kademlia DHT and relay the job, with **multi-peer failover**. Response headers report where it ran (`X-Lucid-Routed-Via`) and whether the peer's signed receipt verified (`X-Lucid-Receipt-Verified`).
- **Attributable output integrity** — every result carries a streaming SHA-256 commitment in an Ed25519 `SignedReceipt`; verification binds signature, job, serving PeerId and delivered output. This does not prove correct model execution.
- **Operator policy** — a hot-reloadable `policy.toml`: serve-model allowlist, auto-pause on battery / thermal / time-of-day / concurrency, a signer allowlist, and a `manual_pause`. Self-initiated requests bypass the donation-protection gates so the operator can always use their own GPU.
- **WAN bootstrap** — stable libp2p ports, IPv6 listen, persistent identity, explicit `--bootstrap-peer`, and DNS TXT-record bootstrap (`--bootstrap-dns`, PeerID-pinned).
- **Plasm** — the reference WASM node (`plasmd`): signed-manifest WASM execution in a sandboxed wasmtime runtime, content-addressed artifact serving, and the Phase Boot netboot/kexec pipeline.

---

## Current checkpoint and remaining roadmap

The protocol remains workload-neutral. Implementation does not imply release acceptance; the [v0.2 tracker](memory-bank/releases/lucid-v0.2/README.md#progress-tracker) records each gate.

**v0.2 — implemented, awaiting acceptance**
- Content-derived CIDs, signed alias/provider records, verified network pull and resumable transfer.
- Live peer streaming with cancellation, backpressure, ordered frames and terminal receipt binding.
- Circuit relay, AutoNAT and DCUtR machinery; public rendezvous serving stays disabled pending registration quotas.
- Local evidence/reputation and bounded deterministic redundant checks. Agreement is empirical evidence, not proof of model execution.
- Experimental pinned MLX subprocess adapter, pending real Apple Silicon acceptance.

**Next evidence gates**
- Demonstrate the [intended stream](memory-bank/releases/lucid-v0.2/intended-stream-vertical-slice.md) across physical networks and consumer NAT using a real model, verified pull/resume, live remote output, cancellation and invalid-content/receipt rejection.
- Evaluate independent correctness-validation methods against existing work such as TOPLOC and OpenHydra's challenge/replay mechanisms, with exact model/backend binding and explicit limitations.
- Measure independent whole-model jobs across heterogeneous peers before proposing tensor sharding. **ShardWorker remains research-blocked**; no production worker exists.
- Finish Linux ARM64, real MLX, rollback, clean-host and public-load qualification.

**v0.3 — privacy**
- Research prompt confidentiality from the serving operator. Encrypted transport and onion routing alone do not hide a prompt from the machine executing it; a stronger claim requires a separately evaluated construction. The current local-only toggle keeps requests on the operator's machine.

**Backends (LUCID)**
- **MLX worker** — experimental implementation exists; real model, lifecycle, peer-routing and power/performance acceptance remain.
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
cargo test --workspace
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
- [Competitive ecosystem review](memory-bank/research/2026-06-21-ai-sovereignty-and-local-inference-ecosystem.md#competitive-reassessment--2026-11-07) — direct peers, evidence limits and Phase's remaining differentiation
- [LUCID v0.2 tracker](memory-bank/releases/lucid-v0.2/README.md) — authoritative implementation and release-acceptance status
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
