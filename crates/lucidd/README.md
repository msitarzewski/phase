# lucidd

The **LUCID inference daemon** — the open GPU-inference flagship built on the Phase substrate. `lucidd` presents the Ollama API on `:11434`, runs inference and embeddings through pluggable workers, routes each request locally or across the Phase DHT, and returns cryptographically verifiable signed receipts.

License: **AGPL-3.0-or-later** (the flagship is copyleft so no one can fork it closed). Depends on `phase-net`, `phase-identity`, `phase-manifest`, `phase-receipt`, and `phase-protocol`.

---

## HTTP API (`:11434`, Ollama-compatible)

Point any Ollama client (Open WebUI, Continue, Cursor, LangChain, `curl`) at `http://localhost:11434` — no modification required.

| Endpoint | Shape | Purpose |
|---|---|---|
| `POST /api/chat` | NDJSON stream | Chat inference (load-bearing path) |
| `POST /api/generate` | NDJSON stream | Single-prompt completion |
| `POST /api/embed` | JSON | Embedding vectors; `input` is a string or array → `embeddings` |
| `POST /api/embeddings` | JSON | Legacy singular-`prompt` embedding → `embedding` |
| `POST /api/pull` | JSON / NDJSON | **v0.1.1 stub** — registers an already-present local GGUF (no network download) |
| `GET /api/tags` | JSON | List local models |
| `POST /api/show` | JSON | Model metadata stub |
| `GET /api/version` | JSON | Capability sniff |

**Response headers:**
- `X-Lucid-Routed-Via` — `local` or `peer:<short-id>` (where the job actually ran)
- `X-Lucid-Receipt-Verified` — `true` / `false` / `unverifiable` (peer-served receipt verdict; SEC-05)
- `X-Phase-Receipt` — base64 commitment (non-streaming paths)

The Ollama HTTP surface is **unauthenticated** and binds `127.0.0.1` by default. Binding a non-loopback address logs a loud warning — put an authenticating reverse proxy in front if you expose it.

---

## Workers

- **`llama-cpp`** (`--worker llama-cpp`) — real inference + embeddings by managing `llama-server` subprocesses (one per loaded model; any of llama.cpp's Metal/CUDA/ROCm/Vulkan backends). Streams tokens over `POST /completion`; embeddings via a separate `--embeddings` instance over `POST /embedding`. Crash supervision (restart with backoff, 3-crash/60s circuit-break), `/health` polling, per-request hang detection, and LRU model eviction at the resident-model cap.
- **`echo`** (`--worker echo`, default) — no GPU, no model. Reverses the last message (chat) and emits deterministic SHA-256-seeded vectors (embeddings). For dev, CI, and demos.

Both workers serve `JobSpecKind::{Inference, Embedding}` and sign receipts with the node identity, so peer-served receipts bind to the serving node's PeerId.

**Deferred backends (roadmap):** `MlxWorker` (Apple Silicon via `mlx-lm`, LUCID M3) and a future `CoreAIWorker` (Apple Neural Engine). See the repo [decisions.md](../../memory-bank/decisions.md).

---

## Routing & verification

Per-request decision order (`router.rs`):

1. `local-only` requested but model not loaded locally → **refuse** (privacy posture).
2. Operator policy (`should_serve_self`) pauses → **refuse** with a structured reason.
3. Model loaded locally → **serve local**.
4. DHT lookup finds peers → **relay to the first**, with the rest carried as `fallback_peers` for **multi-peer failover**.
5. Nobody serves it → **refuse**.

**Inbound relay (serving side)** verifies the manifest signature, then authorizes via the operator **allowlist** *or* the **SEC-06 PeerID-bind** (a peer signing with the same identity it dials from is trusted to spend its own work). Resource caps (`max_tokens`, prompt length, concurrency) are clamped server-side before dispatch. The escape hatch `allow_unauthenticated_jobs = true` restores open behavior for local dev.

**Receipt verify+bind (SEC-05)** — a peer-served `SignedReceipt` is checked four ways before it is trusted: signature, `job_id` bind to the dispatched manifest, `worker_pubkey → delivering PeerId` bind, and a commitment replay over the received chunks.

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

Loaded models are advertised on the Kademlia DHT under `b"phase/model/" || model_cid` as a postcard-encoded, Ed25519-signed `SignedModelAdvertisement` (schema v2), refreshed on a 5-minute cadence. Name→CID resolves via the local loaded set, falling back to `ModelCid::from_model_id` (SHA-256 with domain separation) so two peers compute the same CID for the same name without coordinating. **Real content-hashed CIDs (from `/api/pull` verification) land in v0.2.**

---

## CLI

```
--mode {worker|relay}            worker (default) or consume-only relay node
--worker {echo|llama-cpp}        which worker to expose (default echo)
--no-local-worker                consume-only (alias for --mode relay)
--model-dir <PATH>               directory of .gguf files (required with llama-cpp)
--llama-server-binary <PATH>     llama-server path (resolved absolute at startup; SEC-04)
--llama-n-gpu-layers <INT>       GPU layers (default -1 = all)
--llama-ctx-size <USIZE>         context window (default 8192)
--policy-config <PATH>           policy.toml override
--identity-path <PATH>           persistent libp2p identity (default ~/.config/phase/identity.key)
--libp2p-port <U16>              listen port (default 0 = ephemeral; set e.g. 4001 for WAN)
--bootstrap-peer <MULTIADDR>     dial on startup (repeatable)
--bootstrap-dns <DOMAIN>         TXT-record bootstrap, PeerID-pinned (repeatable; SEC-09)
--dns-fallback                   opt into public DNS resolvers (default off, fail-closed)
```

Env: `LUCIDD_PORT` (default 11434), `LUCIDD_HOST` (default 127.0.0.1).

---

## v0.1 limitations (roadmap, not bugs)

- Peer-relay is **batch-shaped**; token streaming over the relay is v0.2.
- `/api/pull` registers an *already-present* local GGUF; network download + content-hashed CIDs are v0.2.
- Cross-peer `name → cid` resolution uses the deterministic placeholder; a verified content-hashed index is v0.2.
- Peer-receipt verification is the v0.1 "friend's GPU" posture (verdict surfaced, tokens not failed on mismatch); reputation-based hardening is v0.2.
- The libp2p circuit-relay server + DCUtR hole-punching (full NAT traversal) is v0.2.

See [memory-bank/releases/lucid/](../../memory-bank/releases/lucid/) for the release plan and [memory-bank/decisions.md](../../memory-bank/decisions.md) for the architectural record.
