# Review: privacy-inference-rfc.md — Claude Opus 5.5
**Date**: 2026-11-27 | **Commit Reviewed**: `59e8da0` (RFC reviewed as the untracked working-tree draft), plus Wave 2 branch `90e9789` (`/phase/shard-rpc/1.0.0`) where it changes the privacy surface

> Independence note: I did not read the other reviewers' files before writing this. Every `file:line` below is at `59e8da0` unless marked otherwise. "Measured" means I ran the pinned b12651 binaries on the Mac M5 Max role machine, on loopback, outside the repo.

## Verdict

**Split into phases, and reorder them. Phase A as written rests on a false premise.**

- **Layer 1 (blind quota tokens).** Its shape is right: per-provider tokens, issuer == verifier, a bounded spent set. But the RFC skips the attacks that actually break unlinkability in practice: issuer key tagging, timing between issuance and redemption, content linkability, and identity leaks at the transport layer. At today's circle sizes it buys almost nothing (see K below).
- **Layer 2 (memory hygiene).** As written it would not do what it promises.
  - Today's worker *enables* cross-tenant KV reuse.
  - On b12651, a per-request `cache_prompt: false` does **not** stop a later request from reusing that prompt's KV. Measured; see Q5.
  - The real fix is a small, high-value change that should ship independently of this RFC as a privacy bug fix.
- **Layer 3 (TEE).** A separate product with a heavy supply-chain and hardware dependency, and no node in the fleet can run it. Keep it as research with an interface stub only. Move HPKE here: today there is no hop for HPKE to protect.

## What Breaks Against Current Codebase (cite file:line)

### 1. Phase A's premise is wrong: the worker deliberately shares KV across tenants today

- Every completion is sent with a hard-coded `cache_prompt: true` (`crates/lucidd/src/worker_llama/mod.rs:1608`). Clients are explicitly blocked from overriding it (`:1633`).
- `llama-server` is spawned with `--cache-reuse 256` (`:1147`).
- b12651 defaults add a **host-RAM prompt cache** (`--cache-ram`, default 8192 MiB), cache-idle-slots, and slot selection by prompt similarity.
- A single `llama-server` per model serves every caller: the operator's own traffic and every enrolled consumer's relayed jobs alike.

The result is a **co-tenant confirmation channel that is live now.** Any enrolled consumer of a provider can test whether another consumer recently submitted a given prompt, or a longer prompt extending it, by timing their own request. The measured gap is roughly 15 ms for a cache hit against roughly 800 ms cold for 2,150 tokens on the M5 Max role machine. That is far above network jitter, and the receipt's worker-attested `total_duration_ms` (`crates/phase-protocol/src/job_spec.rs:337`) reports it without network noise at all.

MLX is already correct here: `--prompt-cache-size 0` and a decode concurrency of 1 (`crates/lucidd/src/worker_mlx/mod.rs:1328-1329`).

The RFC's threat matrix has **no "other consumers of the same provider" row**. That row is the one this channel lives in.

### 2. Layer 1 cannot use the current inbound authorization path

`prepare_inbound_relay` (`crates/lucidd/src/router/mod.rs:2214`) authorizes a job only if both hold:
- the signer is allowlisted or enrolled, via `is_authorized_submitter` (`:2235`, backed by `policy.rs:357` and `consumer.rs:167`);
- the signer is the delivering PeerId, via `signer_matches_peer` (`:2236-2239`).

With ephemeral identities, the first check fails by construction.

The replay cache has two problems for this design:
- It is keyed by `(signer_pubkey, manifest_hash)` (`:2163-2166`).
- It is one global map capped at 65,536 entries. When full, it **refuses all new work for everyone** (`:72`, `:2173-2175`). Manifests can live up to 20 minutes: a 5-minute future skew plus a 15-minute TTL (`crates/phase-manifest/src/manifest.rs:24,27`).

The spent-token store must not copy this failure mode (see Q2).

### 3. "Ephemeral PeerID" is a transport profile, not a manifest field

- The libp2p identity is the Noise static key, and `Discovery::new` builds one swarm per process. A fresh PeerID per job therefore means a fresh swarm: a new TCP connection, a new Noise and yamux handshake, and a new relay circuit, all over Tor. That costs seconds.
- Kademlia is forced into server mode unconditionally (`crates/phase-net/src/discovery/mod.rs:896`). An ephemeral swarm would join other peers' routing tables.
- `identify` advertises listen addresses and `phase-net/<version>` (`:975`).

