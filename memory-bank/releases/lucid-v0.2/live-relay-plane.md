# Milestone: Live Relay Plane

**Status:** 🟦 In progress — implementation checkpoint approved; physical/load acceptance pending
**Track:** Intended stream
**Blocking:** Yes
**Tracker:** [README.md](./README.md)

## Outcome

A requesting LUCID node receives remote `JobEvent`s as the serving worker emits them. Output is not buffered into a `Vec<JobEvent>` until completion. Cancellation, deadlines, bounded buffering, and the terminal signed receipt work across the peer boundary.

The current relay is deliberately batch-shaped: `JobRelayResponse::Ok` carries encoded events and a receipt (`crates/phase-net/src/protocol.rs:34-67`), and `CombinedBehaviour` uses libp2p request-response (`crates/phase-net/src/discovery.rs:281-306`). The accepted ADR states that v0.2 changes the wire delivery while preserving the `JobEvent` model (`memory-bank/decisions.md:637-655`).

## User-visible success

For a request routed to another peer, the first NDJSON token from `/api/chat` or `/api/generate` must reach the local HTTP client before the remote worker emits its terminal event. The response must still end with a receipt-backed commitment over exactly the delivered output chunks.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Workload-neutral stream | `crates/phase-protocol/src/worker.rs:315-378` | Transport `JobEvent` without adding a token-specific Phase primitive |
| Commitment | `crates/phase-protocol/src/commitment.rs:29-86` | Requester replays delivered `OutputChunk`s in sequence |
| Existing batch compatibility | `crates/phase-net/src/protocol.rs:16-67` | Keep or reject v0.1 explicitly; never ambiguously decode both versions |
| Swarm behavior | `crates/phase-net/src/discovery.rs:88-99`, `crates/phase-net/src/discovery.rs:132-195` | Extend the existing driver command/event model |
| LUCID outgoing route | `crates/lucidd/src/router.rs:330-530` | Stream frames into the existing local `JobStream` and verify the terminal receipt |
| LUCID incoming route | `crates/lucidd/src/router.rs:590-825` | Forward worker events as produced and honor policy/cancellation |
| HTTP edge | `crates/lucidd/src/ollama.rs:265-278`, `crates/lucidd/src/ollama.rs:396-1045` | Preserve Ollama NDJSON framing and client-disconnect cancellation |

## Required streaming ADR

Before the wire contract stabilizes, approve an ADR covering:

- Protocol identifier and version negotiation; the existing batch protocol must not be silently redefined.
- Open sequence: request envelope, accept/refuse response, event stream establishment.
- Frame envelope: schema version, job ID, monotonically increasing sequence, frame kind, payload length, and maximum.
- Whether `Progress` frames are transported and how they are bounded.
- Terminal sequence: final event, signed receipt, acknowledgement, and stream close.
- Cancellation and half-close semantics in both directions.
- Per-frame and per-job limits, compression policy, idle and total deadlines.
- Compatibility behavior for v0.1 batch peers.
- Retry boundary: a job cannot be blindly replayed after output has reached the client.

## Work packages

### Transport primitive

- [ ] Evaluate the smallest libp2p substream/behavior extension that supports server-pushed frames and cancellation.
- [ ] Keep transport types opaque to inference; `phase-net` may know `JobEvent` bytes or a generic framed payload, not tokens/models/Ollama.
- [ ] Integrate the behavior into `CombinedBehaviour` and the existing `Driver`, rather than running a second swarm.
- [ ] Expose a handle returning acceptance plus an async stream and terminal receipt result.
- [ ] Add protocol negotiation metrics and structured logs.

### Frame validation

- [ ] Reject unknown mandatory schema versions and oversized frames before allocation.
- [ ] Enforce job ID binding and monotonic event sequence.
- [ ] Bound `Progress` frequency and message length; progress is informational and not commitment-covered (`crates/phase-protocol/src/worker.rs:332-335`).
- [ ] Permit exactly one terminal `Final`; reject output after terminal or EOF without terminal.
- [ ] Treat duplicate, skipped, reversed, or conflicting frames as verification failures.

### Backpressure

- [ ] Use bounded channels between libp2p, router, and HTTP edge.
- [ ] Slow HTTP clients must slow or cancel upstream work rather than grow memory without bound.
- [ ] Document buffer sizes and the maximum memory cost per active stream.
- [ ] Ensure one slow stream cannot starve swarm polling, DHT refresh, or unrelated jobs.
- [ ] Load-test concurrent slow consumers and large output chunks.

### Cancellation and lifecycle

- [ ] Propagate HTTP disconnect → local router → peer stream → remote `JobHandle::cancel()`.
- [ ] Remote worker must release model slot/subprocess request state and produce a cancelled receipt where possible.
- [ ] Make cancellation idempotent and race-safe against natural completion.
- [ ] Define outcomes for requester crash, serving-peer crash, relay path loss, and terminal receipt loss.
- [ ] Do not retry onto a fallback peer after any committed output reached the client unless a resumable protocol is separately designed.

