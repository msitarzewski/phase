# Release: LUCID — Layered Universal Compute Inference Daemon

**Status: RELEASED 2026-05-28. M8 live two-node demo proven on real hardware.**
M1 (scaffold + spike), M2 (LlamaCppWorker), M5 (router), M6 (model registry), M7 (policy + auto-pause), M8 (live demo) all shipped. M4 (Ollama API) demo-sufficient — `/api/chat`, `/api/generate`, `/api/tags`, `/api/show`, `/api/version`. M3 (MlxWorker) deferred to v0.1.1. Asciinema artifact at `dist/demos/lucid-2node-demo.cast`. Three v0.1 bugs caught during the demo session and fixed in place: `ModelCid::from_model_id` deterministic placeholder, `Kademlia::set_mode(Server)`, JSON peer-relay encoding (replacing bincode for `SignedManifest`). See `memory-bank/activeContext.md` for the postmortem.

**Scope:** New Phase node implementation focused on open GPU inference. Exposes Ollama-compatible API on `:11434`, runs llama.cpp / MLX backends locally, routes overflow requests across the Phase DHT to peers with the requested model loaded.

**Outcome:** Anyone running `lucidd` can serve inference requests for any model that fits on their hardware, and route requests they cannot serve to peers via Phase's DHT. Existing AI tooling (Open WebUI, Continue, Cursor, Zed, LangChain, opencode, raw curl) works unmodified — LUCID presents as a local Ollama instance.

**Tagline:** *Open inference, on the public's hardware, that can't be turned off.*

**Target Platforms:** macOS ARM64 (MLX), Linux x86_64 + NVIDIA (CUDA), Linux x86_64 + AMD (ROCm), Linux ARM64, Windows (CUDA, via WSL2 or native).

**License:** AGPL-3.0 (closes the SaaS loophole — anyone running LUCID-as-a-service must release modifications).

---

## Vision

```
                  USER
                    │
        ┌───────────┴───────────┐
        │   Consumer apps       │  Already exist. Just point at :11434.
        │   - Open WebUI        │
        │   - Continue / Zed    │
        │   - Cursor            │
        │   - LangChain         │
        │   - opencode          │
        │   - curl              │
        └───────────┬───────────┘
                    │ Ollama API on :11434
                    ▼
   ╔══════════════════════════════════════════════════╗
   ║                    LUCID                          ║
   ║  ┌────────────────────────────────────────────┐  ║
   ║  │              api/ollama.rs                  │  ║
   ║  │       /api/chat · /api/generate ·           │  ║
   ║  │       /api/tags · /api/embeddings           │  ║
   ║  └─────────────────┬──────────────────────────┘  ║
   ║                    ▼                              ║
   ║  ┌────────────────────────────────────────────┐  ║
   ║  │             router/decide.rs                │  ║
   ║  │  local model fits?  → run locally          │  ║
   ║  │  else              → DHT lookup → peer     │  ║
   ║  │  flagged "local"?  → refuse to route       │  ║
   ║  └──────┬────────────────────────┬────────────┘  ║
   ║         │                        │                ║
   ║         ▼                        ▼                ║
   ║  ┌─────────────┐         ┌────────────────────┐  ║
   ║  │   worker/   │         │  registry/         │  ║
   ║  │ LlamaCpp    │         │  model_cid →       │  ║
   ║  │ MLX         │         │  [peer_id, …]      │  ║
   ║  │ ExoProxy v2 │         │  (via Phase DHT)   │  ║
   ║  └──────┬──────┘         └────────────────────┘  ║
   ║         │                                         ║
   ║         ▼                                         ║
   ║  ╔═══════════════════════════════════════════╗   ║
   ║  ║       impl phase-protocol::Worker          ║   ║
   ║  ║  →  SignedReceipt on every inference       ║   ║
   ║  ╚═══════════════════════════════════════════╝   ║
   ╚══════════════════════════════════════════════════╝
                       │
                       ▼ Depends on (Phase libraries from phase-core release):
   phase-net · phase-identity · phase-manifest · phase-receipt ·
   phase-protocol · phase-artifact-server
```

