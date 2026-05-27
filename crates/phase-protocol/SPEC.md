# phase-protocol — Worker Trait Specification

**Status:** Proposed — subject to validation against `LlamaCppWorker` (LUCID M2) and `WasmtimeWorker` (phase-core M7) before the trait is frozen.
**Version:** v1 (commitment domain string `phase-protocol:v1:commitment`)
**Owners:** Michael S.

---

## 1. Scope

`phase-protocol` defines a single trait — `Worker` — plus the types that flow through it. Every Phase node implementation impls this trait. That includes Plasm's `WasmtimeWorker`, LUCID's `LlamaCppWorker` and `MlxWorker`, and every future workload type (image gen, fine-tune, embeddings, scientific compute).

The trait is the seam between the network layer (`phase-net`, `phase-manifest`, `phase-receipt`) and any specific backend. Get this right and adding LUCID is "implement a trait." Get it wrong and every workload type drags its own scheduling, signing, cancellation, and resumption code into the protocol layer.

This document is normative. Code that contradicts it should be considered buggy.

---

## 2. The trait

```rust
pub trait Worker: Send + Sync {
    fn supported_kinds(&self) -> &[JobSpecKind];
    fn capacity_hint(&self) -> usize { 1 }

    fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> impl Future<Output = Result<(JobHandle, JobStream), WorkerError>> + Send;
}
```

`JobStream` is `Pin<Box<dyn Stream<Item = JobEvent> + Send + 'static>>`. `JobEvent` is:

```rust
pub enum JobEvent {
    Output(OutputChunk),       // partial result, folded into the commitment
    Progress(ProgressUpdate),  // informational, NOT signed
    Final { result: JobResult, error: Option<String> },  // terminal
}
```

The trait has **one method**. Streaming is universal — batch jobs are streams of length one. Cancellation, signing, and resumption all live on the `JobHandle` and `JobStream` types, not on additional trait methods. New workload types add `JobSpec` variants and `OutputChunk` kinds; the trait itself does not grow.

---

## 3. Lifecycle

```
caller (api/router)                       worker
─────────────────────                     ──────
SignedManifest<JobSpec> ───execute()────►
                            ◄─── (JobHandle, JobStream)
                                            │
                            ◄─── JobEvent::Progress(…)         (any time, optional)
                            ◄─── JobEvent::Output(chunk_0)     (commitment update)
                            ◄─── JobEvent::Output(chunk_1)
                                  …
                            ◄─── JobEvent::Final { result, .. } (stream ends)

handle.finish().await   ─────────────────►
                            ◄─── SignedReceipt<JobResult>
```

**Invariants:**

- `execute()` returns once the job is *accepted* — model loaded into a slot, WASM module compiled — not once it's complete.
- The stream emits zero or more `Output` / `Progress` events, then exactly one `Final`, then ends.
- Every `Final` is mirrored by a `SignedReceipt<JobResult>` delivered through `JobHandle::finish()`. The stream version is unsigned (for fast client-facing emission); the receipt is the signed authority.

---

## 4. Cancellation semantics

The client (`lucidd::api::ollama`) can drop the HTTP connection mid-stream. The worker must release KV-cache slots, kill subprocesses, and stop billing the GPU cycles immediately.

Mechanism: dropping the `JobStream` half signals cancellation via the `JobHandle`'s embedded `watch` channel. Workers `select!` between their next-token future and `producer.cancelled()`:

```rust
loop {
    tokio::select! {
        _ = producer.cancelled() => {
            // Flush cancellation Final, release slot, return.
            yield JobEvent::Final {
                result: build_partial_result(Completion::Cancelled, &acc),
                error: None,
            };
            return;
        }
        tok = decode_next_token() => { ... }
    }
}
```

**Contract:**

