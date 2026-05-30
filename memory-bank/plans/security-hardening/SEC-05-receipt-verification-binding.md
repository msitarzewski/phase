# SEC-05 — Verify and bind receipts (make verifiable-compute real)

**Severity:** 🟠 HIGH (H2) | **Phase:** 2 | **Effort:** medium (1 agent-wave) | **Status:** planned

## Why
`router.rs:429-431` pulls the `SignedReceipt` from a peer-served job and **drops it** ("Best-effort: pull the signed receipt and drop it"). The entire verifiable-compute thesis — cryptographic proof that a specific worker executed a specific job — is unenforced on the consumer side in v0.1. Nothing checks:
- `receipt.worker_pubkey` == the peer the job was dispatched to (`execute_via_peer`, `router.rs:258`),
- `receipt.job_id` == the dispatched `manifest_hash`,
- `result.output_commitment` == commitment recomputed over the received chunks.

A malicious worker returns any result with a valid self-signature over an attacker-chosen job_id. `SignedReceipt::verify()` itself is correct (`receipt.rs:79-107`) and the commitment accumulator (`commitment.rs`) is sound — they're just never invoked. This is "the good crypto exists, nobody calls it."

## Scope
- `crates/lucidd/src/router.rs` — the peer-relay receipt path (`~290-301` decode, `~429-431` the drop).
- Possibly `crates/phase-protocol/src/commitment.rs` — expose a verifier helper if one isn't already public (replay chunks → recompute terminal commitment).

## Approach
In `execute_via_peer`, after collecting the `Vec<JobEvent>` from the peer:
1. Extract the `SignedReceipt<JobResult>` (the relay response must carry it — **note:** the audit flagged that peer-served receipts don't currently propagate back through the relay; only the commitment rides in `JobEvent::Final`. So this task may first need to extend `JobRelayResponse` to include the receipt. Confirm and extend if so.)
2. `receipt.verify()` — reject on failure.
3. Assert `receipt.job_id_bytes() == dispatched_job_id` (the `manifest_hash` we sent).
4. Assert `receipt.worker_pubkey` corresponds to the PeerID we dispatched to (derive PeerID from pubkey, compare — same primitive as `registry.rs:585`).
5. Recompute the commitment by replaying the received `OutputChunk`s through a `CommitmentAccumulator` and assert it equals `result.output_commitment`.
6. On any mismatch: surface an error to the API client (the response is unverifiable) and optionally ding the peer's local reputation EMA (future).

Decide the failure UX: does an unverifiable receipt fail the request, or return the tokens with a warning header (`X-Lucid-Receipt-Verified: false`)? Recommendation: return tokens but set the header false, and log — v0.1 is "friend's GPU" trust; v0.2 with reputation can harden to reject.

## Acceptance criteria
- Peer-served responses carry the worker's `SignedReceipt`.
- The receipt is verified, job_id-bound, worker-pubkey-bound, and commitment-checked.
- A tampered receipt (wrong job_id, wrong key, or output not matching commitment) is detected.
- Response exposes verification status to the client.

## Test plan
- Integration test (mock/in-process peer): peer returns a valid receipt → verified true, headers reflect it.
- Peer returns a receipt with mismatched job_id → detected.
- Peer returns output chunks that don't match the signed commitment → detected.
- Peer returns a receipt signed by a different key than the dispatched PeerID → detected.

## Dependencies
- **Land after SEC-01** (shares `router.rs`; SEC-01 establishes the authz plumbing and the PeerID-in-handler pattern this reuses).
- May require extending `JobRelayResponse` (phase-net protocol) to carry the receipt — small protocol addition, keep wire-compatible-ish or bump a schema version.
