# Review: privacy-inference-rfc.md - Microsoft MAI 2
**Date**: 2026-11-27 | **Commit Reviewed**: `135d4f0`

## Verdict

**Needs redesign, then a staged implementation.**

The RFC's direction is useful, but its current wording crosses important
security-property boundaries. Phase A blind authorization can plausibly make
token issuance unlinkable from redemption for a consumer inside a circle.
Phase B currently does not establish zero retention, and Phase C is not an
attestation design until it specifies the complete measured boot, GPU, model,
runtime, key-binding, and revocation chain.

The implementation should use narrower claims:

- **Anonymous network access** is possible only with fail-closed Tor-only
  transport, per-request identity and stream isolation, and a meaningful
  anonymity set.
- **Serving-operator confidentiality** does not exist on ordinary consumer
  GPUs. HPKE hides bytes from a passive relay, not from the process receiving
  and decrypting them.
- **Receipt integrity** proves that a particular identity signed a transcript
  or output commitment. It does not prove that the requested model executed
  correctly.
- **Reduced retention** is plausible. “Zero retention” and GPU-memory erasure
  are not established by HPKE, Rust-side `zeroize`, or `cache_prompt: false`.

## Findings

| # | Severity | File | Lines | Vulnerability | Confidence |
|---|----------|------|-------|---------------|------------|
| 1 | 🟠 HIGH | `docs/tails-and-tor-guide.md` | 24-48 | The “Tor mode” workflow documents ordinary direct TCP with no SOCKS connector or fail-closed enforcement. A source IP can be exposed, and transports outside Tor remain possible. | 9/10 |
| 2 | 🟠 HIGH | `memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md` | 79-95 | The attestation layer lacks a complete verified-boot, GPU-binding, model/runtime measurement, revocation, and rollback policy. The root-host confidentiality claim is conditional, not established. | 8/10 |
| 3 | 🟡 MEDIUM | `apps/lucid-desktop/src/lib/ChatPanel.svelte` | 25-60 | The UI defaults `localOnly` to false and can send complete history remotely while labeling the entry point a “private chat.” | 10/10 |
| 4 | 🟡 MEDIUM | `memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md` | 64-77 | HPKE and Rust `zeroize` do not erase GPU KV slots, allocator arenas, driver copies, subprocess memory, logs, crash artifacts, or swap. Current llama execution also enables prompt-cache reuse. | 9/10 |
| 5 | 🟡 MEDIUM | `memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md` | 53-62 | Spent-token handling lacks atomicity, crash recovery, exact-capacity semantics, saturation behavior, and transcript-bound possession proof. A Bloom-style or evicting store can permit replay or deny valid work. | 8/10 |
| 6 | 🟡 MEDIUM | `memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md` | 60-62 | Cryptographic token unlinkability does not prevent linkage through prompt content, writing style, model selection, issuance batches, timing, persistent circuits, or an anonymity set of one. | 9/10 |

## What Breaks Against Current Codebase (cite file:line)

### Tor is documented but not enforced

The current transport can make normal direct TCP connections:
`crates/phase-net/src/discovery/mod.rs:901-909`. `--outbound-only` disables
direct listeners and LAN discovery, not arbitrary outbound dialing:
`crates/lucidd/src/main.rs:340-358`. A TCP-only Tor claim is therefore
not currently a property of the process.

The RFC's threat matrix at
`memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md:29-36` depends on a
Tor connector, isolation, and fallback prevention that the current mode does
not enforce. `docs/tails-and-tor-guide.md:24-48` documents literal-address
behavior without a SOCKS configuration, proxy health check, or fail-closed
transport. Following it outside a separately enforced host firewall can make a
direct connection and expose the consumer's ordinary source address.

**Required direction:** add an explicit Tor/SOCKS-only mode that owns every
`lucidd` outbound connection, rejects direct fallback, and disables QUIC, mDNS,
direct DHT dialing, DCUtR/hole punching, and unproxied relay retries. A proxy
failure must result in zero prompt dispatch and zero non-loopback connection
attempts.

### Chat consent and privacy labelling are unsafe by default

`apps/lucid-desktop/src/lib/ChatPanel.svelte:25-60` defaults `localOnly` to
false and sends the full conversation history. The backend adds
`X-Lucid-Local-Only` only when the user opts in:
`apps/lucid-desktop/src-tauri/src/chat.rs:100-112`. The component nevertheless
labels the primary empty state “Start a private chat”:
`apps/lucid-desktop/src/lib/ChatPanel.svelte:128-133`.

