# Mission

**Phase is the open verifiable compute protocol. LUCID is the first flagship built on it that the world urgently needs: open inference, on the public's hardware, that nobody can turn off.**

---

## For everyone

The smartest AI today costs twenty dollars a month, doesn't work in many countries, and tells you to come back tomorrow once you've used it enough.

LUCID is the opposite. It's smart AI — the good kind — that anyone can use, anywhere, for free, that doesn't run out, and that nobody can take away.

It works because thousands of regular people share a little of their computer's spare power. A gaming PC in Texas. A water-cooled rig in rural Kenya on a Starlink connection. A university lab in São Paulo. A laptop in Warsaw, running quietly overnight. Each one alone is small. Together they're a global network that nobody owns and nobody controls.

**If you want to use AI, install LUCID. It's free.**

**If you have a gaming computer or a spare GPU, you can be part of the network that makes it free.**

---

## Thesis

In 2026, frontier-quality open-weights models (DeepSeek-V4, Qwen3-Next, Llama 4 series) are competitive with closed APIs at the median use case. Hundreds of millions of consumer GPUs sit idle most hours of the day. The hard parts of distributed inference — cross-vendor GPU support (llama.cpp's Metal/CUDA/ROCm/Vulkan backends), heterogeneous device sharding (exo's partitioning), open weight distribution (HuggingFace, IPFS) — are already solved upstream.

What is *not* solved is the substrate: a credibly-neutral, vendor-agnostic protocol for advertising, discovering, and verifying compute across machines that do not trust each other, without payments, without KYC, without lock-in.

That substrate is **Phase**. The flagship application built on it is **LUCID**.

---

## What we are building (and what we are not)

### We are building

- An **open compute protocol** — discovery, identity, signed manifests, signed receipts, verifiable execution. Workload-agnostic by design.
- A **family of reference node implementations** that speak the protocol:
  - **Plasm** — WASM workloads (existing work, repositioned)
  - **LUCID** — GPU inference (new flagship)
  - Future: phase-render, phase-science, anything that benefits from verifiable distributed compute
- A **bootstrap layer** (Phase Boot) for putting Phase nodes onto otherwise-idle hardware via USB / netboot.
- A **foundation-shaped governance** structure — eventually a 501(c)(3), Mozilla/Tor-shaped, that holds the trademark and accepts grants without becoming captured by any single sponsor.

### We are not building

- A centralized inference service competing with OpenAI / Anthropic / Together / Fireworks.
- A payment rail, marketplace, KYC system, or token. Compute is volunteered; no money flows through the protocol.
- A blockchain. The substrate is libp2p + Ed25519, not consensus.
- A closed commercial product. The whole point is that nobody can turn it off.

The motivation is **principle**, not margin. Folding@home, not Foldit. Tor, not Telegram.

---

## Architecture (the layering)

```
                ┌────────────────────────────────────────────────┐
                │  Consumer apps (already exist — just point     │
                │  them at LUCID's :11434 like local Ollama):    │
                │  Open WebUI · Continue · Cursor · Zed ·        │
                │  LibreChat · LangChain · opencode · curl       │
                └────────────────────────┬───────────────────────┘
                                         │ Ollama API
                                         ▼
PhaseBased  ──  foundation umbrella (eventually 501(c)(3))
│
├── Phase  ────────────────────  github.com/phasebased/phase
│   │   "The open verifiable compute protocol"   Apache-2.0
│   │
│   ├── spec/                          Protocol specification
│   ├── crates/
│   │   ├── phase-net                  libp2p · DHT · mDNS · Noise+QUIC
│   │   ├── phase-identity             Ed25519 · persistent keypair
│   │   ├── phase-manifest             SignedManifest<T>
│   │   ├── phase-receipt              SignedReceipt<T>
│   │   ├── phase-protocol             JobSpec enum · Worker trait
│   │   ├── phase-artifact-server      Content-addressed HTTP
│   │   └── plasm/                     Reference WASM node
│   │       └── WasmtimeWorker
│   └── boot/                          Phase Boot
│
└── LUCID  ───────────────────  crates/lucidd/ (will split to own repo)
    │   "Open inference on the public's hardware"   AGPL-3.0
    │
    └── crates/lucidd/
        ├── worker/                    LlamaCpp · MLX · ExoProxy (v2)
        ├── api/                       Ollama :11434 · OpenAI :8000 (later)
        ├── router/                    local-fits? else DHT lookup
        └── registry/                  DHT model_cid → peer_id mapping
```

