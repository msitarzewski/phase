# Phase / LUCID Security Audit — 2026-05-28

**Scope:** Full-codebase sweep. 5 parallel specialist passes (network trust boundary, subprocess/path, cryptography, HTTP+PHP SDK, supply-chain/DoS/secrets) + empirical `cargo audit`.

**Bottom line:** The cryptographic *primitives* are well-built (proper CSPRNG, domain separation, sound canonicalization, tamper-evident commitment chain). The **trust model around them is not enforced** — `verify()` proves "someone signed this," nobody checks "someone *authorized* signed this," and the verifiable-compute receipt loop is currently a no-op on the consumer side. Combined with a vulnerable wasmtime and missing DoS guards, a worker node exposed to the internet today is wide open. The good news: the one node currently on the public internet (umbp) runs `--mode relay` with **no worker**, so it refuses all jobs — the live exposure is latent, not active.

---

## CRITICAL

### C1. No signer authorization anywhere — `verify()` ≠ "authorized"
**Root cause, three faces.** `SignedManifest::verify()` / `SignedReceipt::verify()` decode the pubkey *from inside the object* and check the signature against it (`phase-manifest/manifest.rs:92-136`, `phase-receipt/receipt.rs:79-107`). This is correct self-consistency but proves only that *some* keyholder signed it. Nothing pins that key to an authorized identity.

- **Inference (lucidd):** `make_inbound_relay_handler` (`router.rs:376→417`) decodes a remote `SignedManifest<JobSpec>` and dispatches it to the worker **without ever calling `verify()`** (`worker_llama.rs:313-352` computes `manifest_hash()` but never verifies). Any anonymous internet peer → free use of your GPU on any model you have loaded.
- **WASM (plasm):** `WasmtimeWorker` (`plasm/src/worker.rs:112-147`) calls `verify()`, it passes for any self-signed manifest, then runs attacker WASM. Resource caps (`max_memory`/`max_duration`) come from the *same untrusted manifest*. **Chains with C2 into host RCE.**
- **PHP SDK:** `Receipt::verify()` with no pinned key checks the signature against the receipt's own embedded pubkey (`Receipt.php:155-170`, `Crypto.php:42`). Both shipped examples use this insecure no-arg form. A forged receipt with attacker keypair verifies `true`.

**Fix:** After `verify()`, assert `signer_pubkey ∈ operator_allowlist` (or bind to the relaying libp2p `PeerId` the way the registry already does at `registry.rs:585`). Make the pinned key **mandatory** in PHP `verify()`. Cap `max_memory`/`max_duration` server-side regardless of manifest. This is the single most important fix.

### C2. wasmtime 27.0.0 — 16 advisories incl. sandbox escapes (empirical, `cargo audit`)
The supply-chain pass missed these (training cutoff predates the 2026 RUSTSEC entries). Confirmed by running `cargo audit`:
- **RUSTSEC-2026-0096** — miscompiled guest heap access → **sandbox escape on aarch64 Cranelift** (you run ARM64 on Mac + Linux — directly applicable).
- **RUSTSEC-2026-0095**, **RUSTSEC-2026-0087**, **RUSTSEC-2025-0118** — further sandbox-escape / out-of-bounds memory access.
- **RUSTSEC-2026-0149** — `wasmtime-wasi` `path_open(TRUNCATE)` bypasses `FilePerms::WRITE`.
- 11 more (panics = DoS, data leakage between pooling instances, etc.).

