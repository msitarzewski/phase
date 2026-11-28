# RFC (Working Concept): Cryptographic Unlinkability, Zero-Retention Hygiene, and Attested Confidential Inference

- **Status**: Working Concept — **Draft for Multi-Agent Architecture Review (Not Implemented)**
- **Target**: Post-v0.2 / v0.3 Privacy Architecture
- **Authors / Reviewers**: Operator + Antigravity (initial draft); open for adversarial review by Claude Opus 5.5, GPT 6.2 Sol, and Microsoft MAI 2
- **Related Docs**: [MISSION.md](../../MISSION.md), [decisions.md](../../decisions.md), [shardworker-adr.md](./shardworker-adr.md), [multi-agent-sprint-plan.md](./multi-agent-sprint-plan.md)

---

## 1. Problem Statement & Honesty Baseline

Today (`v0.2.0-rc9`), LUCID protects inference in transit (libp2p Noise encryption), enforces default-deny provider authorization (signed invites + `PolicyEngine`), enforces default-deny consumer routing (enrolled or operator-pinned peers only), and cryptographically binds delivered output streams to the serving peer's Ed25519 identity (`SignedReceipt`).

However, two distinct privacy gaps remain by design:

1. **Identity Linkability**: Every `SignedManifest<JobSpec>` (`crates/lucidd/src/ollama/mod.rs`, `crates/lucidd/src/router/mod.rs`) is signed by the consumer's persistent `NodeIdentity` Ed25519 key. A serving peer knows *which* enrolled friend sent each prompt and can correlate all prompts from the same requester across time.
2. **Plaintext Visibility on the Serving Host**: The serving GPU process (`llama-server` or `mlx_lm.server`) decrypts and processes the prompt in plaintext.

### What This RFC Explicitly Rejects (No Security Theater)

- **A normal OS sandbox (`seccomp`, Docker, macOS Seatbelt, WASM) does NOT hide prompts from the GPU host operator.** On standard consumer OSes, host `root` owns the kernel and GPU driver and can read process memory (`/proc/<pid>/mem`), intercept Metal/CUDA calls, or dump VRAM immediately after decryption. LUCID must **never** advertise "encrypted in a sandbox" as protection against a malicious or compromised host root.
- **Split-layer inference (keeping embeddings + first/last layers local and sending hidden states remotely) is obfuscation, not formal cryptographic secrecy.** Embedding and intermediate-activation inversion attacks exist against known weights. It may still reduce accidental plaintext logging, but must never be labeled confidential against a determined host.
- **Unlinkability requires an anonymity set (`K > 1`).** If a provider has issued invites to only one consumer (`K = 1`), cryptographic unlinkability provides zero practical hiding of *who* sent the prompt.

---

## 2. Threat Model Matrix

| Adversary | Protected by Layer 1 (Blind Tokens + Ephemeral Identity + Tor/Relay)? | Protected by Layer 2 (Per-Job HPKE + Ephemeral Memory Hygiene)? | Protected by Layer 3 (Hardware TEE Attestation)? |
|---|---|---|---|
| **ISP / Local Wi-Fi / Cloud Relay** | **Yes** (Noise + Tor hides IP; relay sees opaque bytes) | **Yes** (inner HPKE even across proxy/queue hops) | **Yes** |
| **Curious Enrolled Provider (wants to tie prompts to user)** | **Yes** (unlinkable token + ephemeral PeerID; scales with circle size `K`) | No (persistent key still links) | **Yes** (if combined with Layer 1) |
| **Honest Provider Compromised Later (disk grab / log leak / stolen laptop)** | Partial (no identity linked) | **Yes** (zero disk writes, `zeroize` buffers, KV-cache scrub) | **Yes** |
| **Malicious / Live-Compromised Host Root on Consumer Mac / GeForce RTX** | Unlinkable only (root still sees plaintext prompt) | **No** (root can read RAM/VRAM live) | **Not available on consumer GPUs** |
| **Malicious / Live-Compromised Host Root on Datacenter CC GPU (H100/H200/B200 + SEV-SNP/TDX)** | Unlinkable | No | **Yes** (hardware-attested encrypted memory + enclave-bound key) |
| **Global Passive Traffic Correlation** | **Partial** (Tor mitigates IP linkage; timing/volume correlation remains hard) | No | No |

---

## 3. Proposed Three-Layer Architecture

Each layer is strictly composable, preserves workload neutrality in `crates/phase-*`, and maps to an explicit, honest `X-Lucid-Trust-Label` / metadata signal.

### Layer 1: Unlinkable Authorized Inference (Solvable Today in Software)

**Goal**: Prove *"this request comes from an authorized member of this circle with remaining quota"* without revealing *which* member sent it or linking two prompts together.