Layer 1 therefore needs an explicit **client-only ephemeral profile**:
- Kademlia in client mode;
- no capability advertisement and no rendezvous;
- no identify listen addresses;
- dials only to the pinned relay and circuit addresses.

### 4. Linkability channels the RFC does not list

- **Content.**
  - Multi-turn chat resends the full history, so turns are linkable by content regardless of tokens.
  - `resume_from` and `resumption` are explicit cross-request linkage handles (`job_spec.rs:142,306`).
  - System prompt, model, sampling parameters, language and time of day all partition the anonymity set.
- **Clock fingerprint.** `created_at` is a full-precision client timestamp, and the verifier accepts up to 5 minutes of skew (`manifest.rs:24,117`). The consumer's clock offset is a stable fingerprint across ephemeral identities.
- **Unsalted prompt hashes at rest.**
  - The signed receipt carries `job_spec_hash` (`job_spec.rs:285`), an unsalted hash over the full job spec, prompt included.
  - The consumer persists `remote_peer_id`, `job_spec_hash` and `observed_at_unix_ms` in its evidence log (`crates/lucidd/src/reputation.rs:135-141`).
  - Anyone holding a seized consumer disk or leaked receipts can run an offline confirmation attack ("did this user send prompt X?").
  - Layer 2 only covers the provider. It says nothing about retention on the consumer side.
- **Redundant verification.** It sends byte-identical deterministic jobs to two providers (`router/mod.rs:1561-1563`), so they can correlate. It is off by default, but must be disabled for unlinkable jobs.

### 5. The Tor workflow is not enforced by the transport

- `docs/tails-and-tor-guide.md` relies on `--outbound-only`. That flag disables listeners, mDNS, DCUtR and AutoNAT (`discovery/mod.rs:912,964,980`), but:
  - QUIC is always compiled in (`:908`);
  - direct and `/quic-v1` addresses learned through identify, the DHT and rendezvous are still added to Kademlia and the swarm (`:3186-3193`), so later dials can use them.
- `lucidd` has no SOCKS or Tor transport, and the guide never says how TCP gets onto Tor.
- On Tails, the firewall fails closed, so dials just hang. Under `torsocks` on an ordinary OS, a UDP/QUIC dial **bypasses Tor and reveals the real IP**.

### 6. The matrix overclaims "ISP / Wi-Fi / Cloud Relay: Yes"

- The worker emits one `OutputChunk` per SSE token (`worker_llama/mod.rs:1921-1930`), and the relay sends one frame per event (`router/mod.rs:2480-2486`).
- Noise and yamux add constant overhead and no padding.
- An ISP, the Wi-Fi operator, or any relay therefore sees each streamed response's **token-length sequence**. That is the known "remote keylogging" side channel against streamed LLM output, and it lets an observer reconstruct responses.
- Content is protected; response length and token rhythm are not.

### 7. Layer 2's HPKE has no hop to protect today

- libp2p circuit relay v2 keeps Noise end-to-end, and there is no store-and-forward queue anywhere in the system.
- An inner HPKE envelope therefore adds nothing for Layer 2.
- It becomes meaningful only in Layer 3, as the binding to an attested enclave key. Specify it there.

### 8. RPC layer offload widens the set of hosts that see plaintext (Wave 2, `90e9789`)

- `trusted-circle-sharded` workers ship prompt-derived activations to every RPC peer, and each of them is effectively another observer of the prompt. The RFC correctly calls split-layer execution obfuscation, not confidentiality.
- What the RFC does not say: unlinkable or ephemeral jobs must either refuse RPC-backed routes or disclose the larger host set. The trust label already exists to hang that policy on.
- I tested whether `ggml-rpc-server -c` on b12651 writes prompt activations to its disk cache. Across two distinct 2,145-token prompts, it wrote no new files: only weight tensors from load (12 files). This is a narrow negative result for one configuration (RPC device first, `-ts 1,15`). Keep it as a regression check, not a guarantee.

## Subtle Attacks, Side Channels & Over/Under-Specification