Phase and LUCID currently share one repository for development velocity. They will split into separate repos once the protocol interfaces stabilize. See [STRUCTURE.md (future)] for the split policy.

---

## Why now

- **Open weights are competitive.** 2026 frontier models are nearly indistinguishable from closed APIs at the median use case. The "donate your GPU for a worse model" objection no longer applies.
- **AI sovereignty is a political reality.** Export restrictions on frontier hardware and weights, API gating, board-level control disputes at major labs — there's a growing constituency for inference infrastructure that cannot be unilaterally switched off.
- **Consumer GPUs are everywhere and mostly idle.** Aggregate consumer compute capacity is enormous; the missing piece has always been coordination, not silicon.
- **Petals proved the protocol; the movement was missing.** Distributed inference works technically. What it has lacked is brand, packaging, and ideological energy.
- **Phase already exists.** The libp2p + signed-receipts + DHT substrate is ~60-70% built. The work ahead is extraction, generalization, and a flagship — not greenfield.

---

## Operating principles

1. **Substrate first, then product.** Phase has to be credibly workload-neutral so anything — inference, render, science — can build on it. We resist the temptation to specialize the protocol around inference. The protocol does not know what a "model" is.

2. **Compatibility as the wedge.** LUCID presents the Ollama API on `:11434`. Every existing AI tool (Open WebUI, Continue, Cursor, LangChain, opencode) just works without modification. No new client SDK adoption required for v0.1. Familiar interface, distributed substrate underneath.

3. **Privacy is foundational, not roadmap.** Per-request prompt visibility is the brutal honest UX cost of distributed routing. UI must communicate this from day one. "Local-only" toggle ships in v0.1. Real cryptographic prompt privacy (onion-routing, split-prompt) lands in v0.3 — but it is *committed*, not aspirational.

4. **Verification beats payments.** No tokens, no rate-limit-by-pay. Abuse is the real threat model, addressed by community moderation (Tor-shaped), redundant execution, and reputation. Not blockchain-shaped.

5. **No vendor capture.** Apache-2.0 for the protocol so anyone — including governments hostile to other partners — can adopt without legal friction. AGPL-3.0 for LUCID so no one can fork-and-close the flagship. Take public dev resources from NVIDIA, AMD, Apple; never take their official blessing.

6. **Foundation, not company.** The 5-year shape is a 501(c)(3) that holds the Phase trademark and accepts grants from anywhere — Mozilla, NSF, EU Horizon, Sovereign Tech Fund, NVIDIA Inception, AMD, Apple — without any single one capturing the project. Sustainability is donations and grants, not revenue.

---

## Current state (as of 2026-05-26)

| Component | Status |
|---|---|
| Phase Open MVP (libp2p, DHT, signed receipts, WASM execution) | ✅ Complete (Nov 2025) |
| Phase Boot (netboot, hardware testing on x86_64 + ARM64) | ✅ Implemented (Nov 2025) |
| Netboot Provider (HTTP artifact server, manifest signing) | ✅ Complete (Nov 2025) |
| Repository | 80 tests passing, 5,743 lines Rust, dormant since 2025-11-30 |

**Next:**
1. **[phase-core release](releases/phase-core/)** — in-place refactor of `daemon/` into proper library crates (`phase-net`, `phase-identity`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`) plus repositioning of the existing daemon as `crates/plasm/`. No new functionality; pure structure.
2. **[lucid release](releases/lucid/)** — the inference flagship. New `crates/lucidd/` daemon depending on the extracted Phase libraries. Workers (llama.cpp, MLX), Ollama API surface, local-or-DHT routing, signed receipts.

End state of these two releases: a runnable demo where two machines on different networks both run `lucidd`, one has a model loaded, and the other can `curl http://localhost:11434/api/chat` and get a signed inference result routed through the Phase DHT.

---

## Files

- [decisions.md](decisions.md) — Architectural decision record (existing)
- [projectRules.md](projectRules.md) — Coding and contribution conventions (existing)
- [activeContext.md](activeContext.md) — Current sprint state (existing)
- [progress.md](progress.md) — Historical milestone log (existing)
- [releases/phase-core/](releases/phase-core/) — Library extraction release plan
- [releases/lucid/](releases/lucid/) — Inference flagship release plan