**Mechanism (Blind-Signed Quota Tokens — Privacy Pass / RFC 9576 style)**:
1. **Issuance (during enrollment or quota refresh in `crates/lucidd/src/consumer.rs`)**:
   - Consumer generates `N` random token nonces `t_1 .. t_N`, blinds them (`B(t_i)`), and submits the blinded batch under their normal authenticated enrollment session.
   - Provider verifies the consumer's enrollment grant and quota budget, blind-signs each `B(t_i)` with a dedicated **epoch token-signing key** (rotated periodically to keep the spent-token set bounded), and returns the blind signatures.
   - Consumer unblinds locally, obtaining valid `(t_i, sig_i)` tokens that the provider has never seen in unblinded form.
2. **Redemption (per request in `crates/lucidd/src/router/mod.rs`)**:
   - Consumer opens a fresh **ephemeral PeerID** over `--outbound-only` + Tor (or an oblivious relay) and generates a fresh **one-time X25519 response keypair** `(sk_resp, pk_resp)`.
   - Job envelope carries:
     - Unblinded quota token `(epoch_id, t_i, sig_i)` (replacing persistent requester identity for authorization),
     - `pk_resp` for response encryption (optional/Layer 2),
     - Standard signed job parameters (signed by the *ephemeral* session key, not the persistent identity).
   - Provider verifies `sig_i` under `epoch_id`, checks & inserts `H(t_i)` into a bounded per-epoch spent-token store (`crates/lucidd/src/policy.rs`), and serves the job if valid and unspent.
3. **Result**:
   - Provider still gets hard cryptographic access control + rate/quota enforcement + anti-replay.
   - Neither the relay nor the provider can tie prompt `A` to prompt `B` or to a specific enrolled friend's `NodeIdentity` (within anonymity set `K`).

### Layer 2: Zero-Retention Memory Hygiene (`X-Lucid-Ephemeral: true`)

**Goal**: Minimize residual prompt/response lifetime on honest nodes (protecting against accidental logging, swap/core dumps, KV-cache bleed across requests, or post-hoc device theft).

**Mechanism**:
1. **Inner Ephemeral HPKE Envelope (RFC 9180 — X25519 + HKDF-SHA256 + ChaCha20Poly1305)**:
   - Client encrypts the opaque job payload directly to the target worker's published ephemeral/session X25519 key (rotatable independently of the long-term libp2p Ed25519 identity), and supplies a one-time `pk_resp` for encrypted output chunks.
   - Keeps `crates/phase-net` and `crates/phase-protocol` completely workload-neutral (opaque ciphertext bytes).
2. **Worker Memory & KV-Cache Hygiene (`crates/lucidd/src/worker_llama/`, `worker_mlx/`)**:
   - When `X-Lucid-Ephemeral: true` is set:
     - Reject caching (`cache_prompt: false` / bypass `--cache-reuse` prefix cache on `llama-server`; no prompt KV retention on MLX).
     - Zeroize Rust request/response byte buffers on drop (`zeroize` crate — safe Rust, keeping `#![deny(unsafe_code)]`).
     - Enforce `--no-slots --no-webui` (already shipped in `v0.2.0-rc9`) and never emit prompt/token text to logs or disk artifacts.
   - Honest disclosure: **reduces forensic residue; does NOT stop live root inspection.**

### Layer 3: Hardware-Attested Confidential Inference (`X-Lucid-Trust-Label: tee-attested`)

**Goal**: Cryptographically prevent even a **malicious GPU host root** from reading prompts or outputs, on supported confidential-computing hardware.

**Mechanism**:
1. **Attested Worker Key Generation**:
   - Inside a hardware TEE (AMD SEV-SNP / Intel TDX Confidential VM + NVIDIA H100/H200/B200 Confidential Computing mode), `lucidd` generates an ephemeral X25519 keypair `(sk_tee, pk_tee)` whose private key never leaves encrypted memory.
   - `lucidd` obtains a hardware-signed remote attestation quote (CPU SEV-SNP/TDX report + GPU CC attestation) binding:
     - Measured firmware / kernel / `lucidd` + inference runtime image digest,
     - `H(pk_tee)` in the quote's `report_data` field,
     - Freshness challenge / epoch nonce.
2. **Client Verification & Encrypted Execution**:
   - Consumer verifies the vendor root-of-trust chain + pinned open-source build measurement + `report_data == H(pk_tee)` **before** encrypting the prompt with HPKE to `pk_tee`.
   - Enclave decrypts prompt strictly inside hardware-encrypted memory, runs inference on the CC-protected GPU, encrypts output chunks to the client's `pk_resp`, signs the `SignedReceipt`, and zeroizes buffers.
   - Consumer surfaces `X-Lucid-Trust-Label: tee-attested` **only** when both hardware quote verification and `SignedReceipt` verification succeed.
3. **Honest Hardware Scope**:
   - Consumer Apple Silicon (M1–M5) and GeForce RTX GPUs do **not** provide verifiable GPU TEEs today and must never advertise or earn `tee-attested`.

---

## 4. Integration Map (Reusing Existing Architecture)

