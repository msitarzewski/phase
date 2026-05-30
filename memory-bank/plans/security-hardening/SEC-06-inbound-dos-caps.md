# SEC-06 — Inbound DoS caps (size, concurrency, off-driver execution)

**Severity:** 🟡 MEDIUM (M1) — but a prerequisite for safe public worker exposure | **Phase:** 3 | **Effort:** small–medium | **Status:** planned

## Why
Three compounding DoS gaps on the inbound relay path:
1. **No request-size cap.** `cbor::Behaviour` is built without `set_request_size_maximum` (`discovery.rs:252-259`); even within libp2p's default, the inner JSON is `serde_json::from_slice`'d unbounded (`router.rs:376`), and prompt/message length is unbounded before the worker.
2. **No concurrency cap.** No semaphore on inbound relay jobs — a peer floods job requests, each triggering a real (GPU/CPU-heavy, ~GB) dispatch.
3. **Driver-task blocking.** The handler future is `await`ed *inline on the swarm driver task* (`discovery.rs:979`), so one slow/large job **stalls the entire swarm event loop** — DoS amplification: one request degrades all peer connectivity.

## Scope
- `crates/phase-net/src/discovery.rs` — request_response config (size maxima), the inline `handler(...).await` in `handle_job_relay_event` (`~965-979`).
- `crates/lucidd/src/router.rs` — `make_inbound_relay_handler`: add a concurrency semaphore + prompt/message length bound (coordinate with SEC-01's gate, same function).
- `crates/lucidd/src/policy.rs` — `max_concurrent_remote_jobs` already exists in `PolicyConfig`; wire it to a real `tokio::sync::Semaphore`.

## Approach
1. `request_response::Config::set_request_size_maximum(N)` + `set_response_size_maximum(M)` on both the job-relay and job-offer behaviours (pick sane caps: a manifest is KBs, a relayed `Vec<JobEvent>` for a capped-token response is bounded — size accordingly, e.g. 256 KiB request / a few MiB response).
2. Bound `messages`/`prompt` length in the policy/authz gate (reject oversized before dispatch). Tie the cap to the same server-side `max_tokens`/context limits from SEC-01.
3. Wrap dispatch in a `tokio::sync::Semaphore` sized to `policy.max_concurrent_remote_jobs`; return `JobRelayResponse::Err{busy}` (or queue with a bounded queue) when exhausted.
4. **Move execution off the driver task:** `tokio::spawn` the handler and reply via the channel, so the swarm event loop keeps polling. Confirm the relay-response plumbing supports an async reply (it uses `ResponseChannel` — spawn, then `send_response` from the spawned task).

## Acceptance criteria
- Oversized relay request → rejected at the codec, never buffered fully.
- Oversized prompt → rejected at the gate.
- More than `max_concurrent_remote_jobs` in flight → excess rejected/queued, not unbounded.
- A slow job does not block processing of other peers' events (the swarm loop stays responsive).
- 210 tests pass.

## Test plan
- Test: relay request exceeding the size cap → codec error / `Err`, no OOM.
- Test: N+1 concurrent jobs with cap N → the N+1th is rejected/queued.
- Test (timing/behavioral): while a long job runs, a second peer's job-offer still gets a timely response (proves off-driver execution).

## Dependencies
- **Coordinate with SEC-01** (same `make_inbound_relay_handler`). Recommended: SEC-01 lands the gate + authz, SEC-06 adds the semaphore + size caps + off-driver spawn on top. Single owner for `discovery.rs` changes.