The user's existing client (Open WebUI / Cursor / Zed / Continue / opencode / etc.) thinks it is talking to a local Ollama instance. LUCID transparently routes the request to the best-available node — local if the model fits, peer if it doesn't, signed-receipt-verified either way.

---

## Why LUCID

- **Open weights are competitive in 2026.** DeepSeek-V4, Qwen3-Next, Llama 4 — frontier-quality at the median use case. Donating a GPU no longer means running a worse model.
- **Hundreds of millions of consumer GPUs sit idle.** Aggregate capacity is enormous; coordination is the missing piece.
- **AI sovereignty is a political reality.** Export restrictions, API gating, board-level control disputes at major labs — there is a real and growing constituency for inference that can't be switched off.
- **The protocol is already built.** Phase (after the phase-core release) ships discovery, identity, manifest signing, signed receipts. LUCID is the workload-specific layer on top — not a from-scratch network.
- **Compatibility is the wedge.** Speak Ollama's API. Every existing tool works unchanged. The user doesn't even need to know they're on a distributed network.

---

## Milestones

| ID | Name | Description |
|----|------|-------------|
| M1 | Workspace Scaffold | `crates/lucidd/` inside Phase monorepo; depends on `phase-*` crates via path |
| M2 | `LlamaCppWorker` | Shell out to `llama-server`; sign receipts via `phase-receipt`; serve over Phase protocol |
| M3 | `MlxWorker` | Native Apple Silicon inference via MLX (subprocess to `mlx-lm` or in-process via mlx-rs when available) |
| M4 | Ollama API Surface | `:11434` HTTP server: `/api/tags`, `/api/chat`, `/api/generate`, `/api/embeddings`, `/api/pull` (streaming via SSE) |
| M5 | Local-or-DHT Router | Per-request decision: run locally if model loaded + VRAM available, else lookup peer via Phase DHT |
| M6 | Model Registry | DHT advertisement of loaded models — `(model_cid, peer_id) → ModelCapabilities` |
| M7 | Privacy Surface | Per-request "local-only" header/flag; clear UI/log communication of routing decisions; no telemetry |
| M8 | End-to-End Demo | Two machines on different networks; one has model loaded; other curls `/api/chat` and gets signed result via DHT routing |

---

## Definition of Done