The first message can therefore disclose all included history to an enrolled
serving operator without an explicit remote disclosure at the action boundary.
Default sensitive chat to local-only, require per-session remote consent, show
the selected route before dispatch, and explain that conversation history
leaves the device as one request.

### Layer 2 does not match llama's current retention behavior

The llama worker launches with `--cache-reuse 256`:
`crates/lucidd/src/worker_llama/mod.rs:1143-1147`. Chat completion requests set
`"cache_prompt": true`:
`crates/lucidd/src/worker_llama/mod.rs:1604-1608`.

MLX sets `--prompt-cache-size 0` at
`crates/lucidd/src/worker_mlx/mod.rs:1308-1329`, which is a better reuse
posture, but it does not prove that live request KV data, allocator arenas,
driver buffers, GPU command buffers, or crash artifacts are erased.

`cache_prompt: false` may prevent later prefix reuse. It is not evidence of
post-job memory erasure. Layer 2 should be named **reduced retention**, with a
separate backend-specific process lifecycle, until a runtime can provide and
test a real clear operation.

### Replacing current inbound identity binding is underspecified

Current inbound authorization ties the manifest client key to the delivering
PeerID and policy authorization at
`crates/lucidd/src/router/mod.rs:2125-2133`. The RFC proposes replacing this
with an anonymous redemption path, but `check-and-insert` at
`memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md:59` is not a complete
authorization transition.

The replacement must preserve provider identity, manifest integrity, response
key identity, expiry, challenge freshness, and a replay-proof spent-state
transition. A token hash alone is a reusable bearer secret unless possession is
bound to a one-time transcript.

## Subtle Attacks, Side Channels & Over/Under-Specification

### Token theft and downgrade

A relay or malicious observer that obtains the serialized request must not be
able to replay or transplant it. Binding a hash to plaintext fields is not a
proof: an attacker with the bearer token can recompute a hash.

Require a token-derived proof or MAC over a canonical transcript that includes:

- provider PeerID and token scope;
- epoch and protocol version;
- ephemeral consumer PeerID;
- `job_id` and canonical manifest hash;
- response-key hash;
- expiry and server challenge/nonce.

HPKE authenticated additional data must cover the same transcript. The
provider must verify and atomically consume the redemption before decrypting
or dispatching expensive work. A privacy-labelled session must not silently
fall back from token authorization to persistent-key authorization.

### Crash and capacity replay

Authorization state cannot be merely “bounded”; it must be exact enough to
reject every spent token that is still valid. An eviction policy, Bloom false
positive, or reset after an unclean restart is either a replay or availability
failure.

At capacity, fail closed: stop issuance, reject new redemptions, and expose
pressure through an operator metric. Retire expired epochs only after their
validation window and all crash-recovery paths are closed.

### Semantic and traffic linkability

Blind tokens remove a cryptographic link between issue and redeem. They do not
hide:

- the complete plaintext from the serving peer;
- names, secrets, writing style, and stable user intent;
- model ID, options, token cadence, request duration, or sequence;
- issuance batch size and refresh timing;
- persistent Tor, Yamux, relay, or peer connections;
- global timing correlation.

The RFC acknowledges `K = 1` at
`memory-bank/releases/lucid-v0.2/privacy-inference-rfc.md:21-23`, but later
wording risks presenting unlinkability as a user-wide guarantee. The client
should report measured eligibility/anonymity `K`; a privacy-labelled session
should fail below an operator-defined minimum.

### GPU and process residue

`zeroize` covers selected Rust buffers, not:

- llama.cpp or MLX heaps;
- active and cached KV slots;
- GPU/driver allocations and command queues;
- swap, core dumps, logs, diagnostics, or temp files;
- model-runtime copies and allocator retention.

Where the runtime cannot expose a verified erasure primitive, use a dedicated
ephemeral process pool and terminate/reap the process after the sensitive job.
Do not claim post-execution VRAM erasure without backend and driver evidence.

### Attestation is not the same as confidentiality

A valid CPU quote for an arbitrary image is not proof of a safe inference
service. The verifier needs one freshness-bound chain covering:

1. vendor root and TCB policy;
2. verified boot and exact kernel/initramfs;
3. `lucidd` and worker-runtime image measurements;
4. GPU confidential-compute quote and CPU/GPU key binding;
5. model content hash and runtime configuration;
6. response-decryption key possession;
7. anti-rollback and policy-expiry checks.