**Chain:** C1 (anonymous WASM execution) + C2 (sandbox escape) = anonymous remote peer → host code execution on any internet-exposed plasm worker. **Fix:** upgrade wasmtime to the latest 27.x patch or current stable (check the advisories' patched-version column) before any plasm node is exposed.

### C3. PHP dual-format downgrade + `local_execution` bypass
`Receipt.php` picks the strong Ed25519/canonical-JSON path only if `schema_version` AND `result` keys are present; otherwise silently falls to a legacy SHA-256-of-pipe-fields path (`Receipt.php:62-89`, `Crypto.php:80-102`). An attacker omits those keys to force the legacy branch, supplies their own pubkey+signature, and forges any `module_hash`/`exit_code`. Worse: `Receipt.php:158-159` returns `true` unconditionally for any legacy receipt with `node_pubkey === "local_execution"` — a magic-string total bypass. **Fix:** drop legacy verification for trust decisions; require pinned key + `isSignedEnvelope()`; delete the `local_execution` branch (local trust must come from transport context, never a field in the untrusted object).

---

## HIGH

### H1. Path traversal / arbitrary-file-load via `model_id` (local API)
`resolve_model_path` (`worker_llama.rs:363-374`): absolute `model_id` (`"/etc/shadow"`) returns verbatim; relative (`"../../etc/passwd"`) escapes `model_dir` because `Path::join` doesn't normalize `..`. Reachable from the local Ollama HTTP API (`ollama.rs:312,533`) with zero validation. The relay path is gated by the model-loaded check (`router.rs:410`), so remote peers can't hit it — but a local process, or anyone if `LUCIDD_HOST=0.0.0.0`, gets a filesystem existence + partial-content oracle (distinct error strings, `worker_llama.rs:235-238` vs `:701-708`) and feeds arbitrary files to llama.cpp's C++ GGUF parser. **Fix:** reject `model_id` containing `/`, `..`, `\0`, or leading `-`; canonicalize and assert `starts_with(model_dir)`; drop the absolute-path passthrough.

### H2. Receipts never verified or bound (the verifiable-compute thesis is unenforced)
`router.rs:429-431` pulls the `SignedReceipt` and **drops it** ("Best-effort: ... and drop it"). Nothing checks `receipt.worker_pubkey` == dispatched peer, `receipt.job_id` == dispatched hash, or `output_commitment` == replayed chunks. The commitment accumulator (`commitment.rs`) is sound but its guarantee is never realized because no verifier replays it. A malicious worker returns any result with a valid self-signature. **Fix:** call `receipt.verify()`, assert job_id + worker_pubkey binding, recompute commitment over received chunks.

---

## MEDIUM

### M1. DoS: no request-size cap, no concurrency cap, driver-task blocking
`cbor::Behaviour` built without `set_request_size_maximum` (`discovery.rs:252-259`); the inner JSON is `from_slice`'d unbounded (`router.rs:376`); no semaphore on inbound relay jobs; the handler is `await`ed *inline on the swarm driver task* (`discovery.rs:979`), so one slow/huge job stalls all peer event processing. **Fix:** set request/response size maxima; add a `tokio::Semaphore` concurrency cap; bound prompt/message length in the policy gate; `tokio::spawn` the handler off the driver task.

### M2. Unbounded model loading → memory exhaustion
`ensure_loaded` has no cap on concurrently-loaded models (eviction deferred to "M6", `worker_llama.rs:46`); each is ~GB RAM. Local API has no limit; relay peers can pin all your on-disk models. Port range is only 120 (`worker_llama.rs:130`) and `allocate_port` wraps and reuses live ports with no failure handling (`:292-305`). **Fix:** LRU cap using the existing `last_used` field; track allocated ports, return `Capacity` when full.

### M3. Non-atomic private-key write (world-readable window)
`storage.rs:67-86`: `fs::write` creates the key at umask default (~0o644), *then* `set_permissions(0o600)`. Between those lines the Ed25519 private key is world-readable on disk. No temp-file+rename; `load_or_create` has a TOCTOU race (`keypair.rs:50-60`). **Fix:** `OpenOptions::create_new(true).mode(0o600)` + write + atomic rename; `create_new` also closes the race.

### M4. DNS bootstrap unauthenticated + unbounded (newly introduced)
`resolve_dns_bootstrap_peers` (`main.rs:146-196`): no DNSSEC, silent fallback to Cloudflare/Google resolvers, and **every** TXT line starting with `/` is dialed with no cap. A spoofed/MITM `bootstrap.phasebased.net` TXT → eclipse-style seeding of attacker peers (bounded by Noise: PeerIds still can't be impersonated) + connection-flood amplification. **Fix:** cap multiaddrs per domain (e.g. 64); require `/p2p/<id>` in each; document trust-on-first-use; make resolver fallback opt-in.

### M5. hickory-proto DNS CVEs (empirical, introduced by `--bootstrap-dns`)
- **RUSTSEC-2026-0119** — O(n²) name-compression CPU exhaustion (both 0.24.4 and 0.25.2 present).
- **RUSTSEC-2026-0118** — NSEC3 unbounded loop on cross-zone responses (0.25.2).
A malicious resolver/MITM can hang/peg the bootstrap path. **Fix:** upgrade hickory-resolver to a patched line; dedupe the two versions in the tree.

### M6. Ollama log injection via unsanitized URI
`ollama.rs:212-215` logs `uri = %req.uri()` at WARN unsanitized. With `LUCIDD_HOST=0.0.0.0`, a request path with CRLF/ANSI escapes forges log lines / abuses terminals. **Fix:** log percent-decoded, control-char-stripped, length-capped `uri.path()` only.

---

## LOW / INFO

- **L1.** `nix 0.19.1` — RUSTSEC-2021-0119 OOB write in `getgrouplist` (old transitive dep). Upgrade the dependent.
- **L2.** Range requests buffer the full slice into RAM (`server.rs:398` `vec![0u8; len]`); bounded by file size but N concurrent × large artifact = memory amplification. Stream with `ReaderStream` instead.
- **L3.** `bincode 1.3.3` **unmaintained** (RUSTSEC-2025-0141) — still used for DHT advertisement encoding (`registry.rs`). Plan migration to `bincode 2.x` or `postcard`.
- **L4.** Unmaintained transitive crates: `fxhash` (RUSTSEC-2025-0057), `mach` (RUSTSEC-2020-0168), `paste` (RUSTSEC-2024-0436). Low risk, track.
- **L5.** `NodeIdentity` derives `Debug` (`keypair.rs:25`). dalek 2.x redacts the secret and nothing prints it today, but add a manual `Debug` printing only the public key to prevent future regression.
- **L6.** `ModelCid::from_model_id` hashes the model *name*, not *content* (`registry.rs:139`) — model-substitution: any peer can advertise a backdoored model under a popular name at zero cost. Documented v0.1 limitation; surface the advertising pubkey to users, plan content-hashed CIDs for v0.2.
- **L7.** 4 crates don't `#![deny(unsafe_code)]` — phase-net, phase-identity, lucidd, plasm. Add it (no `unsafe` exists today; this prevents regressions).
- **L8.** Subprocess inherits full parent env + `$PATH`-resolved `llama-server` (`worker_llama.rs:386,126`). `env_clear()` + require absolute binary path.
- **L9.** Lock-poison `expect()`s in `policy.rs:251-324` are cascade-amplification only (fire after a prior panic). Recover poisoned locks rather than re-panicking.

---

## Confirmed clean (credit where due)

- **Artifact-server path traversal — NOT exploitable.** axum per-segment `Path` extraction rejects embedded `/`; `is_valid_name` (`artifacts.rs:372-378`) blocks `/ \ .. .` empty; blob path gated by `BlobId::from_hex` (64-char hex) + prefix check. Solid.
- **Range parsing — safe.** `u64::parse` rejects negative/overflow; enforces `start<=end<file_size`; no underflow/over-alloc.
- **Crypto primitives — sound.** `OsRng` CSPRNG; signing envelope excludes pubkey/sig (no field-injection on covered fields); domain separation (`phase-manifest:v1:` / `phase-receipt:v1:` / `phase-protocol:v1:commitment`); deterministic canonical JSON with sorted keys + dup-key rejection; commitment chain detects truncation/reorder/extension/kind-substitution.
- **DHT advertisement verification — correct.** `SignedModelAdvertisement::decode` verifies; pubkey→peer_id binding checked; tamper tests cover caps/pubkey/sig/schema.
- **PHP comparison primitives — fine.** `===` for length checks, libsodium constant-time `verify_detached`, no `unserialize`/`eval`/`==` type-juggling. (The flaw is key-trust + downgrade logic, not the comparisons.)
- **"No telemetry" claim — SUBSTANTIATED.** Every outbound call accounted for: reqwest→127.0.0.1 only (llama-server), hickory→DNS bootstrap, libp2p peers, phase-fetch→user-supplied URL. No analytics/phone-home.
- **No secrets in repo.** No hardcoded keys/tokens; `.gitignore` covers `*.key` (commit a85adab).
- **Main daemon panic discipline — excellent.** Zero non-test panics on the swarm driver / startup path; phase-net's 11 unwraps are all `#[cfg(test)]`.

---

## Prioritized fix order

1. **C1** — signer allowlist / PeerId binding on every execute path + mandatory pinned key in PHP. (The keystone.)
2. **C2** — upgrade wasmtime before any plasm worker is internet-exposed.
3. **C3** — remove PHP legacy/`local_execution` bypass paths.
4. **M1 + M2** — DoS caps (size, concurrency, model-LRU, off-driver-task spawn) before exposing a worker node.
5. **M3** — atomic 0o600 key write.
6. **M4 + M5** — DNS bootstrap caps + hickory upgrade.
7. Wire `cargo audit` (and ideally `cargo deny`) into CI so C2/M5-class issues surface automatically. **Tooling gap: there is currently no automated dependency-advisory check** — this audit found 20 only because it was run by hand.

*Generated by a 5-agent parallel audit + `cargo audit`. Severity calibrated to the v0.1 trust model (no auth/no payment by design; routed-prompt visibility is an accepted design decision, not a finding).*