1. Worker MUST observe cancellation within one token's worth of latency (≤ ~50ms for typical inference).
2. Worker MUST emit `JobEvent::Final { completion: Completion::Cancelled, .. }` before terminating the stream — partial results stay verifiable.
3. Worker MUST deliver a signed receipt for the partial run through `JobHandle::finish()`. Cancellation is not failure; it's an early `Stop` with a different completion code.
4. Worker MUST release any KV-cache slot, subprocess, or GPU memory associated with the job before the stream's `Drop` impl returns.

**Why not just rely on `Stream::Drop`?** Because cancellation has to be observable from inside the worker's token loop. `Drop` on the stream signals the watch channel; the watch channel is what the loop polls. Without an explicit cancellation primitive, workers would have to use `try_send` heuristics or detect closed channels on the next emit — both racy and both worse for tail latency.

**Why have BOTH `JobHandle::cancel()` and stream `Drop`?** Stream `Drop` covers the common case (HTTP client disconnects, caller is gone). Explicit `cancel()` covers structured cases (router policy timeout, model unload eviction, "local-only" mode tripping mid-stream).

---

## 5. KV-cache contract (conversation continuity)

Inference workers maintain an attention KV cache across tokens. A multi-turn chat should reuse the cache from turn N when serving turn N+1 — otherwise every turn pays a full prefill cost.

**Mechanism:** the worker returns a `ConversationToken` in the `Final` event's `JobResult.resumption`. The token contains:

```rust
pub struct ConversationToken {
    pub issuer: PeerId,         // who can resume this
    pub state: Bytes,           // opaque, worker-defined (≤ 256 bytes)
    pub valid_until_unix_ms: u64,
}
```