- **Issuer key tagging (the classic Privacy Pass break).** A provider can issue each member under a different epoch key or key ID. Redemption then reveals which member it was. Layer 1 is meaningless without **key consistency**:
  - one published epoch key, signed by the provider's persistent identity;
  - clients verify issuance proofs against it (VOPRF DLEQ);
  - clients cross-check it either by fetching it anonymously through the ephemeral profile before first use, or by gossiping it within the circle.
- **The anonymity set cannot be verified.** K is not "invites issued". It is the number of members *actively redeeming at this provider in this epoch*, further partitioned by model, client version and time of day. The consumer cannot verify K at all. With today's circles (a handful of machines, largely one operator) K ≈ 1. No UI or label may claim "unlinkable" unless the user knows the circle out of band.
- **Issuance timing.** "Fetch tokens, then immediately redeem" links the two events. Issue on a fixed schedule (for example at epoch start, or prefetched in the background), and keep a buffer.
- **Quota partitioning.** Unequal per-member quotas make heavy users statistically distinguishable. Use uniform batch sizes.
- **Double spend under concurrency or restart.** Check-and-insert must be atomic, and the token must be spent *before* execution. An in-memory spent set lost on restart turns into double spend. Either persist the token hashes (they are unlinkable, so harmless) or rotate the epoch key on restart.
- **Over-specification.** It's too early to name `phase-protocol` fields (`auth_token`, `ephemeral_envelope`). A commitment to the token inside the already-signed manifest payload is enough.
- **Under-specification.** Missing pieces:
  - token format and expiry;
  - epoch clock source;
  - what happens to in-flight jobs at epoch rollover;
  - whether retries need a new token;
  - truncating `created_at`.
- **Layer 3.** Even in a TEE, the host still sees the encrypted network and PCIe traffic's sizes and timing, so the token-length channel survives. CPU TEEs have a steady record of practical attacks (for example BadRAM against SEV-SNP, and Heckler-style interrupt injection against SNP and TDX). That makes a *minimum-TCB policy* mandatory, not optional.

## Answers to Section 5 Open Design Questions (Q1–Q7)

### Q1. Blind-token primitive

**Recommendation:** a privately verifiable VOPRF (RFC 9497, ristretto255-SHA512, VOPRF mode with DLEQ proofs, matching the RFC 9578 privately verifiable token type).
- The issuer, the verifier and the provider are the same party: each provider issues for itself.
- Bind tokens to the provider at issuance (RFC 9577-style `redemption_context = H(provider PeerId || epoch)`), so a token is worthless anywhere else.
- Publicly verifiable RSA-Blind (RFC 9474 / 9578, `RSABSSA-SHA384-PSS-Deterministic`) is only needed if a third party must verify. Avoid that: a circle-wide key forces a *distributed* spent set, which is a much harder problem.

**Rust options:**

| Crate | What it is | Caveat |
|---|---|---|
| `voprf` | RFC 9497 with a ristretto255 ciphersuite | I found no public audit report |
| `privacypass` | RFC 9578 token types | Pre-release (0.2.0-pre.0) |
| `blind-rsa-signatures` | RFC 9474 | See below |

RSA blind *signing* is a private-key operation on attacker-chosen input, the worst case for timing leaks. The RustCrypto `rsa` crate carries RUSTSEC-2023-0071 (Marvin), with no patched version listed. Confirm the bignum backend before choosing RSA.

`#![deny(unsafe_code)]` applies only to our own crates. The meaningful bar for a dependency is constant-time arithmetic, published RFC test vectors passing in CI, pinned versions, and an audit trail. Put the primitive behind a small lucidd-side trait so it can be swapped.

### Q2. DoS and spent-token store bounds

**Bound the spent set by issuance, never by redemption.** Issuance is authenticated and quota-limited, so |spent| ≤ Σ(per-member epoch quota) × (active epochs). A member cannot grow it past their own quota.

- Epochs of 24 hours, with at most 2 active (current plus one grace epoch).
- Uniform per-member batches.
- A global issuance budget per epoch, enforced **at signing time**.
- An exact per-epoch hash set, dropped wholesale at expiry. Not a Bloom filter: false positives reject valid tokens, which is a DoS on honest members.
- Example size: 50 members × 1,000 tokens × 2 epochs × about 64 bytes ≈ 6.4 MB.
- When the budget runs out, **refuse issuance, never redemption** of already-issued valid tokens. That is the opposite of today's replay cache (`router/mod.rs:2173-2175`).

### Q3. Unlinkability vs reputation and revocation

