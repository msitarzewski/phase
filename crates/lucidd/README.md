# lucidd

The **LUCID inference daemon** — the open GPU-inference flagship built on the Phase substrate. `lucidd` presents the Ollama API on `:11434`, runs inference and embeddings through pluggable workers, routes each request locally or across the Phase DHT, and returns cryptographically verifiable signed receipts.

**Current checkout: LUCID v0.2 implementation checkpoint, unreleased (reviewed 2026-11-07).** Phase Core and LUCID v0.1/v0.1.1 are shipped history. Features below describe this checkout; real cross-network/NAT, large pull/resume, MLX and the remaining release matrix still require acceptance. See the [authoritative tracker](../../memory-bank/releases/lucid-v0.2/README.md#progress-tracker).

License: **AGPL-3.0-or-later** (the flagship is copyleft so no one can fork it closed). Depends on `phase-net`, `phase-identity`, `phase-manifest`, `phase-receipt`, and `phase-protocol`.

---

## HTTP API (`:11434`, Ollama-compatible)

Point an Ollama-compatible client at `http://localhost:11434`. The supported endpoint/option subset is listed below; qualify the actual client rather than assuming complete upstream API parity.

| Endpoint | Shape | Purpose |
|---|---|---|
| `POST /api/chat` | NDJSON stream | Chat inference (load-bearing path) |
| `POST /api/generate` | NDJSON stream | Single-prompt completion |
| `POST /api/embed` | JSON | Embedding vectors; `input` is a string or array → `embeddings` |
| `POST /api/embeddings` | JSON | Legacy singular-`prompt` embedding → `embedding` |
| `POST /api/pull` | JSON / NDJSON | Resolve a signed alias and download/resume verified model content; content-plane configuration is required |
| `GET /api/tags` | JSON | List local models |
| `POST /api/show` | JSON | Verified installed/loaded model metadata; unknown models fail explicitly |
| `GET /api/version` | JSON | Capability sniff |

**Response headers:**
- `X-Lucid-Routed-Via` — `local` or `peer:<short-id>` (where the job actually ran)
- `X-Lucid-Receipt-Verified` — `pending` for a live remote stream; `true` / `false` / `unverifiable` where a final verdict is available. The live stream carries its final `x_lucid_receipt_verified` verdict in-band; the initial header is not proof of success.
- `X-Phase-Receipt` — base64 commitment (non-streaming paths)

The Ollama HTTP surface is **unauthenticated** and binds `127.0.0.1` by default. Binding a non-loopback address logs a loud warning — put an authenticating reverse proxy in front if you expose it.

---

## Workers

- **`llama-cpp`** (`--worker llama-cpp`) — real inference + embeddings by managing `llama-server` subprocesses (one per loaded model; any of llama.cpp's Metal/CUDA/ROCm/Vulkan backends). Streams tokens over `POST /completion`; embeddings via a separate `--embeddings` instance over `POST /embedding`. Crash supervision (restart with backoff, 3-crash/60s circuit-break), `/health` polling, per-request hang detection, and LRU model eviction at the resident-model cap.
- **`echo`** (`--worker echo`) — no GPU, no model. Reverses the last message (chat) and emits deterministic SHA-256-seeded vectors (embeddings). Explicit development/CI/demo selection; it cannot count as real-model acceptance.
- **`mlx`** (`--worker mlx`) — experimental macOS ARM64 adapter for a pinned `mlx_lm.server` entry point and immutable canonical model bundle. Inference only; real hardware acceptance remains open. See the [MLX contract](../../memory-bank/releases/lucid-v0.2/mlx-backend.md).

The llama.cpp and echo workers serve inference and embeddings; MLX serves inference. Workers sign receipts with the node identity, so peer-served receipts bind to the serving node's PeerId. Worker mode requires an explicit `--worker` selection.

**Research backends:** `CoreAIWorker` (Apple Neural Engine) and `ShardWorker` have no production implementation. Sharding is gated on the independent whole-model baseline and partial-compute verification. See [decisions.md](../../memory-bank/decisions.md).

---

## Routing & verification

Per-request decision order (`router.rs`):

1. `local-only` requested but model not loaded locally → **refuse** (privacy posture).
2. Operator policy (`should_serve_self`) pauses → **refuse** with a structured reason.
3. Model loaded locally → **serve local**.
4. Resolve the signed model alias to an exact CID and discover eligible serving peers → **live relay**, with ordered fallback peers for failover before any output begins. v1 batch compatibility is explicit; it cannot count as live-stream acceptance.
5. Nobody serves it → **refuse**.

**Inbound relay (serving side)** verifies the manifest, requires an operator-authorized signer **and** binds that signer to the delivering libp2p PeerId (`src/router.rs:1903-1930`). Identity binding does not authorize a stranger. Token budgets outside the configured ceiling are rejected; prompt and concurrency bounds apply before dispatch. The explicit `allow_unauthenticated_jobs = true` escape hatch bypasses authorization for isolated development; it is not the public-network default.

**Receipt verify+bind (SEC-05)** — a peer-served `SignedReceipt` is checked four ways before it is trusted: signature, `job_id` bind to the dispatched manifest, `worker_pubkey → delivering PeerId` bind, and a commitment replay over the received chunks.

These checks establish attributable transcript integrity. They do not establish that the claimed model, backend or precision was used. Local reputation and deterministic redundant comparisons provide limited observations; the independent correctness-evidence validator remains unimplemented. Never treat a routing header or two agreeing peers as proof of correct model execution.

---

## Operator policy (`~/.config/lucidd/policy.toml`)

Hot-reloadable (file watcher + `SIGHUP`). Governs what this node serves **to peers** — the operator's own requests on `:11434` always run, gated only by `manual_pause`.

- `serve_models` — glob allowlist of model ids (`["*"]` = all)
- `auto_pause_on_battery`, `auto_pause_on_thermal_threshold_c`, `time_of_day_window`, `max_concurrent_remote_jobs` — donation-protection auto-pauses
- `authorized_submitters` — signer allowlist (empty = default-deny)
- `allow_unauthenticated_jobs` — insecure escape hatch (default false)
- `manual_pause`, `max_tokens_ceiling`

---

## Model registry

Loaded models are advertised on the Kademlia DHT under `b"phase/model/" || model_cid` as postcard-encoded, Ed25519-signed records. Production GGUF CIDs derive from verified bytes; MLX CIDs derive from the canonical bundle. Signed alias/provider records bind human names and transfer sources to content identities, with expiry and durable rollback/equivocation protection. Ambiguous aliases require explicit selection rather than an arbitrary winner. The echo entry uses an explicitly developmental name hash and has no transferable model artifact.

`--serve-model-content` opts into serving verified blobs; installing a model does not automatically donate upload bandwidth. `/api/pull` verifies bytes before atomic installation and catalog registration. Large physical transfer/resume acceptance remains open.

---

## CLI

```
--mode {worker|consume-only|infrastructure}  worker (default), requester, or bounded public relay
--worker {echo|llama-cpp|mlx}    explicit worker choice, required in worker mode
--no-local-worker                consume-only (alias for --mode relay)
--model-dir <PATH>               directory of .gguf files (required with llama-cpp)
--artifact-dir <PATH>            verified content store (separate from source models)
--serve-model-content            opt into uploading verified model blobs
--llama-server-binary <PATH>     llama-server path (resolved absolute at startup; SEC-04)
--llama-n-gpu-layers <INT>       GPU layers (default -1 = all)
--llama-ctx-size <USIZE>         context window (default 8192)
--policy-config <PATH>           policy.toml override
--identity-path <PATH>           persistent libp2p identity (default ~/.config/phase/identity.key)
--libp2p-port <U16>              listen port (default 0 = ephemeral; set e.g. 4001 for WAN)
--bootstrap-peer <MULTIADDR>     dial on startup (repeatable)
--relay-peer <MULTIADDR>         peer-pinned relay candidate (repeatable)
--bootstrap-dns <DOMAIN>         TXT-record bootstrap, PeerID-pinned (repeatable; SEC-09)
--no-default-bootstrap           disable the foundation domain for isolated/offline tests
--no-bootstrap-relays            use only explicitly configured relay candidates
--external-address <MULTIADDR>   public address (required in infrastructure mode)
--evidence-store <PATH>          observer-local bounded execution-evidence store
--redundant-verification-permille <U16>  deterministic spot-check rate (default 0)
--dns-fallback                   opt into public DNS resolvers (default off, fail-closed)
```

Env: `LUCIDD_PORT` (default 11434), `LUCIDD_HOST` (default 127.0.0.1).

---

## Remaining acceptance and trust limits

- Live relay, content pull and reachability components are implemented; the complete physical-network acceptance matrix is not.
- Public rendezvous serving is disabled pending hard registration quotas.
- Default authorization remains deny-by-default; reputation does not open the node.
- Receipt integrity and redundant agreement do not prove correct inference. Independent validation requires a separate method decision and evidence.
- Encrypted transport protects the route; the serving operator can see plaintext prompts. Local-only requests stay local.
- The experimental MLX adapter has not passed real-device acceptance; ShardWorker is research-blocked.

See the [v0.2 release tracker](../../memory-bank/releases/lucid-v0.2/README.md), [physical test runbook](../../memory-bank/releases/lucid-v0.2/intended-stream-vertical-slice.md), and [architectural record](../../memory-bank/decisions.md).