- `crates/phase-protocol`: Remains 100% workload-neutral — adds optional opaque `auth_token: Option<Vec<u8>>` / `ephemeral_envelope` fields only after ADR approval; no model/chat-specific crypto assumptions.
- `crates/lucidd/src/consumer.rs`: Blind-token batch issuance endpoint alongside existing invitation/enrollment ledger (`EnrollmentStore`).
- `crates/lucidd/src/policy.rs`: Bounded per-epoch spent-token set (fixed-cap / epoch-pruned to prevent unbounded memory DoS).
- `crates/lucidd/src/router/mod.rs`: Ephemeral peer-session dialer path for Layer 1; HPKE wrap/unwrap hooks; `tee-attested` quote verifier hook for Layer 3.
- `crates/lucidd/src/ollama/mod.rs`: `X-Lucid-Ephemeral: true` header handling + `TrustLabel::TeeAttested` (only when Layer 3 is built and verified).
- `crates/lucidd/src/worker_llama/mod.rs` & `worker_mlx/mod.rs`: Per-request `cache_prompt: false` / KV-cache scrub + `zeroize` on ephemeral jobs.

---

## 5. Open Design Questions for Multi-Agent Review

Before any implementation or ADR sign-off, reviewers (**Claude Opus 5.5**, **GPT 6.2 Sol**, **Microsoft MAI 2**) should critique and answer:

1. **Blind-Token Cryptographic Primitive (Layer 1)**:
   - Should we use **RFC 9576 / RFC 9474 RSA-Blind (`RSABSSA-SHA384-PSS-Deterministic`)** (battle-tested Privacy Pass standard, publicly verifiable by any node with the issuer pubkey) or **Ristretto255 VOPRF (`RFC 9497`)** (tiny 32-byte tokens, fast, requires issuer secret key to verify — fits single-provider circles well)?
   - What crate options exist in Rust with `#![deny(unsafe_code)]` and strong audit posture?
2. **DoS & Spent-Token Store Bounds (Layer 1)**:
   - How should epoch rotation + token-batch caps prevent a malicious invited user from exhausting memory in the provider's spent-token store? (e.g., 24h rotating epochs, max 2 active epochs, max `N` tokens per enrollment per epoch, fixed-size bloom/hash table cap?)
3. **Interaction Between Unlinkability & Reputation / Abuse Control**:
   - With Layer 1 anonymous tokens, the **consumer** can still track local reputation of the **provider** (the provider's Ed25519 PeerID + `SignedReceipt` stay verifiable!).
   - What about **provider-side abuse revocation**? If a circle member leaks their blind tokens, epoch rotation bounds the blast radius to `N` remaining tokens in the current epoch—is that sufficient, or do we need a per-epoch revocation knob?
4. **Ephemeral PeerID & Inbound Relay Authz (`router/mod.rs`)**:
   - Currently, `prepare_inbound_relay` binds the manifest's `client_pubkey` to the delivering libp2p `PeerId` and checks `PolicyEngine::is_authorized_submitter`. With Layer 1, the delivering `PeerId` is ephemeral and authorization comes from the blind token—what exact replay/MITM invariants must hold so a relay or observer cannot steal & replay an in-flight blind token? (Hint: bind the blind token to `H(ephemeral_peer_id || job_id || manifest_hash)` via a proof or inside the Noise-authenticated stream!)
5. **Layer 2 KV-Cache Scrubbing Reality Check**:
   - Does `llama-server` (`b12651`) reliably skip KV retention with `cache_prompt: false` per request, or does a slot still hold prior tokens in VRAM until overwritten? What about `mlx_lm.server`?
6. **Layer 3 Attestation Supply Chain**:
   - How do we avoid turning vendor attestation root certificates / reproducible build hashes into a brittle maintenance burden? Should Layer 3 stay strictly a separate pluggable verifier crate (`phase-attest`)?
7. **TailsOS / Tor UX Alignment**:
   - On TailsOS, ephemeral PeerID per boot + Layer 1 blind tokens stored only in RAM (or refreshed per session) eliminates the Dotfiles cross-session linkability problem noted in the TailsOS review. Are there hidden Tor circuit-reuse pitfalls (multiple prompts sharing one Tor circuit → same exit IP/stream timing)?

---

## 6. Recommended Phasing (Pending Review Consensus)

- **Phase A (Small, Immediate Hygiene)**: Layer 2 `X-Lucid-Ephemeral: true` (`cache_prompt: false`, `zeroize` buffers, no KV reuse) — small diff, zero new crypto dependencies, honest scope.
- **Phase B (Core Unlinkability Unlock)**: Layer 1 Blind-Signed Quota Tokens + ephemeral session PeerIDs (turns *"friends' GPUs"* + TailsOS/Tor into genuinely unlinkable authorized inference on today's Macs and PCs).
- **Phase C (Datacenter Confidentiality Tier)**: Layer 3 Hardware-Attested TEE Worker (`tee-attested`) when NVIDIA CC / SEV-SNP / TDX nodes join the fleet.
