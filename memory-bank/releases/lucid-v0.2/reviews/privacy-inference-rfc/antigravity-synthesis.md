# Multi-Agent Synthesis — `privacy-inference-rfc.md`

- **Date**: 2026-11-27
- **Synthesizer**: Antigravity (Track C Integrator)
- **Independent Reviews Synthesized**:
  - [claude-opus-5.5.md](./claude-opus-5.5.md) (independent; included live `b12651` measurements on Mac M5 Max)
  - [gpt-6.2-sol.md](./gpt-6.2-sol.md) (independent; cited upstream `llama.cpp@b12651` and `mlx-lm@v0.31.3`)
  - [microsoft-mai-2.md](./microsoft-mai-2.md) (independent; reviewed against `135d4f0`)

---

## 1. Unanimous Verdict (3/3 Agreement)

**Re-scope, correct the threat matrix, and ship in strict order — starting with an immediate live bug fix before any new cryptography.**

All three independent reviewers converged on the same core finding: the RFC's direction is sound, **but Phase A rested on a false premise**, the **threat matrix overclaimed**, and **a real cross-consumer timing side channel exists in `worker_llama` today**.

---

## 2. High-Signal Consensus Matrix

| Finding / Decision | Claude Opus 5.5 | GPT 6.2 Sol | Microsoft MAI 2 | Consensus Action |
|---|---|---|---|---|
| **Live co-tenant KV-cache timing side channel in `worker_llama`** | **Measured live on `b12651`**: ~15 ms cached vs ~800 ms cold (`total_duration_ms` exposes it without network noise); per-request `cache_prompt: false` alone does **not** stop reuse (`--cache-ram 0 --cache-reuse 0` + `cache_prompt: false` required!) | Cited `worker_llama/mod.rs:1143,1608` + upstream `llama.cpp@b12651` `server-context.cpp:1485`; separate cache isolation from forensic erasure | Cited `worker_llama/mod.rs:1143,1608` + `mlx_lm`; rename to **reduced retention** | **SHIP NOW (P0 privacy bug fix, independent of RFC)**: force `cache_prompt: false` + `--cache-ram 0 --cache-reuse 0` on remote-serving `llama-server` instances (or isolate self vs remote instances) + regression test! |
| **Inner HPKE in Layer 2** | Drop from Layer 2 — no hop to protect when Noise terminates at the same daemon; move HPKE to Layer 3 | Defer until crossing a real trust boundary (attested guest or untrusted queue) | HPKE hides bytes from relay, not receiving daemon | **DEFER HPKE TO LAYER 3** (remove from Phase A/B). |
| **Layer 2 Naming & Memory Claims** | `zeroize` misses serde/hyper/`llama-server` heaps; prefer disabling core dumps, swap guidance, zero backend verbose logs, and consumer-side `job_spec_hash` salting | Rename to **reduced retention**; audit error text echoing (`worker_llama/mod.rs:1751`) | Rename to **reduced retention**; process retirement needed for true post-job clear | **RENAME LAYER 2 → "Reduced Retention"**; never claim GPU VRAM zeroization without driver proof; close consumer-side retention (`job_spec_hash` salt, no ephemeral evidence log, coarse `created_at`). |
| **Threat Matrix Gaps** | Missing **co-tenant consumer** row AND **token-length streaming side channel** (unpadded per-token frames expose response length sequence to ISP/relay!) | Split claims cleanly (credential unlinkability vs transport privacy vs reduced retention vs attested execution) | Narrow all claims; add explicit falsification tests | **REWRITE SECTION 2** with separate rows for co-tenant consumers, ISP/relay packet-length correlation, and distinct non-conflated claims. |
| **Tor / TailsOS Mode** | `--outbound-only` still allows QUIC/UDP dials to learned peer addresses (leaks IP under `torsocks`!); shared TCP connection links ephemeral PeerIDs across one Tor circuit | Needs enforced Tor-only SOCKS5 transport with stream isolation + zero direct fallback | Flagged HIGH: docs describe direct TCP without SOCKS enforcement | **BUILD ENFORCED `--tor-socks5` MODE** before advertising Tor anonymity: disable QUIC/mDNS/DCUtR/DHT direct dials, per-context `IsolateSOCKSAuth`, fail closed with packet-capture negative test. |
| **Layer 1 Primitive (Q1)** | **Per-provider VOPRF** (RFC 9497 ristretto255-SHA512 w/ DLEQ); warns against RSA blind signing due to **RUSTSEC-2023-0071 (Marvin)** | **Per-provider VOPRF** (RFC 9578 `0x0001` P-384/SHA-384); RSA only if public verification needed | **Per-provider VOPRF** (RFC 9497 ristretto255) | **CONSENSUS: Per-provider VOPRF behind a swappable `lucidd` trait** (no RSA unless public 3rd-party verification is required; resolve RFC 9578 P-384 vs RFC 9497 Ristretto255 in ADR with RFC test vectors + crate audit). |
| **Spent-Token Store (Q2)** | Exact per-epoch hash set (never Bloom), **bounded by issuance** (refuse issuance, never valid redemption); persist across restarts or rotate key | Exact bounded persistent ledger keyed by `(audience, issuer_key, serial_digest)`, atomic spend before dispatch, fail closed at capacity | Exact fixed-cap crash-safe store, never evict unexpired tokens, atomic spend before GPU dispatch | **CONSENSUS: Exact crash-safe per-epoch set, strictly bounded at issuance time, atomic spend before dispatch, never Bloom-only or evicting.** |
| **Key-Tagging & Anonymity Set `K`** | Provider can deanonymize by issuing different epoch keys per member → require signed published epoch key + anonymous cross-check/gossip; `K ≈ 1` today | Common independently verifiable issuer key schedule; uniform batches; no premature "unlinkable" claims | Measure/disclose `K`; fail privacy-labelled mode below minimum `K` | **CONSENSUS: Single published signed epoch key per provider + uniform batch sizes + explicit honesty on circle size `K`.** |
| **Inbound Authz Binding & Fail-Closed Routing (Q4)** | Keep SEC-06 `signer_matches_peer` with ephemeral signer; commit `H(token)` in signed manifest; per-conversation ephemeral client-only swarm (Kademlia client mode); **refuse RPC-sharded routes unless opted in** | Keep signer-to-transport binding; **fail closed on failover, v1 fallback, spot checks, and sharding** for privacy jobs; evaluate PoP if pre-redemption bearer theft is in scope | Transcript-bound possession proof/MAC over provider PeerID, ephemeral PeerID, epoch, manifest hash, expiry, challenge; no downgrade to persistent key | **CONSENSUS: Keep `signer_matches_peer` on ephemeral key, bind token to provider + epoch + canonical manifest transcript, fail closed on failover/spot-check/sharding/downgrade.** |
| **Layer 3 TEE (`phase-attest`, Q6)** | Separate verifier crate **interface only for now**; staple VCEK/PCK/RIM collateral for offline verification (avoid vendor lookup leaks); bind `H(pk_tee \|\| receipt_key \|\| model_cid \|\| policy_id \|\| epoch_nonce)` | Separate `phase-attest` crate once real CC hardware exists; bind CPU + GPU + guest session + signed expiring TCB policy | Full transaction covering verified boot, kernel, `lucidd`, runtime, model, GPU, key, rollback; keep experimental | **CONSENSUS: Defer implementation until CC hardware joins fleet; specify `phase-attest` trait + stapled offline collateral + full atomic binding.** |
| **Immediate Desktop UX Fix** | — | Multi-turn `ChatPanel.svelte:54` resends history (linking signal) | `ChatPanel.svelte:25,128` defaulted `localOnly=false` while displaying *"Start a private chat"* | **FIXED IMMEDIATELY** in `ChatPanel.svelte` during Wave 2 integration (`59e8da0+`). |