- Consumer-to-provider reputation is unaffected: receipts stay verifiable.
- Provider-to-consumer reputation and abuse attribution are **impossible by design**. Say so plainly.
- Leaked tokens are usable until the epoch ends, so the blast radius is that member's remaining quota.
- Revocation that stays compatible with unlinkability:
  - **Per member:** stop issuing to them, effective next epoch.
  - **Emergency:** rotate the epoch key early. This is global: every member re-fetches tokens.
- If rate-limiting without one-shot token state matters, evaluate the IETF Anonymous Rate-limited Credentials draft (ARC; a draft, not a standard).
- Keep *identified* mode for high-cost jobs if abuse is a concern.

### Q4. Ephemeral PeerID and relay authorization binding

No extra proof is needed. These invariants suffice:
1. Tokens are sent only inside a Noise session to the provider's self-certifying PeerId. Circuit relay is end-to-end Noise, so relays see only ciphertext.
2. Issuance binds each token to the provider and epoch.
3. **Keep the SEC-06 binding.** The manifest signer is the per-job ephemeral key, and it must equal the delivering PeerId; `signer_matches_peer` stays. Only `is_authorized_submitter` is replaced, by token verification.
4. Commit `H(token)` inside the signed manifest payload, so a token can never be detached and re-attached to another job.
5. Spend atomically before execution, and key the replay cache by `H(token)`.
6. One token per job, including retries.
7. Never reuse an ephemeral key across conversations. Per *conversation* is the realistic granularity, because content links turns anyway, and per-prompt swarms cost seconds over Tor.

### Q5. KV-cache scrubbing reality check (measured on b12651)

**Setup:** a 35B-A3B MoE at Q4 on the Mac M5 Max role machine, one slot (`-np 1`), a 2,150-token prompt.

**Run 1** used lucidd's flags (`--cache-reuse 256`, otherwise defaults, including `--cache-ram 8192`):

| # | Request | `cache_prompt` | `prompt_n` | Prompt eval |
|---|---|---|---|---|
| 1 | Prompt A, first time | true | 2150 | 1132 ms |
| 2 | A again | true | **4** | 15 ms |
| 3 | Prompt B, first time | **false** | 2150 | 823 ms |
| 4 | B again | true | **4** | 18 ms |
| 5 | A again, after B (single slot) | true | **4** | 14 ms |
| 6 | First half of A plus a new suffix | true | 1056 (full) | 539 ms |
| 7 | Unrelated prompt | true | 2292 | 856 ms |

What Run 1 shows:
- Row 4: a `cache_prompt: false` request **still leaves its KV for the next request to reuse**.
- Row 5: A's KV came back from the host-RAM cache after B had replaced it in the slot.
- Row 6: the half-prefix probe was recomputed in full. The server log showed a keep-ratio of 0.49, apparently below its reuse threshold. So this build does not leak arbitrary short prefixes. The channel is confirmation of exact or extended repeats.

**Run 2** set `--no-cache-prompt --cache-ram 0` server-wide, with the same request bodies:
- Rows 2 and 4 still hit (`prompt_n` = 4). A per-request `true` overrides the server default.
- Row 5 recomputed in full. `--cache-ram 0` does disable the RAM restore.

**Run 3** used Run 2's flags plus `cache_prompt: false` on **every** request:
- Every repeat was recomputed in full (about 750–800 ms).
- But `tokens_cached` still equalled the prompt length after each request. The last prompt's KV stays resident until overwritten. Removing a sequence frees cache entries without zeroing the memory.

**Therefore:**
- A per-request `X-Lucid-Ephemeral` opt-in is insufficient on a shared server. The next tenant's ordinary request reuses or probes the ephemeral job's KV.
- What works: serve remote traffic with `cache_prompt: false` enforced on every remote job, `--cache-ram 0`, and `--cache-reuse 0`. Ideally, run self-traffic and remote traffic in separate `llama-server` instances, so the operator keeps prefix caching for their own chats.
- A true scrub needs a process restart. Fresh pages are zero-filled across processes on macOS; confirm that per GPU driver for CUDA.

**Zeroize claims.** `zeroize` on Rust buffers has limited reach: copies made by serde, hyper, `Bytes` and `String` growth, the HTTP body sent to `llama-server`, and that server's own heap all remain. Do not overclaim it. Higher-value, cheaper measures:
- core dumps off for `lucidd` and the child;
- encrypted-swap guidance for Linux providers;
- a guarantee that `llama-server` never runs with `-v`/`-lv` (lucidd logs child output at trace, `worker_llama/mod.rs:1539`).

