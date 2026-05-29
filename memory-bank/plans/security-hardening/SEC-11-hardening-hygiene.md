# SEC-11 — Defense-in-depth hygiene bundle

**Severity:** 🔵 LOW (L2, L5, L7, L9) | **Phase:** 4 | **Effort:** small (1 agent-wave, batched) | **Status:** planned

A bundle of small, independent hardening items. Each is cheap; grouped to avoid four tiny PRs.

## L7 — Add `#![deny(unsafe_code)]` to the 4 crates missing it
**Files:** `crates/phase-net/src/lib.rs`, `crates/phase-identity/src/lib.rs`, `crates/lucidd/src/lib.rs`, `crates/plasm/src/lib.rs`
No `unsafe` exists in these today (verified). Add the deny attribute to prevent regression. **Caveat:** plasm/lucidd pull wasmtime/libp2p which use `unsafe` internally — `deny(unsafe_code)` is per-crate (your code only), so this is safe to add; it won't fight dependencies. Verify the build after adding (a proc-macro or generated block occasionally trips it; if so, scope to the modules).

## L5 — Manual `Debug` for `NodeIdentity`
**File:** `crates/phase-identity/src/keypair.rs:25` (`#[derive(Debug, Clone)]`)
dalek 2.x redacts the secret and nothing prints a `NodeIdentity` today, so no live leak. Replace the derived `Debug` with a manual impl that prints only the public key / PeerID, so a future `debug!("{identity:?}")` can never regress into a secret leak. Defense-in-depth.

## L2 — Stream artifact range responses instead of buffering
**File:** `crates/phase-artifact-server/src/server.rs:398` (`vec![0u8; content_length]` + `read_exact`)
Bounded by file size (not unbounded), but N concurrent range requests on a large artifact = N×size RAM. Replace the read-into-Vec with a streamed body: `tokio_util::io::ReaderStream` over the file `.take(len)` after `seek(start)`. Caps per-request memory regardless of artifact size.

## L9 — Recover poisoned locks instead of re-panicking
**File:** `crates/lucidd/src/policy.rs:251-324` (`.read()/.write().expect("...poisoned")`)
These only fire if a lock is already poisoned by a prior panic (cascade amplification, not a primary trigger). Replace `.expect()` with `.unwrap_or_else(|e| e.into_inner())` to recover the guard, so one panic doesn't cascade into node-wide policy-check failures. Low priority but trivial.

## Acceptance criteria
- 4 crates have `#![deny(unsafe_code)]`; workspace still builds.
- `NodeIdentity` `Debug` prints only public material (test it).
- Artifact range responses stream (per-request memory constant w.r.t. file size).
- Policy lock access recovers from poison.
- 210 tests pass; clippy clean.

## Test plan
- Build the workspace (deny(unsafe_code) compile check).
- Test: `format!("{:?}", node_identity)` contains the pubkey/PeerID and **not** the secret bytes.
- Test: a large-file range request doesn't allocate the whole file (harder to unit-test; at least confirm the streaming code path via a behavioral test).

## Dependencies
None. All independent, single-file edits. Safe to do anytime.