Even that does not remove model-selection traffic, request volume, availability
failures, side channels, or malicious behavior by software inside the TCB.
The RFC should avoid saying a host is “cryptographically prevented” from seeing
plaintext until this full chain and failure policy exist.

### Accountability tension

Permanent per-member revocation and permanent issuer-facing request
unlinkability cannot both hold for the same simple anonymous token class.
The implementable choices are:

- anonymous issuance with epoch-wide revocation;
- pseudonymous, attributable issuance;
- conditional traceability through a separately designed and governed
  authority or cryptographic protocol.

The third option is not specified here and should not be silently introduced.

## Answers to Section 5 Open Design Questions (Q1-Q7)

1. **Q1 (Blind-Token Primitive)**: Start with **RFC 9497 Ristretto255 VOPRF**
   for the single-provider-circle design. It avoids unnecessary public
   verification and has a small token footprint. Do not redeem by presenting
   the reusable VOPRF output; derive a one-request possession proof/MAC over the
   complete transcript above. Domain-separate provider PeerID, epoch, purpose,
   and protocol version. Use RSA-BSSA only when unrelated parties genuinely
   need to verify delegated tickets. Candidate Rust crates include `voprf` and
   a maintained blind-RSA implementation, but versions and audit posture must
   be evaluated during dependency review; `#![deny(unsafe_code)]` does not
   constrain unsafe code inside dependencies. Acceptance: official vector
   tests, malformed/noncanonical encoding rejection, wrong-epoch/cross-provider
   rejection, transcript mutation failure, dependency audit, and documented
   transitive-unsafe review.

2. **Q2 (DoS & Spent-Token Store Bounds)**: Use an exact, fixed-cap,
   crash-safe spent-token set. A reasonable first envelope is a one-hour epoch;
   current plus one overlapping epoch with at most five minutes overlap;
   32-token issuance batches; 64 issued tokens per enrollment per hour; and a
   global exact-set capacity derived from enrolled identities × quota plus a
   10% reserve. Atomically consume before GPU dispatch, persist the mark, and
   retain it across cancellation and restart. Never evict an unexpired spent
   token to make space. Reject issuance or redemption fail-closed at capacity.
   Acceptance: 10,000 concurrent redemptions execute exactly once; process
   termination cannot make a token reusable; replay remains rejected at 100%
   capacity; expired state is reclaimable on a measured deadline.

3. **Q3 (Unlinkability vs Reputation / Revocation)**: Preserve consumer-side
   reputation of providers, because provider PeerID and signed receipts remain
   attributable. Provider-side abuse control should use future issuance
   revocation, epoch issuance ceilings, and a short-lived epoch-wide emergency
   revocation switch. A leaked batch then has a bounded but real theft window.
   Targeted revocation of one anonymous member is impossible without adding
   attribution; put that behavior in a separately labelled accountable mode.
   Acceptance: epoch revocation prevents new GPU work from old tokens,
   revoked enrollment receives no new tokens, and the UI states that the
   leaking member cannot be identified from anonymous redemptions.

4. **Q4 (Ephemeral PeerID & Relay Authz Binding)**: Treat the relay as an
   untrusted byte forwarder and require end-to-end Noise authentication to the
   serving peer. Derive the delivering libp2p identity from the ephemeral
   signing key. A one-request proof must bind provider PeerID, ephemeral PeerID,
   epoch/scope, `job_id`, canonical manifest hash, response-key hash, expiry,
   protocol version, and a fresh provider challenge. HPKE AAD covers the same
   transcript. Verify and atomically consume before decryption and GPU dispatch.
   `H(ephemeral_peer_id || job_id || manifest_hash)` alone is not proof.
   Acceptance: relay modification, delay, truncation, duplication, and
   reordering cannot authorize work; changed transcript fields fail; exact
   concurrent replay produces one execution; replay fails after restart.

5. **Q5 (KV-Cache Scrubbing Reality Check)**: Do not assume either backend
   provides zero retention. `cache_prompt: false` can disable cache reuse but
   does not erase a live slot, allocator, driver copy, or GPU allocation. For
   ephemeral jobs, disable reusable caching in command and request settings,
   use separate worker-process pools, suppress backend request logging,
   disable swap/core dumps where operationally possible, and terminate/reap
   the process if no verified cache-clear primitive exists. Zeroize Phase-owned
   buffers as narrow hygiene. Rename the claim “cache reuse disabled and
   process retired.” Acceptance: a fresh process allocation per ephemeral
   job, zero cross-request reuse counters, and fault/crash tests showing no
   prompt in controlled logs, temp files, dumps, or documented cache paths.