The client (or LUCID's API translator) stores this and passes it back in the next turn's `InferenceJobSpec.resume_from`. The scheduler routes the follow-up turn to `issuer` when possible (see `should_resume_on_same_peer`). If it cannot — peer offline, token expired — it routes anywhere and the worker silently cold-starts; the next token is just slower, never wrong.

**Worker obligations:**

1. The `state` bytes are opaque to the protocol. For llama.cpp, recommended encoding is `(slot_id || sha256(prompt_prefix) || nonce)`. For MLX, a session UUID. For backends without KV reuse (some scientific workers), `state` can be empty and `valid_until_unix_ms` zero — clients see no cache benefit but the contract still holds.
2. Workers SHOULD hold the cache state for at least `DEFAULT_RESUMPTION_GRACE` (5 minutes) after emitting a token. Workers MAY evict earlier under memory pressure — clients always see at-most-best-effort reuse.
3. On resumption, the worker MUST verify the prefix hash in the token matches the conversation history in the new `InferenceJobSpec.messages`. Mismatch → cold-start (don't return wrong cached attention from a different conversation).

**Why opaque bytes and not a structured field?** Because the protocol can't know what state is meaningful. llama.cpp slot IDs aren't meaningful to MLX; an MLX session UUID isn't meaningful to a future MoE-shard worker. Opaque bytes let each backend evolve its cache encoding without protocol churn. The protocol only enforces size and lifetime.

**Why a `valid_until` rather than a TTL?** Wall-clock absolute time is unambiguous across the network. TTLs accumulate drift across hops (DHT lookup → router → worker is multiple async steps). Absolute time + NTP-loose-sync is good enough at the 5-minute granularity that matters here.

---

## 6. Signing strategy

We picked: **streamable-verifiable Merkle accumulation with a single final Ed25519 signature.**

### What the worker does

1. Initialize a `CommitmentAccumulator` (SHA-256 chain, domain-separated by `"phase-protocol:v1:commitment"`).
2. For each `OutputChunk` emitted on the stream, fold the chunk into the accumulator: `state_n = SHA256(state_{n-1} || seq || len(kind) || kind || len(data) || data)`.
3. At end-of-stream, populate `JobResult.output_commitment = state_final` and `JobResult.output_chunk_count = chunks`.
4. Sign the canonical CBOR encoding of `JobResult` with the worker's Ed25519 key. This is the `SignedReceipt<JobResult>`.

The worker emits exactly **one Ed25519 signature per job**, regardless of stream length.

### What the verifier does

A verifier receives `(JobSpec, [chunks…], SignedReceipt<JobResult>)`:

1. Verify the Ed25519 signature on `SignedReceipt<JobResult>` against the issuer's public key.
2. Replay the chunks through a fresh `CommitmentAccumulator`, in `seq` order.
3. Compare `acc.finalize()` to `receipt.payload.output_commitment` and `receipt.payload.output_chunk_count`. Mismatch → tampered or truncated.
4. Verify `receipt.payload.job_spec_hash` matches the hash of the manifest the caller submitted.

If all four pass, the receipt cryptographically commits the worker to the exact bytes of the stream. Any attempt to add, drop, reorder, or modify a chunk invalidates the commitment.

### Why this and not alternatives

| Alternative | Rejected because |
|---|---|
| **Per-chunk Ed25519 receipts** | At 30 tok/s × ~100 concurrent streams, 3000 sigs/s/peer of pure overhead. Burns CPU that should be serving inference. Reasonable for batch jobs, ruinous for streaming. |
| **Final-only signature, no commitment** | Worker could lie about which chunks it emitted vs which the verifier received. We need *the verifier* to be able to rebuild the signed bytes from the on-wire stream. |
| **Merkle tree (binary, balanced)** | More work than a chain for no benefit; clients don't selectively reveal chunks, they replay the whole stream. The chain is the linearised special case. |
| **Pedersen commitment / KZG** | Real cryptography overkill. We're not hiding chunk content from the verifier; we're proving the worker committed to the bytes. SHA-256 is plenty. |
| **Both per-chunk + final** | Hybrid was considered. Adds per-chunk verification (chunk-N is independently verifiable), but doubles signing cost and doesn't deliver any property a verifier replaying the full stream couldn't already check. Defer to v2 if a future use case demands it. |

### What's signed, exactly

The signing message is the canonical CBOR encoding of `JobResult` — which contains `job_spec_hash`, `output_commitment`, `output_chunk_count`, `completion`, `resumption` (optional), and `metrics`. The signature is over the whole serialised structure.

`metrics` is signed but the verifier explicitly does NOT trust it for correctness — it's worker-attested telemetry, useful for reputation but never load-bearing for "did this run produce these bytes."

`ProgressUpdate` events are NOT in the commitment. They're informational only. Dropping them or fabricating them doesn't change anything verifiable. Workers can use them freely for UX (queue position, prompt-eval %, time-to-first-token) without worrying about signing overhead.

---

## 7. Implementation guide (read this if you're writing a Worker)

### Minimum-viable impl

```rust
impl Worker for MyWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        &[JobSpecKind::Inference]
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        // 1. Validate kind.
        let JobSpec::Inference(spec) = &job.payload else {
            return Err(WorkerError::Unsupported { kind: job.payload.kind() });
        };

        // 2. Acquire a slot (subprocess, GPU memory, etc.).
        let slot = self.acquire_slot(spec).await
            .map_err(|e| WorkerError::Capacity)?;

        // 3. Build the handle + producer.
        let job_id = JobId(job.manifest_hash);
        let (handle, producer) = JobHandle::new(job_id);
        let manifest_hash = job.manifest_hash;
        let signer = self.signer.clone();

        // 4. Spawn the stream.
        let stream = async_stream::stream! {
            let mut acc = CommitmentAccumulator::new();
            let mut producer = producer;
            let mut seq = 0u64;

            loop {
                tokio::select! {
                    _ = producer.cancelled() => {
                        finish(producer, signer, &acc, manifest_hash,
                               Completion::Cancelled, None).await;
                        return;
                    }
                    tok = slot.next_token() => {
                        match tok {
                            Some(text) => {
                                let chunk = OutputChunk {
                                    kind: "token".into(),
                                    data: text.into(),
                                    seq,
                                };
                                acc.update(&chunk);
                                seq += 1;
                                yield JobEvent::Output(chunk);
                            }
                            None => {
                                let result = finish(/* ... */).await;
                                yield JobEvent::Final { result, error: None };
                                return;
                            }
                        }
                    }
                }
            }
        };

        Ok((handle, Box::pin(stream)))
    }
}
```

### Checklist for a Worker impl

- [ ] `supported_kinds()` is exhaustive and stable across instances.
- [ ] `execute()` returns within `<1s` for dispatch-time decisions; long work happens in the stream.
- [ ] Token loop selects on `producer.cancelled()` AND token-decode future.
- [ ] Every code path that ends the stream emits exactly one `JobEvent::Final`.
- [ ] Every code path that emits `Final` also calls `producer.deliver_receipt(...)`.
- [ ] Commitment is updated for every `OutputChunk`, in `seq` order, with monotonically increasing `seq`.
- [ ] `ProgressUpdate` is NOT folded into the commitment.
- [ ] Cancellation releases resources before `Drop` returns.
- [ ] If the backend supports it, a `ConversationToken` is emitted in `JobResult.resumption` for inference workloads.

---

## 8. Rationale (the alternatives we rejected)

### 8.1 Streaming-unified API vs. two methods (`run` + `stream`)

**Considered:** Separate `async fn run() -> Result<JobResult>` for batch and `fn stream() -> JobStream` for inference.

**Rejected because:** Every concern that matters — cancellation, signing, resumption, metrics, error propagation — has to work the same way in both, and duplicating the contract for the two methods means we'd inevitably let them drift. A batch WASM job IS just a stream of one chunk and one Final; expressing it that way costs ~zero (one extra `yield` in the implementation) and means there's one cancellation path, one signing strategy, one receipt shape. The Ollama API translator was the deciding factor: it has to emit NDJSON streaming chunks for `/api/chat` AND single JSON responses for `/api/generate` with `stream: false`. If we want one worker code path to serve both, the worker must always stream and the translator decides whether to emit progressively or buffer-and-emit-once.

### 8.2 Native `async fn` in trait vs. `#[async_trait]`

**Considered:** `#[async_trait]` for object-safety + universal trait object support.

**Rejected because:** Rust 1.88 (libp2p 0.57 MSRV, our floor) supports native `async fn` in trait. The hidden allocation per call in `#[async_trait]` is measurable in hot dispatch paths. The cost of native: the trait isn't dyn-compatible out of the box. The workaround is a thin `DynWorker` shim that boxes the future at the registry boundary — written once, then everyone downstream depends on `Worker` directly. Net win.

### 8.3 Opaque `ConversationToken` vs. structured KV-cache descriptor

**Considered:** A typed `KvCacheRef { model_cid, slot_id, prefix_hash, …}` so any worker could in principle resume on any other worker's state.

**Rejected because:** Cross-worker cache resumption is a fantasy. llama.cpp slot state isn't portable to MLX, and even llama.cpp's KV cache isn't portable across processes without bespoke serialisation that no upstream supports. Pretending otherwise would force every worker to expose internals it doesn't have. Opaque bytes let each backend evolve its cache encoding privately, and the protocol enforces only the contract that matters: "if you resume the same conversation on the same peer within 5 minutes, you get cache reuse; otherwise you get a cold start and correctness is preserved."

### 8.4 Per-chunk signing vs. single final signature over a commitment

**Rejected per-chunk** for performance reasons documented in §6. The streamable-commitment approach gives a verifier everything per-chunk signing would (tamper detection, truncation detection, reorder detection) at one signature per job instead of one per token.

### 8.5 `JobSpec` as enum vs. `JobSpec` as trait object

**Considered:** `Box<dyn Job>` so new workload types could be added by downstream crates without modifying the protocol.

**Rejected because:** `SignedManifest<JobSpec>` is the on-wire payload. A peer receiving a manifest has to deserialise it from bytes — which requires a closed set of recognised variants at the protocol-version level. A trait object can't be deserialised; the wire format wouldn't be well-defined. The enum + `#[non_exhaustive]` + minor-version bump per new variant is the honest version of "extensible."

### 8.6 `OutputChunk { kind: String, data: Bytes }` vs. typed per-workload chunks

**Considered:** `enum OutputChunk { Token(String), ImageTile { x, y, png: Bytes }, ProgressLog { line: String }, … }`.

**Rejected because:** This is the future-proofing exit. The trait has to accept workloads we haven't designed yet. An enum variant per chunk kind means every new workload is a breaking change to `phase-protocol`. The `kind: String + data: Bytes` design means new workloads add new strings and bytes interpretations without touching the protocol crate at all. Strings are reserved at the spec level (`"token"`, `"image_tile"`, `"progress_log"`, etc.) to prevent collisions, but the type system doesn't enforce them. Cost: workers have to range-check `kind` themselves. Benefit: image generation lands as a `JobSpec::ImageGen` variant + a new `"image_tile"` chunk kind, with zero changes to the trait. Worth it.

---

## 9. Open questions

1. **Multi-modal inference inputs (images, audio).** Currently `InferenceJobSpec.messages[].images` carries base64-encoded image data inline. Large images bloat the manifest. Future: carry a CID and have the worker fetch from `phase-artifact-server`. Doesn't change the trait, but the manifest grows a "referenced artifacts" field. Defer to LUCID M4 when vision models land.

2. **Tool calling round-trips.** Ollama's `/api/chat` supports tool-use roundtrips inside a single response. The worker emits a `tool_call` token, the API translator pauses streaming, executes the tool client-side, and resumes with `role: tool`. The protocol surface here is: how does the worker emit "I want to call a tool" vs. "here's a text token"? Working assumption: `OutputChunk { kind: "tool_call", data: <serialised call> }` and the translator handles the rest. Needs validation against a real client (Continue or Open WebUI doing function calling).

3. **Speculative decoding receipts.** If a worker uses `--spec-draft-model`, the draft tokens are technically the work of the draft model, not the main model. Reputation-wise this MIGHT matter. Working assumption: it doesn't — the receipt commits to the bytes emitted, regardless of which internal pathway produced them. Revisit if any user actually asks.

4. **Receipt batching.** A worker serving 100 concurrent streams could theoretically batch the Ed25519 signatures via Ed25519's batch verification trick. We have not implemented this; the current trait emits one signed receipt per job. Future optimisation; doesn't change the trait surface.

5. **`JobHandle::Clone` + `finish()` interaction.** Current design: only one caller can `finish()`; later callers get `WorkerError::Dropped`. Is this the right ergonomics? Alternative: broadcast the receipt to all clones. Defer until a real router actually needs multiple-waiter semantics.

---

## 10. Versioning

The commitment domain string is `phase-protocol:v1:commitment`. Any change to the commitment construction (hash function, byte layout, domain string) is a protocol version bump. Workers running v1 and verifiers running v2 will detect mismatch via the domain separator and refuse to verify.

The `JobSpec` enum is `#[non_exhaustive]`. New variants are minor-version changes (consumers with wildcard arms keep compiling). Removing a variant or changing the wire encoding of an existing variant is a major-version change.

Field additions to `JobResult` and `JobSpec` sub-structs are minor as long as new fields are `Option<_>` or `#[serde(default)]`. Removing fields is major.

---

## 11. References

- `crates/phase-protocol/src/worker.rs` — normative type surface
- `crates/phase-protocol/src/job_spec.rs` — workload payload types
- `crates/phase-protocol/src/commitment.rs` — `CommitmentAccumulator` + tests
- `memory-bank/releases/phase-core/README.md` § "The Generic Worker Trait" — release-level scoping
- `memory-bank/releases/lucid/README.md` — the inference workload that drove this design
- `memory-bank/releases/phase-core/research-brief.md` — libp2p 0.57 / Ollama / llama-server constraints
