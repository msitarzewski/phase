# Research Report — Apple Core AI & Foundation Models: impact on Phase / Plasm / LUCID

**Date:** 2026-06-09
**Author:** Michael S. + agent
**Status:** Record of investigation. Conclusions inform the Apple-worker backlog and a hard constraint on Foundation Models. No code action taken.
**Method:** Read the JS-rendered Apple docs via a headless Chrome (WebFetch returned only titles); cross-checked `coreai-torch` docs and the Foundation Models acceptable-use page.

---

## TL;DR

- **Two different Apple frameworks, often conflated.** *Core AI* (the iOS 27 / macOS successor to Core ML) is a **bring-your-own-model on-device runtime**. *Foundation Models* is Apple's **own LLM** exposed via Swift, with cloud routing to Private Cloud Compute.
- **Foundation Models is a hard no for LUCID** — on two independent axes (redistribution model + an acceptable-use policy that's structurally unenforceable for a neutral relay). Not in the plan; recorded as a guardrail so it stays out.
- **Core AI is a legitimate, license-clean path to run *our own* open-weights models on device — including the Neural Engine.** It's purpose-built for modern LLMs (ships attention/RoPE/RMSNorm/MoE conversion composites). It's the efficiency play for always-on contributor Macs, distinct from MLX (GPU). More integration work than MLX, and gated on confirming KV-cache support.
- **Phase substrate: untouched.** **Plasm: orthogonal.** **LUCID: a future ANE worker backend + a sharpened strategic position.**

---

## 1. Core AI — what it is (from the docs)

> "Core AI helps you build, run, and deploy AI models in your app. Designed with Apple silicon in mind, Core AI allows your app to use the latest model architectures and inference techniques across the CPU, GPU, and Neural Engine."

The Core ML *successor* (Beta; iOS 27 / macOS). A **bring-your-own-model on-device runtime**:

- **`.aimodel` format**, produced by converting a PyTorch model via the **Core AI PyTorch Extensions** (`coreai-torch`, apple.github.io/coreai-torch). A model contains one or more **inference functions**.
- **LLM-aware conversion.** `coreai-torch` exposes "well-known building blocks — such as **attention, RoPE embeddings, RMSNorm, and gather-matmul (the MoE primitive)** — as PyTorch modules." That's the modern transformer/LLM stack (RoPE+RMSNorm = Llama-family; MoE = Mixtral/Qwen/DeepSeek). This is the key fact: Core AI is built for the LLM era, not just the classifier-era tensor graphs old Core ML targeted.
- **Compute targets:** CPU / GPU / **Neural Engine** (`ComputeUnitKind`).
- **API surface (pure in-app Swift):** `AIModel`, `AIModelAsset`, `InferenceFunction`, `InferenceValue`, `NDArray`/`NDArrayDescriptor`, `ImageDescriptor`, `ComputeStream` (async/streaming work), `AIModelCache`, `SpecializationOptions`, `AssetError`.
- **AOT compilation**, model specialization + caching, **weight externalization** (weights as separate assets, not baked into the binary — relevant to content-addressed model distribution), Xcode debug/profiling integration.
- **On-device, private, offline, no per-inference cost.** Model bundled in the app or downloaded at runtime.
- **No server, no daemon, no OpenAI/Ollama endpoint.** It is an in-app Swift framework, not a serving layer.

## 2. Foundation Models — what it is (separate framework)

Apple's **own** on-device LLM via a native Swift API (language understanding, structured output, tool calling). At WWDC26 it gained image input, **server-side routing to Private Cloud Compute** (and other server model providers) for tasks needing more reasoning/context, and dynamic profiles. Free cloud access for small developers (App Store Small Business Program, <2M downloads). It is a *managed* model you do not choose or control.

## 3. Foundation Models — why it's a hard no for LUCID

The [acceptable-use requirements](https://developer.apple.com/apple-intelligence/acceptable-use-requirements-for-the-foundation-models-framework/) are a **content/conduct restriction list** the developer must *guarantee*: no illegal use, violence, defamation, pornography/CSAM, self-harm, fraud, **regulated healthcare/legal/financial services**, employment or criminal-risk assessment, law enforcement, social scoring, biometric inference, network compromise, weapons, IP infringement, derogatory-to-Apple, circumventing guardrails, reproducing/citing training data, or generating scholarly products. Formal license: Apple Developer Program License Agreement **§3.3.11(A)**.

**Two independent reasons it can't back a LUCID worker:**

1. **Redistribution model.** Foundation Models is licensed for use *within your app, on the user's device, for that user* — not to relay/resell its compute to arbitrary network peers. LUCID's purpose is exactly that relay. The "more reasoning" path also routes to **Apple's Private Cloud Compute** — the centralized off-switch LUCID exists to avoid.
2. **The AUP is structurally unenforceable for a neutral relay.** LUCID serves peer prompts it cannot inspect or control. The operator would be contractually responsible for AUP compliance over uninspectable third-party prompts — in violation the instant any peer requests something on the list (regulated financial advice, guardrail circumvention, etc.). A neutral relay categorically cannot guarantee this.

**Verdict: never build a Foundation Models backend for LUCID.** This is a guardrail, not a roadmap item.

## 4. Core AI for LUCID — the path Michael actually intended

Running *our own* converted open-weights models (Qwen/Llama/etc.) on device, the way we'd run MLX. **This is legitimate and license-clean** — Core AI is just a runtime; the `.aimodel` is *your* weights, which you already have the rights to. It carries **none of the Foundation Models ToS problem**.

**Core AI vs MLX (honest comparison):**

| Axis | MLX (deferred `MlxWorker`) | Core AI (`CoreAIWorker`, hypothetical) |
|---|---|---|
| Turnkey serving | Yes — `mlx-lm` = tokenizer + KV-cache + sampling + OpenAI-compatible server | **No** — Core AI gives the accelerated forward pass + LLM building blocks; you supply the generation harness (tokenizer, sampling loop, KV-cache orchestration) |
| Compute target | GPU (Metal) — fast peak, hot, power-hungry | **Neural Engine** — lower watts, less heat |
| Streaming | mlx-lm handles it | `ComputeStream` (async) |
| Rust-daemon bridge | Swift/Python subprocess | Swift subprocess / XPC (same shape) |
| Model license | your weights | your weights |

**Why Core AI is *not* redundant with MLX:** the Neural Engine is the **efficiency play for always-on contributor nodes** — the "donate your idle Mac's compute overnight" use case, which pairs with the battery/thermal auto-pause policy already built (LUCID M7 / SEC-09). MLX pegs the GPU (faster, but the resource a user wants for their own work, and it cooks the laptop). MLX = peak throughput; Core AI/ANE = efficient background donation. Different profiles → a real reason to want both, eventually.

**The make-or-break to verify before committing a `CoreAIWorker`:** explicit **KV-cache / stateful autoregressive decode** in the public Core AI API. The composites seen are forward-pass building blocks; efficient LLM serving needs persistent KV-cache across decode steps. Apple's own Foundation Models do stateful ANE decode, so the silicon/stack can — the open question is whether the *public* API exposes cache management or you hand-roll it. (Next step if pursued: read the `ComputeStream` / `InferenceFunction` / `AIModelCache` symbol pages.)

## 5. Cross-project impact

- **Phase (substrate):** zero protocol impact — workload-agnostic; Core AI doesn't touch libp2p/identity/manifest/receipt. Forward angles: (a) Apple devices as *efficient* nodes via ANE; (b) model-format fragmentation for the v0.2 content-addressed model distribution (GGUF vs `.aimodel` vs MLX vs safetensors) — the CID/registry layer would need to be format-aware if it ever serves Apple-native models; weight **externalization** in Core AI is friendly to content-addressing.
- **Plasm (WASM node):** essentially orthogonal — Core AI is native model inference, Plasm runs WASM. No API interaction.
- **LUCID (inference flagship):**
  - **Backend option:** `CoreAIWorker` as a later, ANE-optimized Apple backend — *after* MLX (which is the faster path via turnkey mlx-lm), justified by the contributor-efficiency profile. Gated on the KV-cache question.
  - **Distribution risk (consumer app, future):** an iOS/macOS LUCID app ships through the App Store, where review favors Apple's own frameworks and may scrutinize an app that downloads arbitrary models and serves compute to a P2P network. Affects the eventual consumer surface, not the daemon.
  - **Strategic position (sharpened, not threatened):** Apple making good on-device AI the default *validates* the on-device thesis, but it's single-vendor, Apple-chosen models, Apple-only platforms, with a **cloud fallback to Apple's servers** — the captured version. LUCID's differentiation gets cleaner: open weights you choose, cross-vendor, no single company's cloud, works where Apple doesn't, can't be switched off. A positioning asset for the launch narrative (Apple proves the demand; LUCID is the un-captured one).

## 6. Recommendations recorded

1. **Constraint:** No Foundation Models backend for LUCID — two independent reasons (above). → `decisions.md`.
2. **Backlog (reframed):** `CoreAIWorker` = license-clean **ANE** backend for our own models; the efficiency play for always-on contributor Macs; more harness work than MLX; **verify KV-cache support first**. After MLX. → `activeContext.md` next-thread.
3. **Positioning note:** Apple's on-device + PCC push as a "validates demand, contrasts with our un-captured model" talking point. → launch narrative.

## 7. Open questions

- Does the public Core AI API expose **KV-cache / stateful decode**, or must it be hand-rolled? (Blocks a `CoreAIWorker` decision.)
- App Store review posture toward a P2P-compute-serving consumer app.
- v0.2 content-addressed distribution across heterogeneous model formats.

## Sources

- [Core AI — overview](https://developer.apple.com/documentation/coreai/)
- [Core AI — integrating on-device AI models](https://developer.apple.com/documentation/coreai/integrating-on-device-ai-models-in-your-app-with-core-ai)
- [Core AI PyTorch Extensions (`coreai-torch`)](https://apple.github.io/coreai-torch)
- [Foundation Models](https://developer.apple.com/documentation/FoundationModels)
- [Foundation Models — acceptable use requirements](https://developer.apple.com/apple-intelligence/acceptable-use-requirements-for-the-foundation-models-framework/)
- [Apple Developer Program License Agreement (§3.3.11(A))](https://developer.apple.com/support/terms/apple-developer-program-license-agreement/)
- [Meet Core AI — WWDC26](https://developer.apple.com/videos/play/wwdc2026/324/)
