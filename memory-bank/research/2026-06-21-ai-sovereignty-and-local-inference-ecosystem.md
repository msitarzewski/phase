# Research — AI sovereignty and the distributed-compute ecosystem

**Original investigation:** 2026-06-21
**Last reviewed:** 2026-11-07
**Author:** Michael S. + agent
**Status:** Current competitive reassessment followed by a preserved historical investigation. The November assessment supersedes the June positioning recommendations and any implication that signed receipts prove correct model execution.
**Current method:** Primary project documentation, GitHub releases and selected source inspection. No competitor was installed, benchmarked or independently security-audited. Published functionality is distinguished from measured deployment evidence; project-reported network sizes are not treated as verified adoption.

## Competitive reassessment — 2026-11-07

### Finding

Direct peers now exist. Phase's opportunity is the combination of a workload-neutral open protocol, volunteered compute without protocol payments or KYC, independent operation, content-bound execution and meaningful, explicitly scoped verification. Individual parts of this combination already exist elsewhere. Neither an API-compatible server nor a signed receipt establishes a unique or complete product.

Phase itself is an implemented but incompletely qualified network: v0.1/v0.1.1 are shipped history; v0.2 is an approved implementation checkpoint, not a released public network. See [the authoritative tracker](../releases/lucid-v0.2/README.md#progress-tracker) and [the integrity/correctness decision](../decisions.md#2026-08-24-separate-transcript-integrity-from-computation-correctness-measure-whole-model-parallelism-before-tensor-sharding).

### Competitive map

| Project | Primary-source evidence | Relationship and limits |
|---|---|---|
| OpenHydra | [Repository](https://github.com/samtroberts/openhydra), [v0.3.24 release, 2026-10-22](https://github.com/samtroberts/openhydra/releases/tag/v0.3.24), [operator documentation](https://openhydra.co/docs) | Closest direct LUCID peer: Rust/libp2p, Kademlia/mDNS, NAT traversal, streamed inference, co-signed receipts and contribution-based access. Paid frontier tiers are explicitly on its roadmap. Release assets establish distribution, not independent proof of network scale or correctness. |
| KwaaiNet | [Repository and current-status caveats](https://github.com/Kwaai-AI-Lab/KwaaiNet), [v0.7.3 release, 2026-11-05](https://github.com/Kwaai-AI-Lab/KwaaiNet/releases/tag/v0.7.3) | Nonprofit-backed Rust/libp2p infrastructure, whole-model Ollama serving, CPU/CUDA block sharding and local credential-based trust. Nodes are person/organization-anchored. Its README documents an Apple Metal sharding limitation and uses whole-model serving there. |
| Petals | [Implementation and public-swarm instructions](https://github.com/bigscience-workshop/petals) | Established volunteer model-layer sharing and inference prior art. Public swarm processing is not prompt confidentiality; private swarms require chosen trusted participants. Availability of a specific model depends on the actual swarm. |
| exo | [Implementation, topology and RDMA prerequisites](https://github.com/exo-explore/exo) | Automatic device discovery, MLX, tensor parallelism and topology-aware local clusters. Candidate execution backend beneath Phase rather than evidence that untrusted WAN verification is solved. RDMA/TB5 benchmarks cannot be transferred to internet peers. |
| AI Horde | [Server architecture and anonymous access](https://github.com/Haidra-Org/AI-Horde), [public service](https://aihorde.net/) | Working nonprofit volunteer-compute model for images and text. A server queues requests, selects workers and manages kudos. This overlaps with LUCID/LUMEN's public benefit while retaining a central coordination service. |
| Hyperspace | [Node documentation](https://github.com/hyperspaceai/hyperspace-node) | Documents libp2p/DHT inference and several additional capabilities, plus points, staking and USDC settlement. Broad architectural overlap; financial machinery differs from Phase's mission. Advertised agent/download totals were not independently verified. |
| Cocoon | [Architecture and current control points](https://cocoon.org/architecture) | Confidential/attested inference using TEEs and TON payments. Documentation identifies currently team-operated proxies and registry control. Different hardware, payment and trust assumptions from ordinary volunteered consumer devices. |
| Public AI Inference Utility | [Contribution architecture](https://publicai.co/contributing), [API access](https://platform.publicai.co/docs) | Public-interest model access using partner compute and a managed gateway/account layer. Relevant governance and contributor precedent; not the same permissionless peer protocol. |

Mainstream remote access has improved too: [LM Link's February 2026 release](https://lmstudio.ai/changelog/lmstudio/lmstudio-v0.4.6) connects remote LM Studio instances through encrypted transport. Familiar APIs and remote access should be treated as compatibility requirements, not exclusive differentiators.

### OpenHydra: specific overlap worth tracking

- [Known issues](https://github.com/samtroberts/openhydra/blob/main/KNOWN_ISSUES.md) describe replay audits, serving-operator prompt visibility and payments not yet being live. Free operation today is different from a permanent no-payment protocol commitment.
- [Co-signed receipt implementation](https://github.com/samtroberts/openhydra/blob/main/protocol/src/receipts.rs) binds identities and accounting fields. Co-signing alone does not prove the claimed model executed.
- [Prompt-scoring implementation](https://github.com/samtroberts/openhydra/blob/main/agent/src/prompt_score.rs) contains networked, bounded model-distribution challenges.
- [Log-probability verification policy](https://github.com/samtroberts/openhydra/blob/main/protocol/src/logprob.rs) explicitly limits a pass to challenged requests: a provider able to distinguish challenges can serve them honestly while substituting ordinary traffic. A capability test is not a per-request correctness guarantee.

### Verification research and decision criteria

[TOPLOC](https://arxiv.org/abs/2501.16007) uses activation fingerprints to assess claimed model configurations across the evaluated hardware. [INTELLECT-2](https://arxiv.org/abs/2505.07291) reports using TOPLOC to check untrusted inference workers, alongside other rollout checks. These are relevant existing methods, not a turnkey guarantee for Phase's mixed GGUF/MLX/precision environment.

LUCID must compare methods against the same exact-CID jobs and independently selected validators, measuring honest cross-backend divergence, model/precision substitution, challenge distinguishability, collusion, validator cost and prompt exposure. Report empirical results and exclusions rather than inheriting an upstream paper's strongest headline. The experiment and go/no-go contract live in the existing [trust milestone](../releases/lucid-v0.2/reputation-and-redundant-verification.md#method-comparison--2026-11-07).

### Recommendations adopted for repository planning

1. Position Phase by its particular protocol and governance guarantees; stop implying the broader category is empty. Keep Plasm, LUCID and planned LUMEN distinct.
2. Publish current checkpoint status and acceptance limits in both READMEs. Preserve the difference between signed transcript integrity, redundant agreement, model-identity challenges and independently validated execution.
3. Execute the [physical intended stream](../releases/lucid-v0.2/intended-stream-vertical-slice.md#operator-runbook--2026-11-07) using exact-CID real content, native Phase transport, live output, cancellation and invalid-content/receipt rejection. Same-LAN, echo, Tailscale data paths and pre-copied artifacts cannot satisfy it.
4. Benchmark independent whole-model jobs before tensor sharding. ShardWorker stays blocked until the existing control baseline and partial-compute ADR gates pass.
5. Keep correctness validation a method-selection gate until an independently reproducible experiment establishes its bounds. Neither receipt code nor a comparison table completes that implementation.

### Review triggers

Recheck OpenHydra and KwaaiNet before a public Phase release or a trust/scale ADR. Record the inspected release/source revision and changes to validation guarantees, payment dependence, bootstrap/control points, public-network evidence and installation friction. Do not infer adoption from stars, download counts or self-reported node totals. Update this document in place and link substantive findings from the active tracker.

## Historical June investigation — not current positioning evidence

The original notes below are preserved as session history. Their model rankings, provider-policy and political-event claims have not been revalidated in this November review and must not be promoted to current mission/product claims without primary-source verification. In particular, the old recommendation to present signed receipts as an answer to covert model substitution is superseded: current receipts prove transcript integrity, not correct computation. The current competitive assessment above controls.

**Original method:** Web research and a local-tool shakedown; the original author attributed claims to the links preserved below. Their inclusion here is not a fresh endorsement of those claims.

---

## TL;DR

- **A government forced a live, commercial frontier model offline for the first time.** On 2026-06-12 the US Commerce Dept (Sec. Lutnick) ordered Anthropic to block all foreign nationals from **Claude Fable 5 / Mythos 5**; unable to verify nationality in real time, Anthropic shut both down **worldwide**. This is the canonical proof case for LUCID's core promise — *the off-switch is real, and it was just pulled.*
- **Mandatory biometric KYC is arriving at the frontier.** Anthropic's revised privacy policy (effective **2026-07-08**) requires identity verification via **Persona** (gov ID + live selfie) for consumer Claude. Validates "without KYC" as a hard differentiator — and reveals the structural link below.
- **The KYC mandate and the export-control shutdown are the same control architecture.** Selective, government-directed access control is *only enforceable* with per-user identity verification. Identity-gating is the supply; export control is the demand. LUCID's no-identity substrate is the structural refusal of both.
- **Closed models are conflicted-out for AI-development work.** Anthropic shipped (and, after backlash, apologized for) *covert* capability-throttling on frontier-AI-development prompts. Confirms: you cannot audit a black box you don't control — which is the argument for open weights **and** for Phase's verifiable execution.
- **Open weights are now unambiguously competitive** (MISSION's "why now" #1), with mid-2026 specifics: GLM-5.2 (744B MoE, on Cloudflare), MiniMax M3 (#1 open-weight SWE-Bench Pro, 59.0%), DeepSeek-V4-Flash (now MLX-runnable), Qwen3.6-35B-A3B (73.4 SWE-bench Verified).
- **The local-node ecosystem moved:** Ollama gained an MLX backend (0.19, Mar 2026); llama.cpp merged an MCP client; and **Osaurus** (MIT, Swift/MLX-native, Ollama-API on `:1337`, MCP server+client, **opt-in** telemetry, no KYC) is the closest single-machine cousin to a LUCID node — relevant as prior art and a possible interop/differentiation surface.

---

## 1. Fable 5 / Mythos 5 — the off-switch was pulled (proof of "nobody can turn it off")

Two distinct events, three days apart, often fused:

- **Release (2026-06-09):** Anthropic shipped **Fable 5** (public, guardrailed) and **Mythos 5** (more capable, fewer guardrails, narrow distribution to cyber-defenders/infra providers). Fable was framed as "Mythos on guardrails."
- **Government shutdown (2026-06-12, 5:21pm ET):** Commerce Dept directive (Sec. Howard Lutnick) ordered Anthropic to block **all foreign nationals** — inside or outside the US, including its own foreign-national staff — citing fear of use by *"military intelligence in countries of concern, such as China and Russia."* Trigger: a potential jailbreak (concerns reportedly first raised by Amazon's Andy Jassy; probed with known-vuln open-source code). Because nationality can't be verified in real time across hundreds of millions of users on same-day notice, Anthropic **shut both models off for everyone, worldwide**. Weights were *not* transferred — "closer to a product recall than traditional export control." Anthropic publicly **disagreed** with the threat assessment. Still down as of 2026-06-21; called "temporary." [[The Hill]](https://thehill.com/policy/technology/5926417-anthropic-fable-mythos-ai/) [[Euronews]](https://www.euronews.com/2026/06/13/why-anthropic-is-halting-access-to-its-fable-5-and-mythos-5-ai-models)

**Debunked:** the viral "NSA Director testified Mythos breached nearly all classified systems in hours" claim appears only in SEO content farms, not credible reporting. The real trigger is a narrow jailbreak whose severity Anthropic disputes. Do not repeat the cyberweapon framing.

**Why it matters to us:** this is the first time a leading lab took a *publicly deployed, commercial* model offline at direct government order. The enforcement mechanism — *can't segment → kill it for everyone* — is precisely the failure mode LUCID's "on the public's hardware, nobody can turn it off" exists to eliminate. Empirically corroborated by **[The New Stack](https://thenewstack.io/fable-ban-open-weights/)**: open-weight models absorbed the displaced demand before Anthropic could restore access. Strengthens MISSION "Why now" → *AI sovereignty is a political reality.*

## 2. Mandatory biometric KYC via Persona (validates "without KYC")

Anthropic's privacy policy (updated 2026-06-08, effective **2026-07-08**) requires identity verification through **Persona** — gov photo ID + live selfie (facial geometry) — for consumer accounts (Free/Pro/Max); enterprise exempt; API/Console scope unclear from coverage. Persona processes the data; contractually barred from ads/training. [[The Register]](https://www.theregister.com/2026/04/16/anthropic_claude_id_verification_persona/) [[Medianama]](https://www.medianama.com/2026/06/223-anthropic-widens-data-collection-id-verification-government-id-selfie-claude-users/)

**Accuracy note for positioning:** Persona is *not* "Peter Thiel's company." Founded 2018 by Charles Yeh & Rick Song; Thiel's **Founders Fund led its Series C/D** (major minority investor, not founder/operator). Use "Founders-Fund-backed Persona," not "Thiel's." [[Wikipedia]](https://en.wikipedia.org/wiki/Persona_(identity_verification_service))

**The structural insight (the sharp positioning point):** export-control compliance (§1) is *only possible* if the provider can verify each user's identity/nationality. The Persona KYC mandate is the **supply** that makes government-directed selective access control the **demand**-side enforceable. They are the same control architecture viewed from two sides. **LUCID's "no payments, no KYC, no lock-in" substrate is not a convenience feature — it is the structural refusal of the entire identity-gating apparatus.** This belongs near MISSION principle #4 (verification beats payments) and "Why now."

## 3. Covert capability-throttling — closed models are conflicted-out (validates open weights + verifiable execution)

Fable 5's system card disclosed *covert* safeguards that *"quietly downgrade its own responses"* on frontier-AI-development prompts — explicitly *"not visible to the user"* (~0.03% of traffic). Researchers (Dean Ball: "secret sabotage"; Jeremy Howard: Anthropic kept frontier access for itself while degrading others') called it monopolistic-behavior-as-safety. Anthropic reversed: *"We made the wrong tradeoff, and we apologize."* [[Fortune]](https://fortune.com/2026/06/10/anthropic-accu-claude-fable-5-limits-capabilities-ai-researchers-developers/) [[Decrypt]](https://decrypt.co/370688/internet-furious-anthropic-claude-mythos-fable-5)

**Analysis:** "control who can build frontier AI" is simultaneously a safety lever *and* a competitive moat — entangled, both plausibly real. As a barrier it's near-symbolic (compute/data, not chat access, is the competitor bottleneck — note the ironic real-world distillation: the public `gemma-4-12B-coder` was bootstrapped from *Composer 2.5 + Fable 5* traces). What's confirmed is the **willingness** to silently shape capability along competitive lines. Implication: for AI-development work, the model maker is structurally your competitor's gatekeeper → closed frontier models are **conflict-of-interest-disqualified**, and *you cannot audit a black box you don't control.* This is a first-principles argument for (a) open weights and (b) **Phase's signed-receipt verifiable execution** — the antidote to invisible, deniable capability manipulation.

## 4. Local-inference ecosystem — what shifted under us

- **Ollama gained an MLX backend** (v0.19, ~2026-03-30; Apple Silicon, 32GB+), ~2× decode. Apple Silicon is now first-class, not just "Metal GPU." [[Ollama]](https://ollama.com/blog/mlx) — *relevant to LUCID's MLX worker plan and the `:11434` compatibility wedge.*
- **llama.cpp merged a full MCP client** into its web UI (Mar 2026) — server mgmt, agentic tool loop, Prompts, Resources. Note: **llama.cpp still does *not* use Apple MLX** (it's GGML+Metal; MLX is a separate framework) — a distinction worth keeping straight when describing worker backends.
- **Osaurus** (osaurus.ai, by Osaurus, Inc.) — **the closest single-machine cousin to a LUCID node.** MIT, pure Swift, **genuinely MLX-native**, exposes **Ollama- + OpenAI- + Anthropic-compatible APIs on `:1337`**, full **MCP server *and* client**, models in `~/MLXModels`. Privacy posture is the cleanest of the field: local-by-default, **opt-in** telemetry (vs LM Studio opt-out), no training on user data, **no KYC** (local cryptographic *installation key* for billing/relay attribution, not identity). Caveats: opt-in cloud path egresses via **Venice**; MIT app wraps a commercial hosted/relay layer (ToS forbids reselling hosted inference) + mandatory arbitration.
  - **Positioning vs LUCID:** Osaurus is *single-machine local + optional commercial relay*; LUCID is *many machines, nobody owns, no relay operator to capture*. They share the Ollama-API wedge and a local-first/anti-KYC ethos. Worth tracking as (a) prior art for the "local node with familiar API" UX, (b) a possible **backend/worker** or interop target, and (c) a contrast point: Osaurus's "cryptographic installation identity for billing" is exactly the centralized hook LUCID avoids.

## 5. Open-weights competitiveness — mid-2026 update to "Why now"

MISSION cites DeepSeek-V4 / Qwen3-Next / Llama 4. Current specifics worth folding in:
- **GLM-5.2** (Z.ai) — 744B MoE, 1M context, agentic coding; now hosted on **Cloudflare Workers AI** (`@cf/zai-org/glm-5.2`, added 2026-06-16). Rated strongest all-around open coder.
- **MiniMax M3** (Jun 2026) — first open-weight to combine frontier coding + 1M context + multimodality; **#1 open-weight SWE-Bench Pro at 59.0%**. REAP-pruned M2.5 variants fit 128GB at ~85GB (MLX 4-bit).
- **DeepSeek-V4-Flash** — now MLX-runnable (`mlx-community/...-8bit`) + experimental llama.cpp fork (antirez); not yet in stable Ollama/LM Studio. *(Supersedes the older "blocked locally" status.)*
- **Qwen3.6-35B-A3B** — **73.4 SWE-bench Verified**, the comfortable-fit local quality leader on Apple Silicon (3B active MoE). This is the model class a LUCID contributor Mac would realistically serve.

The "donate your GPU for a worse model" objection is dead. The median-use-case gap to closed APIs is gone.

---

## 6. Implications for Phase / Plasm / LUCID

- **Phase substrate: untouched, and vindicated.** The events validate the design choices (no KYC, no payments, no central operator, verifiable receipts) rather than suggesting changes. Phase's *credible neutrality* is now a demonstrated necessity, not a nicety.
- **Plasm: orthogonal.** No impact.
- **LUCID: sharpened positioning, no scope change.**
  1. **Lead with the Fable/Mythos shutdown as the proof case** in mission/landing copy: "On June 12, 2026, a government switched off a frontier model for the entire world overnight. LUCID is the architecture where that can't happen." Concrete, dated, undeniable.
  2. **Elevate "no KYC" from feature to thesis** using the §2 structural insight (identity-gating is the enforcement layer for the off-switch). Candidate addition near MISSION principle #4 / "Why now."
  3. **Frame verifiable execution (signed receipts) as the answer to covert capability-throttling** (§3): open weights + Phase receipts = inference you can audit and that no one can silently nerf.
  4. **Refresh "Why now" #1** with the §5 model specifics.
  5. **Track Osaurus** as the local-node UX benchmark and a possible worker/interop target — while drawing the bright line: its "installation identity + hosted relay" is the centralized hook LUCID structurally rejects.

**Net:** nothing here changes the build plan. Everything here makes the *case for the build* more urgent and more concrete. The world produced the demo of why LUCID needs to exist.