1. **`curl http://localhost:11434/api/chat`** works with the same JSON contract as Ollama (drop-in compatibility)
2. **Local inference is byte-for-byte identical to direct `llama-server`** — LUCID adds zero perceptible overhead for local-served requests
3. **DHT-routed inference returns signed receipts** linking request → peer → model_cid → output hash, verifiable by any third party with the peer's public key
4. **Two-node demo passes**: machine A (one network) loads DeepSeek-V3; machine B (different network) `curl`s `/api/chat` for that model; output returns with A's signed receipt; B never had the weights
5. **"Local-only" mode honored**: requests flagged private never leave the local machine even when routing to a peer would be faster
6. **At least one existing client works unmodified**: Open WebUI, Continue, Cursor, or Zed pointed at `localhost:11434` works as expected with no LUCID-specific configuration
7. **No telemetry**: zero outbound network calls except the Phase DHT for peer discovery and the configured llama-server/mlx subprocess
8. **AGPL-3.0 license declared** in `crates/lucidd/Cargo.toml` (distinct from Phase's Apache-2.0)

---

## Acceptance Criteria

- Cross-vendor GPU: at least two different backends working (e.g., Apple MLX + NVIDIA CUDA via llama.cpp)
- Privacy story documented and visible in UI / `--help` output, not just buried in docs
- Per-request receipts verifiable with `phase-receipt`'s verifier utility
- Model registry survives daemon restart (uses persistent Phase identity from phase-identity)
- Streaming responses (SSE) work correctly through both local and DHT-routed paths

---

## Dependencies

- **[phase-core release](../phase-core/) must complete** (M1-M8 of that release)
- **Upstream tools assumed available:**
  - `llama.cpp` — specifically `llama-server` binary, built with Metal / CUDA / ROCm / Vulkan support as appropriate per platform
  - `mlx-lm` (Python) for the MLX backend in v0.1 (subprocess interface); in-process `mlx-rs` if it stabilizes
- **NOT required:**
  - TensorRT-LLM (NVIDIA-proprietary; deliberately deferred to keep vendor-neutral posture)
  - Any closed-source inference engine

---

## Out of Scope (v0.1)

- **Multi-node sharding for big MoE models.** That is v0.2 (`ExoProxyWorker`); depends on Phase MoE-aware routing extensions.
- **Onion-routing of prompts.** Privacy v0.1 is "local-only mode." Real cryptographic prompt privacy (split-prompt, onion-routing) is v0.3 — committed, not aspirational.
- **Payment rails, KYC, marketplace logic.** Never. See [MISSION.md](../../MISSION.md).
- **OpenAI / llama.cpp-server API compatibility.** Ollama API only in v0.1. Other surfaces follow once routing logic is stable.
- **Fine-tuning, image generation, embeddings beyond what Ollama API exposes.** Future workloads, future workers.

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| First-token latency over WAN routing kills UX | Stream tokens via peer; document "expect 2-5s first token for routed requests"; local-only fast path for sensitive work |
| Per-request prompt privacy concerns kill adoption | Honest disclosure in UI from v0.1; "local-only" toggle prominent; no telemetry; visible routing-decision logs |
| Network weaponized for abuse on launch | Soft launch with allowlist of peers (Tor-shaped invitation graph); community moderation tooling before public DHT bootstrap |
| Model CID collisions / fake-model attacks | Ed25519 signatures over model weight CIDs at advertise time; hash verification at request time; reputation accrual on signed receipts |
| Ollama API moves faster than LUCID can track | Pin compatibility to a specific Ollama API version; document drift; refactor when divergence demands it |
| llama-server subprocess management proves fragile | Single supervised process per loaded model; auto-restart on crash; resource-aware loading/unloading |
| MLX backend stays Python-only | v0.1 uses subprocess to `mlx-lm` server mode; revisit when `mlx-rs` matures |

---

## Privacy & Trust Posture (the part most projects punt on)

LUCID's brutal honest cost is that **routed requests are visible to the peer serving them**. A user prompt sent to a stranger's GPU is, by default, readable by that stranger. This is the same trust model as Tor exit nodes for unencrypted traffic, and it must be communicated *honestly and prominently*, not buried.

v0.1 commitments:

- **Visible routing decisions.** Every request response includes a header indicating whether it was served locally or by a peer (and which peer's public key signed the receipt).
- **Local-only mode** is a first-class toggle in the API, the CLI, and any UI that ships. When set, requests refuse to route even at the cost of latency.
- **No telemetry.** Zero outbound calls beyond the Phase DHT (for discovery) and the configured local inference subprocess.
- **No logging of prompts on peer side by default.** Peers serving requests do not persist prompt text. (This is enforced by code, but ultimately depends on peer honesty — see "trust model" below.)

v0.3 roadmap (committed): split-prompt routing where no single peer sees the full request, and/or onion-routed forwarding for full prompt confidentiality from the routing layer.

The trust model is honest: **today, LUCID is for workloads you'd be okay running on a friend's GPU.** Sensitive work runs local-only. Eventually, cryptographic privacy makes the "friend's GPU" assumption unnecessary. We ship the honest version first.

---

## Release Owner

**Owner:** Michael S.
**Contributors:** Worker backends (llama.cpp, MLX), API compatibility (Ollama surface), Router (local vs DHT), Privacy surface, End-to-end testing

---

## Files

- [index.yaml](./index.yaml) — Machine-readable milestone index
- (tasks/M1/ through tasks/M8/ subdirectories will be created as work begins on each milestone)