6. **Q6 (Layer 3 Attestation Supply Chain)**: Keep this in a separate,
   workload-neutral `phase-attest` crate, but do not call pluggability low
   maintenance. Ship signed, versioned, expiring policy that names vendor roots,
   accepted GPU models, minimum TCB versions, allowed measurements, and
   revocations. Verify CPU quote, GPU confidential-compute quote, fresh
   challenge, key binding, verified boot, exact runtime/`lucidd`/model
   measurements, and rollback state as one transaction. Trust labels must be
   generated locally only after complete verification; a remote header is never
   proof. Acceptance: every quote, key, challenge, model-hash, measurement,
   expiry, downgrade, and wrong-GPU mutation fails; stale policy produces an
   explicit degraded status rather than a silent success.

7. **Q7 (TailsOS / Tor Circuit Alignment)**: A per-boot PeerID is not enough.
   Use a fresh identity and fresh SOCKS isolation tag per anonymous request,
   with a dedicated Tor-only TCP connector. Disable QUIC, mDNS, DHT direct
   address dialing, DCUtR/hole punching, and direct fallback in that mode. A
   failed Tor health check must dispatch zero prompt bytes. Keep circuit
   lifetime short, but optionally add delay or batching where traffic
   correlation matters more than latency. Acceptance: packet capture under
   forced proxy failure shows no non-loopback traffic; two prompts have
   different PeerIDs/isolation tags; no discovered address bypasses Tor.
   Document that Tor still does not hide request volume, timing, duration,
   token cadence, model choice, prompt content from the serving peer, or
   global passive correlation.

## Concrete Recommendations (Priority Order)

1. Narrow the RFC terminology before implementation: anonymous authorization,
   reduced retention, attestation-backed execution, and receipt attribution are
   separate properties.
2. Implement a Tor-only transport with a negative network test: forced proxy
   failure produces no non-loopback traffic and no prompt dispatch.
3. Make `localOnly` default to true in sensitive chat UX and require explicit
   per-session consent plus route disclosure for remote execution.
4. Specify a transcript-bound anonymous token possession protocol and exact,
   crash-safe spent-state transition before replacing existing PeerID auth.
5. Replace “zero retention” with a per-job process lifecycle and backend tests.
   Do not claim GPU memory erasure without runtime and driver evidence.
6. Define the complete `phase-attest` verification and policy transaction;
   do not emit a confidential trust label from a partial quote.
7. Measure and expose anonymity `K`, and fail privacy-labelled mode below a
   configured threshold.
8. Add abuse tests before adding throughput: concurrent replay, restart replay,
   epoch revocation, capacity exhaustion, malformed tokens, relay mutation,
   Tor bypass, attestation rollback, and wrong-model/wrong-key substitution.
9. Keep output correctness distinct from receipt verification. A valid receipt
   can still authenticate a wrong, censored, or substituted model result.

## Assumptions to Falsify Before Implementation

1. Can a packet capture observe direct non-loopback TCP or UDP while Tor-only
   mode is selected or Tor fails?
2. Do two prompts share a Tor stream, relay circuit, Yamux connection, or
   observable timing pattern?
3. Is anonymity `K` measurable among eligible simultaneous redemptions rather
   than inferred from enrollment count?
4. Can issuance batch size, refresh time, scope, or delivery destination identify
   the enrollee?
5. Can process death, restart, crash recovery, or epoch rollover resurrect an
   accepted token?
6. Do prompt bytes remain recoverable from runtime allocations, GPU memory,
   swap, logs, diagnostics, or reused backend processes?
7. Does the attestation chain cover the loaded kernel, runtime, `lucidd`, model,
   GPU, and key, or only a CVM launch measurement?
8. Can a relay or host substitute an HPKE key outside the attested transcript?
9. Can a peer execute a different model or quantization, or emit arbitrary
   plausible output, while receiving a valid signed receipt?
10. Is targeted anonymous revocation impossible without a new traceability
    authority or accountability mode?
11. Can a first-time user send a prompt without understanding that complete
    history may leave the device?

Until these assumptions are tested, the RFC should remain a design proposal
with conditional claims rather than a confidentiality or anonymity guarantee.