### Receipt verification

- [ ] Bind the terminal receipt to the dispatched manifest hash and intended serving PeerId, preserving the existing verification path at `crates/lucidd/src/router.rs:430-535`.
- [ ] Replay only `OutputChunk`s actually delivered to the client, in the same sequence.
- [ ] Compare chunk count, commitment, job ID, signer/PeerId, completion, and any error state.
- [ ] Treat a missing or invalid receipt as an attributable failed execution even if useful tokens were delivered.
- [ ] Surface verification state in terminal API metadata/headers without claiming success prematurely.

### Rolling compatibility

- [ ] Advertise supported relay protocol versions in peer capabilities or through negotiation.
- [ ] Define whether a v0.2 requester may use batch fallback, and expose that fallback to clients/operators.
- [ ] Never label replayed batch events as live streaming.
- [ ] Ensure new servers can explicitly refuse unsupported legacy security semantics.

## Invariants

1. At most one accepted execution exists per non-retried stream attempt.
2. Memory per stream is bounded independently of generation length.
3. The HTTP client observes events in the order signed by the serving worker.
4. `Final` is terminal and unique.
5. Success is not verified until the receipt passes all bindings.
6. Cancellation is attributable and cannot be mistaken for a successful stop.
7. Transport loss cannot manufacture a successful completion.
8. `phase-net` remains workload-neutral.

## Failure and security tests

- [ ] First token timing proves remote delivery before generation completion using a worker that intentionally delays later tokens.
- [ ] Slow-client test holds memory within the documented bound.
- [ ] Client disconnect cancels remote work and releases capacity.
- [ ] Serving peer disconnect before first frame, mid-output, after `Final`, and before receipt each map to distinct deterministic outcomes.
- [ ] Duplicate, missing, reversed, oversized, malformed, cross-job, post-terminal, and unknown-version frames are rejected.
- [ ] Wrong manifest hash, wrong signer, wrong PeerId, wrong commitment, wrong chunk count, and expired manifest are rejected.
- [ ] Concurrent streams cannot cross-deliver frames or receipts.
- [ ] Batch fallback is labeled and security-equivalent where permitted.
- [ ] DHT and heartbeat behavior remain responsive during high stream concurrency.
- [ ] Fuzz the frame decoder and lifecycle state machine.

## Acceptance criteria

- [ ] Remote first-token latency is measured independently from total generation time and demonstrates genuine live delivery.
- [ ] All current local inference and embedding streams remain behaviorally compatible.
- [ ] Cancellation crosses two peers and frees serving capacity.
- [ ] 100+ concurrent bounded test streams do not show unbounded memory growth or swarm starvation; the final target is recorded from measured hardware.
- [ ] Receipt verification covers exactly the chunks emitted to the requester.
- [ ] A v0.1 peer encounter follows the documented compatibility policy.
- [ ] Real two-node chat and generate requests pass, including failure injection.
- [ ] Workspace tests, clippy, audit, and release builds pass.

## Explicit non-goals

- Multi-hop onion routing or prompt privacy.
- Seamless job migration after partial output.
- Treating progress telemetry as verified output.
- Transport-layer interpretation of token, embedding, image, or WASM payloads.
- Hiding batch fallback behind a streaming-looking client response.

## Completion evidence required in the tracker

- Approved streaming ADR and protocol-state diagram.
- First-token timing trace from two physical nodes.
- Cancellation/backpressure memory evidence.
- Malformed-frame and receipt-binding QA logs.
- Exact commit/PR and task documentation.

## 2026-08-09 approved implementation checkpoint

- Implemented the workload-neutral `/phase/job-relay-stream/2.0.0` substream alongside explicit v1 compatibility, with immediate ordered frames, bounded channels, cancellation controls, terminal receipt delivery, and fail-closed lifecycle validation in `crates/phase-net/src/{protocol,discovery}.rs`.
- LUCID now prefers v2 live relay, exposes output as it arrives, replays exactly delivered chunks into receipt verification, and shares replay/concurrency gates across v1/v2 at `crates/lucidd/src/router.rs:667-1138` and `crates/lucidd/src/router.rs:1763-2060`.
- Linux qualification exposed and fixed a production timing flaw: transport mutex/multistream negotiation now has a separate bounded deadline, while caller idle timeout starts after establishment (`crates/phase-net/src/discovery.rs:1370-1482`, `1556-1567`).
- Native Ubuntu x86_64 passed all 35 `phase-net` tests, including real ordered substreams, cancellation, blob streaming, relay-only connection, rendezvous, malformed frames, total deadline, and post-acceptance idle timeout.
- Remaining before completion: physical two-peer first-token trace, cross-peer cancellation/capacity proof, 100+ stream resource test, slow-client measurement, and documented v0.1 encounter on real nodes.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
