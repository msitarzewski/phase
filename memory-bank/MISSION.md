# Mission

**Phase is the open verifiable compute protocol. LUCID is the first flagship built on it that the world urgently needs: open inference, on the public's hardware, that nobody can turn off.**

**Positioning review — 2026-11-07:** this document states the intended end state. Current deployment and implementation guarantees live in the [LUCID v0.2 tracker](releases/lucid-v0.2/README.md#progress-tracker). Direct inference-network peers now exist; the [competitive reassessment](research/2026-06-21-ai-sovereignty-and-local-inference-ecosystem.md#competitive-reassessment--2026-11-07) supersedes the older uniqueness claims. Signed receipts establish provenance and transcript integrity; independent correctness validation, serving-operator prompt confidentiality and unrestricted public access are not shipped guarantees.

---

## For everyone

The smartest AI today costs twenty dollars a month, doesn't work in many countries, and tells you to come back tomorrow once you've used it enough.

LUCID is the opposite. It's smart AI — the good kind — that anyone can use, anywhere, for free, that doesn't run out, and that nobody can take away.

It works because thousands of regular people share a little of their computer's spare power. A gaming PC in Texas. A water-cooled rig in rural Kenya on a Starlink connection. A university lab in São Paulo. A laptop in Warsaw, running quietly overnight. Each one alone is small. Together they're a global network that nobody owns and nobody controls.

**If you want to use AI, install LUCID. It's free.**

**If you have a gaming computer or a spare GPU, you can be part of the network that makes it free.**

---

## Thesis

Open-weight models and consumer inference runtimes make useful volunteered inference possible. Existing projects provide model execution, distribution and cluster partitioning; Phase should reuse those capabilities. Their existence does not establish reliable consumer-NAT operation, affordable verification or useful tensor sharding across untrusted internet peers.

The contribution Phase aims to make is a credibly-neutral, workload-neutral protocol for advertising, discovering and checking compute evidence across independently operated machines, without protocol payments, KYC or vendor lock-in. This combination must be demonstrated; it is not a claim that decentralized inference, nonprofit compute or verification research lack prior art.

That substrate is **Phase**. The flagship application built on it is **LUCID**.

---

## What we are building (and what we are not)

### We are building

- An **open compute protocol** — discovery, identity, signed manifests, signed receipts and explicitly scoped execution evidence. Workload-agnostic by design; current transcript integrity is separate from future computation-correctness validation.
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
        ├── worker/                    LlamaCpp · MLX · ShardWorker (v2)
        ├── api/                       Ollama :11434 · OpenAI :8000 (later)
        ├── router/                    local-fits? else DHT lookup
        └── registry/                  DHT model_cid → peer_id mapping
```

Phase and LUCID currently share one repository for development velocity. They will split into separate repos once the protocol interfaces stabilize. See [STRUCTURE.md (future)] for the split policy.

---

## Why now

- **Existing execution projects are reusable.** [llama.cpp](https://github.com/ggml-org/llama.cpp), [exo](https://github.com/exo-explore/exo) and [Petals](https://github.com/bigscience-workshop/petals) supply relevant execution and distribution capabilities. Performance and model quality must be evaluated on the actual workload and hardware.
- **Independent operation is a design requirement.** A provider-controlled account, gateway or payment layer is a control point. Phase's mission is to avoid making one mandatory. This rationale does not depend on a particular provider-policy or political anecdote; older June claims are retained as unvalidated historical notes in the research document.
- **Volunteer compute has working precedent.** [AI Horde](https://github.com/Haidra-Org/AI-Horde) demonstrates donated inference with anonymous access. Phase must earn its distinction through peer operation and evidence guarantees rather than claiming the volunteer model is new.
- **Direct peers sharpen the execution requirement.** [OpenHydra](https://github.com/samtroberts/openhydra) and [KwaaiNet](https://github.com/Kwaai-AI-Lab/KwaaiNet) overlap with LUCID technically and, in KwaaiNet's case, with nonprofit governance. Phase needs reproducible evidence of its particular guarantees.
- **Phase already has a substrate and flagships.** Phase Core, Plasm and LUCID v0.1/v0.1.1 are shipped history. The v0.2 checkpoint implements content, streaming and reachability components; [physical-network and trust acceptance](releases/lucid-v0.2/README.md#progress-tracker) remain open.

---

## Operating principles

1. **Substrate first, then product.** Phase has to be credibly workload-neutral so anything — inference, render, science — can build on it. We resist the temptation to specialize the protocol around inference. The protocol does not know what a "model" is.

2. **Compatibility as the wedge.** LUCID presents an Ollama-compatible API on `:11434`. Clients using the supported endpoints can connect without a Phase SDK. Qualify actual clients and options; the compatibility promise is not a claim of complete upstream API parity. Familiar interfaces should make distributed compute easier to adopt.

3. **Privacy claims must match the trust boundary.** Serving operators can see remote prompts today; clients must disclose that, and the local-only toggle keeps a request local. Stronger confidentiality remains a research goal. Transport encryption and onion routing alone do not hide input from the executing machine; any future construction needs its own threat model and measured acceptance.

4. **Verification beats payments.** No tokens, no rate-limit-by-pay. Abuse controls, bounded redundant checks and local reputation must be established without a payment rail. Signed receipts bind a job, peer and delivered transcript; they do not prove the claimed model executed. Independent correctness evidence must identify its method, exact content/backend binding, validator, cost, confidence and exclusions. Default authorization remains deny-by-default until the approved correctness/load/abuse/public-network gate passes.

5. **No vendor capture.** Apache-2.0 for the protocol so anyone — including governments hostile to other partners — can adopt without legal friction. AGPL-3.0 for LUCID so no one can fork-and-close the flagship. Take public dev resources from NVIDIA, AMD, Apple; never take their official blessing.

6. **Foundation, not company.** The 5-year shape is a 501(c)(3) that holds the Phase trademark and accepts grants from anywhere — Mozilla, NSF, EU Horizon, Sovereign Tech Fund, NVIDIA Inception, AMD, Apple — without any single one capturing the project. Sustainability is donations and grants, not revenue.

---

## Historical starting state (2026-05-26)

Preserved for origin context only. Use the [current tracker](releases/lucid-v0.2/README.md#progress-tracker) for implementation, deployment and acceptance status.

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
- [research/2026-06-21-ai-sovereignty-and-local-inference-ecosystem.md](research/2026-06-21-ai-sovereignty-and-local-inference-ecosystem.md) — June 2026 sovereignty events + local-inference ecosystem; real-world validation of the thesis
