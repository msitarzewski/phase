# SEC-01 — Signer authorization on all Rust execute paths

**Severity:** 🔴 CRITICAL (C1) | **Phase:** 1 | **Effort:** medium (1–2 agent-waves) | **Status:** planned
**This is the keystone fix.**

## Why
`SignedManifest::verify()` decodes the pubkey *from inside the object* and checks the signature against it (`phase-manifest/manifest.rs:92-136`). That proves "some keyholder signed this," **not** "an authorized party signed this." Today:
- **lucidd relay** dispatches inference jobs with **no `verify()` call at all** (`router.rs:376→417`; `worker_llama.rs:313-352` computes `manifest_hash()` but never verifies).
- **plasm WASM** calls `verify()`, it passes for any self-signed manifest, then runs attacker WASM with caps taken from the same untrusted manifest (`plasm/src/worker.rs:112-147`).

Net: any anonymous internet peer gets free use of your GPU (lucidd) or runs arbitrary WASM (plasm). Combined with SEC-02's wasmtime sandbox-escape CVEs, plasm becomes anonymous host RCE.

## ⚠️ Decision required from Michael before implementing
What is the authorization policy? Pick one (the task implements whichever you choose):
1. **Allowlist** — operator config lists authorized client pubkeys (hex). Default-deny. Most explicit; fits the "soft launch with peer allowlist" risk mitigation already in the LUCID release plan.
2. **PeerID-bind** — accept jobs from any *connected* peer but bind the signed manifest's pubkey to the libp2p PeerID that delivered it (reject mismatches), then apply rate-limiting + reputation later. Open-but-attributable.
3. **Hybrid** — allowlist for v0.1 (soft launch), relax to open+rate-limited+reputation in v0.2.

Recommendation: **option 3** — ship allowlist now (matches the documented soft-launch plan), with the PeerID-bind plumbing in place so v0.2 just flips the policy. The work below assumes option 3.

## Scope
- `crates/lucidd/src/policy.rs` — add `authorized_submitters: Vec<String>` (hex pubkeys) to `PolicyConfig` + an `is_authorized_submitter(pubkey) -> bool`. Empty list in v0.1 default with a clear comment + a `allow_unauthenticated_jobs: bool` (default `false`) escape hatch for local/testing.
- `crates/lucidd/src/router.rs` — in `make_inbound_relay_handler`, after decode: (a) `job.verify()`, (b) extract `signer_pubkey`, (c) assert authorized (allowlist OR matches the delivering PeerID — see SEC-06 for getting the PeerID into the handler), (d) reject with `JobRelayResponse::Err` otherwise. Cap `max_tokens`/duration server-side.
- `crates/plasm/src/worker.rs` — `WasmtimeWorker::execute` must take an authorization check; cap `max_memory`/`max_duration` to operator-configured server-side limits regardless of manifest values.
- Possibly `crates/phase-protocol/src/worker.rs` — if the `Worker` trait needs an `authorized_keys`/policy handle threaded through `execute`. Prefer passing authz at the router/handler layer so the trait stays clean; only touch the trait if unavoidable.

## Acceptance criteria
- A self-signed manifest from an un-allowlisted key is **rejected** before any worker dispatch, on both the lucidd relay path and the plasm path.
- `verify()` is actually called on every execute path (grep proves no execute without a preceding verify).
- Server-side resource caps clamp manifest-supplied `max_memory`/`max_duration`/`max_tokens`.
- `allow_unauthenticated_jobs=true` restores open behavior for local dev (documented as insecure).
- 210 existing tests still pass.

## Test plan
- New integration test: two in-process nodes; node B sends a job signed by a key NOT in node A's allowlist → assert `JobRelayResponse::Err` and that the worker was never invoked (spy/counter).
- New test: job signed by an allowlisted key → accepted and executed.
- New test: manifest claiming `max_memory = u64::MAX` → clamped to server limit, not honored.
- plasm: existing hello.wasm round-trip still works when the signer is authorized.

## Dependencies
- SEC-06 provides the delivering PeerID to the handler (for option 2/3 binding). If SEC-06 isn't done yet, ship the allowlist half first; PeerID-bind lands when SEC-06 plumbs the peer identity through.

## Notes
This touches `router.rs` which SEC-05 and SEC-06 also touch — **land SEC-01 first.**