### Q6. Layer 3 attestation supply chain

Yes, a separate pluggable verifier crate. But crate boundaries are not where the brittleness comes from. That comes from **collateral and policy**:
- **Staple collateral.** The worker should ship the VCEK/PCK chain, TCB info and the GPU RIM/certificates inside its attestation bundle. The consumer then verifies offline, and no vendor service (KDS, PCS, NRAS, OCSP) learns who is checking which node, when.
- **Signed reference values.** Publish them (measurements, minimum TCB/SVN, allowed GPU firmware) as a versioned, release-key-signed policy file that consumers pin.
- **Bind everything into `report_data`.** Use `H(pk_tee || receipt_key || model_cid || policy_id || epoch_nonce)`, so receipts are signed by a TEE-bound key rather than the host's identity key.
- **Re-attest per epoch, not per request.**
- Do not build any of this before a confidential-computing node exists in the fleet.

### Q7. TailsOS and Tor circuit alignment

There are real pitfalls:
- **One circuit carries everything.** libp2p keeps one long-lived TCP connection to the relay, multiplexing every stream over a single Tor circuit. Per-job ephemeral PeerIDs on that connection are trivially linkable by the relay: same exit, same TCP connection, adjacent timing. Each ephemeral identity needs its own TCP connection over an isolated circuit. That requires SOCKS5 support in lucidd with per-identity credentials (`IsolateSOCKSAuth`). `MaxCircuitDirtiness` does not rotate streams that already exist.
- **Leaks outside Tor.** QUIC/UDP and dials to learned direct addresses bypass Tor under `torsocks` (finding 5).
- **No Tor transport.** The guide never says how TCP reaches Tor.
- **The relay sees pairs.** It still sees which ephemeral PeerID talks to which provider, with volumes and timing, plus the token-length sequence (finding 6).
- **Tor latency** compounds per-token streaming.

**A good resolution of the Dotfiles trade-off.** Persist only the enrollment identity, and use it **only for token issuance**. Keep tokens in RAM only, and use ephemeral client-only identities for redemption. The persisted identity then links issuance events, which are authenticated anyway, and never prompts. Prefetch tokens at epoch start, not on demand.

## Concrete Recommendations (Priority Order)

1. **Ship now, independent of the RFC.** Stop cross-tenant KV reuse for remote jobs:
   - force `cache_prompt: false` for all relayed jobs;
   - `--cache-ram 0` and `--cache-reuse 0` on any instance that serves remote traffic, or separate self and remote instances;
   - a fixture test asserting the request body;
   - an `#[ignore]` real-binary test that replays the Q5 probe.
2. **Correct the threat matrix.**
   - Add a co-tenant consumer row and a token-length side-channel row.
   - Downgrade ISP/relay to "content yes; response length and token timing no".
   - Pad or coalesce streamed frames: bucketed sizes, optionally at a fixed cadence.
3. **Make the Tor workflow true before advertising it.**
   - An outbound-only/Tor profile that disables QUIC and refuses to dial anything except pinned relay and circuit addresses.
   - SOCKS5 support with stream isolation.
   - Verify with a packet capture.
4. **Close consumer-side retention.**
   - Salt `job_spec_hash` with a per-job secret.
   - No evidence persistence for ephemeral jobs.
   - No resumption tokens in unlinkable mode.
   - Truncate `created_at` to a coarse bucket.
5. **Layer 1 design changes.**
   - Per-provider VOPRF with key consistency.
   - A spent set bounded by issuance (Q2).
   - The token committed inside the manifest, keeping SEC-06 with ephemeral signers.
   - A client-only ephemeral transport profile with Kademlia in client mode.
   - Per-conversation identities.
   - No "unlinkable" wording without known circle size.
   - Refuse `trusted-circle-sharded` routes for unlinkable jobs unless the user opts in.
6. **Re-scope Layer 2 to measures that work.** Cache configuration (item 1), core dumps off, swap guidance, no prompt logging at any level, consumer-side retention (item 4). Drop the inner HPKE envelope from Phase A.
7. **Layer 3: interface only for now.** A `phase-attest` verifier trait plus stapled-collateral and policy formats. HPKE moves here.