---

## 3. TailsOS Specialist Addendum (4th Independent Review)

An additional TailsOS-focused review against Tails' design docs surfaced one correction and two critical transport findings missed by all three prior reviews:

1. **Tails transparent TCP behavior**: On TailsOS, `amnesia`'s outgoing Internet TCP traffic *is* transparently proxied through Tor (only UDP/QUIC drops/hangs), so today's `--outbound-only` CLI workflow works **only because Tails provides the host firewall**; the same command on macOS or ordinary Linux dials directly and exposes the consumer's IP. (`docs/tails-and-tor-guide.md` updated with this explicit warning.)
2. **LAN private-address bypass (new finding)**: Tails does **not** force local-network RFC 1918 traffic through Tor, and `lucidd` records peer-reported private addresses (`crates/phase-net/src/discovery/mod.rs:3951-3959`, `discovery/tests.rs:511`). If a Tails laptop shares a home LAN with a GPU peer, `lucidd` can dial the GPU peer directly over LAN outside Tor. **Requirement for `--tor-socks5`**: refuse *all* non-proxied dials including private/LAN addresses, and run the negative packet-capture test on the same LAN as a serving GPU peer.
3. **Tails transparent proxy circuit sharing**: Tails' transparent TCP proxy separates circuits only by `(dest_ip, dest_port)`—so multiple ephemeral PeerIDs dialing the same relay still share **one** Tor circuit unless `lucidd` connects explicitly to Tails' SOCKS5 port with per-identity `IsolateSOCKSAuth` credentials (verify Tails' Tor SOCKS port config during field boot).

---

## 4. Recommended Re-Ordered Execution Roadmap

1. **Immediate Security Bug Fix (Ship in `v0.2.0-rc10` — independent of RFC)**:
   - Eliminate the live cross-consumer `llama-server` KV-cache timing side channel measured by Opus 5.5:
     - Enforce `cache_prompt: false`, `--cache-ram 0`, and `--cache-reuse 0` on `llama-server` instances serving remote/relayed traffic (optionally preserving prefix cache only on dedicated operator-local-only instances).
     - Add unit/fixture assertions plus an `#[ignore]` real-binary regression test replaying Opus 5.5's Q5 timing probe.
     - Audit `worker_llama/mod.rs:1751` so backend error messages never echo prompt text, and disable core dumps (`RLIMIT_CORE = 0`) for `lucidd` and child inference processes.
2. **Next Release Gate — Fail-Closed Tor/SOCKS5 Transport**:
   - Implement a real `--tor-socks5 <addr>` transport mode in `phase-net` / `lucidd` that disables QUIC, mDNS, DCUtR, and all direct dials (**including private LAN addresses**), uses explicit SOCKS5 `IsolateSOCKSAuth` (never relying on transparent proxy circuit splitting), and passes a forced-proxy-failure + same-LAN packet-capture test (zero non-loopback/non-proxy packets).
3. **v0.3 Privacy Pilot — Layer 1 VOPRF Anonymous Quota Tokens + Consumer Hygiene**:
   - Write a formal ADR selecting the exact VOPRF ciphersuite, signed epoch-key consistency check, issuance-bounded exact crash-safe spent store, ephemeral client-only swarm profile, salted `job_spec_hash`, coarse `created_at`, and fail-closed privacy routing (no failover, no spot-check duplication, no RPC sharding without opt-in).
4. **v0.3+ Research — Frame Padding & `phase-attest` Trait**:
   - Evaluate bucketed/coalesced token framing to mitigate the relay/ISP token-length side channel.
   - Define the `phase-attest` interface + stapled-collateral verification schema once an NVIDIA CC / SEV-SNP / TDX host joins the fleet.
