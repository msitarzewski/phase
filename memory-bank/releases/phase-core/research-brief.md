# Phase + LUCID v0.1 — 2026 Stack Research Brief

**Date:** 2026-05-27
**Author:** Research pass for phase-core / LUCID kickoff
**Scope:** rust-libp2p current stable, Ollama HTTP API surface, llama.cpp `llama-server` subprocess interface
**Method:** WebSearch + WebFetch against GitHub repos, official docs, crates.io, and docs.rs. Each version number cross-checked across at least two sources.

---

## 1. rust-libp2p

### Target versions

| Crate | Target version | Notes |
| --- | --- | --- |
| `libp2p` (umbrella) | **0.57.0** | Latest entry in [libp2p/CHANGELOG.md](https://github.com/libp2p/rust-libp2p/blob/master/libp2p/CHANGELOG.md). docs.rs landing page was still showing 0.56.0 when this brief was written — see Gotchas. |
| `libp2p-swarm` | 0.48.0 | |
| `libp2p-kad` | 0.49.0 | |
| `libp2p-quic` | 0.14.0 | |
| `libp2p-noise` | 0.47.0 | |
| `libp2p-identify` | 0.48.0 | |
| `libp2p-swarm-derive` | matches swarm 0.48 line | |
| **MSRV** | **Rust 1.88.0** | Raised in 0.57.0 / subcrates' x.y.0 cuts. |

We were on `libp2p` 0.54 (Nov 2025); the jump to 0.57 spans three minor versions.

### Breaking changes that matter to our Nov 2025 code

#### 0.55.0
- MSRV raised to **1.83.0**.
- `SwarmBuilder` gained `with_connection_timeout(...)` (replaces the old per-topology setting).
- Idle-connection-timeout default became **10s** (was previously the much shorter old default — confirm what you had hardcoded; behavior of long-lived gossipsub/kad streams may visibly change).
- `ConnectionHandler::{InboundOpenInfo, OutboundOpenInfo}` deprecated (then un-deprecated in 0.47 swarm). Don't lean on these.

#### 0.56.0
- **`async-std` support removed** from `libp2p`, `libp2p-swarm`, `libp2p-quic`, TCP, DNS. Tokio-only world now. Our code is tokio-based, so this is a clarification, not work — but any transitive crate still importing async-std variants will fail to compile.
- Deprecated `Transport::with_bandwidth_logging` and `SwarmBuilder::with_bandwidth_logging` **removed**.
- `libp2p-peer-store` introduced; `libp2p-webrtc-websys` now feature-gated.

#### 0.57.0 (current stable)
- MSRV **1.88.0**. Set `rust-toolchain.toml` accordingly.
- `wasm-bindgen` feature **removed**; wasm support is implicit on `wasm32-*` targets.
- **Protobuf backend reverted from `quick-protobuf` back to `prost`** across `libp2p-kad`, `libp2p-noise`, `libp2p-identify`. If anything in our tree pinned `quick-protobuf` to match libp2p's wire format, drop that pin.
- Metrics delegation for gossipsub fixed.

#### Kademlia (`libp2p-kad`) deltas worth flagging
- `record` module is **private** now (was previously "deprecated but accessible"); imports must come from `kad::` directly.
- `kad::Behaviour` is the canonical name; old `Kademlia` type aliases are gone.
- `ProtocolConfig::default()` removed — you must construct it explicitly with your protocol name.
- `RecordStore` uses GATs; the lifetime parameter is gone.
- Provider records are no longer auto-cached on `get_record` — call `put_record_to()` if you want caching.
- Query event model: `OutboundQueryProgressed` (multiple, streaming) instead of one terminal `OutboundQueryCompleted`. Queries that you want to short-circuit must call `.finish()`.
- `GetRecordError::QuorumFailed` removed in 0.49.

#### Identify
- 0.46: `identify::Config` fields private, use getters; new `hide_listen_addrs` option (security knob — set it on workers we don't want to gossip routes for).
- 0.46: Server validates that announced public key matches the peer ID before accepting `Info` — **this can drop peers that previously connected**, especially mismatched test rigs.

#### Noise
- 0.47: MSRV bump only; no API break.

#### QUIC
- 0.13: async-std gone, `Config::support_draft_29` deprecated.
- 0.10.2: max idle timeout dropped to 10s. Background streams need keepalives or you'll see surprise teardowns.
- Quinn upgraded to 0.11 in the 0.10.3 line. If we pin `quinn` directly anywhere, must align.

### Reference SwarmBuilder snippet (0.57 form)

```rust
use libp2p::{
    identity, kad, noise, ping, swarm::NetworkBehaviour, tcp, yamux, SwarmBuilder,
};
use std::time::Duration;

#[derive(NetworkBehaviour)]
struct PhaseBehaviour {
    kad: kad::Behaviour<kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
    ping: ping::Behaviour,
}

let id_keys = identity::Keypair::generate_ed25519();
let local_peer_id = id_keys.public().to_peer_id();

let mut swarm = SwarmBuilder::with_existing_identity(id_keys)
    .with_tokio()
    .with_tcp(
        tcp::Config::default().nodelay(true),
        noise::Config::new,
        yamux::Config::default,
    )?
    .with_quic()
    .with_dns()?
    .with_behaviour(|key| PhaseBehaviour {
        kad: kad::Behaviour::new(
            local_peer_id,
            kad::store::MemoryStore::new(local_peer_id),
        ),
        identify: libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new(
                "/phase/1.0.0".to_string(),
                key.public(),
            )
            .with_hide_listen_addrs(false),
        ),
        ping: ping::Behaviour::default(),
    })?
    .with_swarm_config(|c| {
        c.with_idle_connection_timeout(Duration::from_secs(60))
            .with_connection_timeout(Duration::from_secs(20))
    })
    .build();
```

### Known regressions / stability issues to avoid

- Some users have reported flakiness around AutoNAT in mixed-version networks during 0.55 → 0.56 transitions. If a peer in our DHT is on 0.54 and we're on 0.57, identify-version-skew warnings will spam logs but should be benign.
- 0.57 `wasm-bindgen` removal silently breaks projects that still set `features = ["wasm-bindgen"]` on the umbrella — you'll get an unknown feature warning, not an error, so it can slip past CI.
- gossipsub metrics were broken pre-0.57; if we plan to scrape them, ensure 0.57.

### Sources

- [rust-libp2p releases](https://github.com/libp2p/rust-libp2p/releases)
- [libp2p umbrella CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/libp2p/CHANGELOG.md)
- [libp2p-swarm CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/swarm/CHANGELOG.md)
- [libp2p-kad CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/protocols/kad/CHANGELOG.md)
- [libp2p-quic CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/transports/quic/CHANGELOG.md)
- [libp2p-noise CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/transports/noise/CHANGELOG.md)
- [libp2p-identify CHANGELOG](https://github.com/libp2p/rust-libp2p/blob/master/protocols/identify/CHANGELOG.md)
- [crates.io: libp2p](https://crates.io/crates/libp2p)
- [docs.rs: libp2p](https://docs.rs/libp2p)

### Gotchas

1. **docs.rs is lagging.** docs.rs landed 0.56.0 as the "latest" page even though crate metadata is 0.57.0 — use the CHANGELOG and crates.io, not docs.rs, for the canonical version.
2. **MSRV 1.88.0** — must bump `rust-toolchain.toml` and any CI matrix. Old stable channels (1.86 etc.) will fail to compile.
3. **`Kademlia` type alias is gone.** Every `use libp2p::kad::Kademlia` must become `use libp2p::kad::Behaviour as KadBehaviour` (or unqualified `kad::Behaviour`).
4. **Provider records no longer cached on `get_record`.** If we built our model registry assuming a node that runs a successful GET will hold a cached copy, that's no longer true — call `put_record_to()` explicitly.
5. **Query events streamed.** Anything matching on `OutboundQueryCompleted` is dead code; switch to `OutboundQueryProgressed` and handle the streaming result set.
6. **QUIC idle timeout 10s default.** Long-idle worker connections need application-level keepalive (gossipsub heartbeat covers this for the mesh, but bespoke streams won't be).
7. **`identify::Config` fields private** — refactor any direct field access to use builder methods.

---

## 2. Ollama HTTP API (2026)

### Target version

| Component | Version | Notes |
| --- | --- | --- |
| Ollama server | **0.24.x** (current stable, May 2026) | v0.30.x is on the `rc` channel — the architecture shift to call llama.cpp directly is happening there. We target the 0.24 line for compatibility with deployed Continue / Open WebUI / Zed clients. |
| Default port | 11434 | |
| OpenAI compat base | `/v1/...` | |
| Native base | `/api/...` | |

### Surface area to implement for LUCID's `:11434` server

#### Native endpoints

**`POST /api/chat`** — Multi-turn chat. Streaming default.

```bash
curl http://localhost:11434/api/chat -d '{
  "model": "llama3.2",
  "messages": [{"role": "user", "content": "why is the sky blue?"}]
}'
```

Streaming wire format — NDJSON, `Content-Type: application/x-ndjson`, HTTP chunked transfer:

```
{"model":"llama3.2","created_at":"2026-05-27T17:15:24.097767Z","message":{"role":"assistant","content":"The"},"done":false}
{"model":"llama3.2","created_at":"2026-05-27T17:15:24.109172Z","message":{"role":"assistant","content":" sky"},"done":false}
{"model":"llama3.2","created_at":"2026-05-27T17:15:24.166576Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","total_duration":4883583458,"load_duration":1334875,"prompt_eval_count":26,"prompt_eval_duration":342546000,"eval_count":282,"eval_duration":4535599000}
```

Request schema fields: `model`, `messages[]` (role: `system|user|assistant|tool`, `content`, `images[]` base64, `tool_calls[]`, `tool_name`), `tools[]`, `format` (`"json"` or JSON Schema object), `options` (sampling params), `stream` (default `true`), `keep_alive` (default `5m`), `think` (reasoning models).

**`POST /api/generate`** — Single-turn completion. Same streaming format, field is `response` instead of `message`.

```bash
curl http://localhost:11434/api/generate -d '{"model":"llama3.2","prompt":"why is the sky blue?"}'
```

Streaming:
```
{"model":"llama3.2","created_at":"...","response":"The","done":false}
{"model":"llama3.2","created_at":"...","response":" sky","done":false}
{"model":"llama3.2","created_at":"...","response":"","done":true,"done_reason":"stop","context":[1,2,3],"total_duration":...,"eval_count":290}
```

Extras: `suffix` (fill-in-the-middle), `images[]`, `raw` (skip template), `system`, `template`, `keep_alive`. Generation-only knobs `width`/`height`/`steps` exist for image-gen models.

**`GET /api/tags`** — list local models.
```json
{"models":[{"name":"llama3.2:latest","model":"llama3.2:latest","modified_at":"2026-05-01T...","size":4661211808,"digest":"sha256:...","details":{"parent_model":"","format":"gguf","family":"llama","families":["llama"],"parameter_size":"3.2B","quantization_level":"Q4_K_M"}}]}
```

**`POST /api/embed`** — vector embeddings. Note: `/api/embeddings` (singular) is the legacy path; `/api/embed` is canonical from late-2024 onward. Both still work but **new clients should send `/api/embed`**.
```bash
curl http://localhost:11434/api/embed -d '{"model":"all-minilm","input":"why is the sky blue?"}'
```
Response: `{"model":"...","embeddings":[[...]],"total_duration":...,"load_duration":...,"prompt_eval_count":...}`. Supports `input: string | string[]`, `truncate`, `options`, `keep_alive`, `dimensions`.

**`POST /api/pull`** — download model, NDJSON progress stream:
```
{"status":"pulling manifest"}
{"status":"pulling 6a0746a1ec1a","digest":"sha256:...","total":2142590208,"completed":241970}
{"status":"verifying sha256 digest"}
{"status":"writing manifest"}
{"status":"removing any unused layers"}
{"status":"success"}
```

**`GET /api/show`** (also accepts `POST` with `{"model": "..."}`) — model metadata: `modelfile`, `parameters`, `template`, `details`, `model_info`, `capabilities[]` (e.g. `["completion","vision","tools","embedding"]`).

#### OpenAI-compatible endpoints (also on :11434)

`POST /v1/chat/completions`, `POST /v1/completions`, `POST /v1/embeddings`, `GET /v1/models`. Streaming uses **SSE** (`text/event-stream`, `data: {...}\n\n` framing) — different from the native NDJSON path.

Supports: streaming, JSON mode, tool calling (but **`tool_choice` is not supported**), vision via base64 (not URL), `seed` for reproducibility, reasoning control for thinking models.
Not supported: `logprobs`, `tool_choice`, stateful `/v1/responses`.

### Auth / version headers

- **Local 11434 has no auth by default.** Clients send `Authorization: Bearer <anything>` only because the OpenAI SDK requires a non-empty key; the server ignores it.
- `OLLAMA_API_KEY` env is for outbound calls to `ollama.com` and private registries, **not** an inbound bearer check.
- No `X-Ollama-Version` or similar versioning header. Clients sniff capabilities by calling `GET /api/version` (returns `{"version":"0.24.0"}`).
- `Content-Type: application/json` on requests. Responses: `application/x-ndjson` for native streaming, `text/event-stream` for `/v1/*` streaming, `application/json` for non-streaming.

### Client compatibility — has the API shifted recently?

The native API is **explicitly described as backwards-compatible** by Ollama and is not strictly versioned. Notable recent moves:

- v0.5.0 dropped the `context` field on `/api/chat` responses in favor of server-side session memory (already in effect; not new in 2026).
- v0.6.x (early 2026) added batch embeddings and the `think`/`thinking` field for reasoning models. **Older clients that don't strip unknown fields will see `thinking` blocks in `message.content`** — Open WebUI handles this, Continue and Zed do as of their current releases, but custom integrations should filter on `done_reason` and tolerate `message.thinking` separately.
- Open WebUI, Continue, Cursor, Zed, LangChain, opencode, and the `ollama` CLI all work against current 0.24.x without changes. The deeper architectural shift in v0.30-rc (direct llama.cpp embed instead of fork) does not change the wire format.

### Sources

- [Ollama API reference (docs.ollama.com)](https://docs.ollama.com/api/streaming)
- [GitHub: ollama/ollama/docs/api.md](https://github.com/ollama/ollama/blob/main/docs/api.md)
- [Ollama OpenAI-compat docs](https://docs.ollama.com/api/openai-compatibility)
- [DeepWiki: Ollama Generation and Chat API](https://deepwiki.com/ollama/ollama/3.2-generation-and-chat-api)
- [Ollama releases](https://github.com/ollama/ollama/releases)

### Gotchas

1. **Two streaming wire formats on the same port.** Native `/api/*` is NDJSON; OpenAI `/v1/*` is SSE. Our LUCID server must emit both correctly depending on the requested route — clients will hard-fail if you serve NDJSON over `/v1/chat/completions`.
2. **`/api/embed` vs `/api/embeddings`.** Both must be supported. Old LangChain versions still POST to `/api/embeddings` (plural). Response schemas differ slightly between them.
3. **`done_reason` is the terminator signal, not connection close.** Clients keep the connection open until they see `"done":true`. If our worker streams crash mid-token, we must inject a terminal `{"done":true,"done_reason":"error"}` chunk, or downstream clients will hang on the chunked transfer.
4. **`keep_alive` semantics drive memory.** Ollama unloads the model after `keep_alive` (default 5m) of inactivity. Clients can set `keep_alive: -1` to pin, or `0` to evict after the request. Mirror this — our DHT-routed model loader needs to honor it or hosts will OOM.
5. **No auth means anyone on the LAN can hit :11434.** LUCID must bind to localhost by default and require explicit policy to expose externally.
6. **`tool_choice` not supported** on the OpenAI-compat path — if a client requires forced tool selection (some agent frameworks do), they'll get unexpected free-form replies.

---

## 3. llama.cpp `llama-server`

### Target version

| Component | Version | Notes |
| --- | --- | --- |
| llama.cpp build | **b9360** (May 27, 2026) | Build cadence is multiple per day. Pin to a known build in our Cargo workspace; don't track HEAD. |
| Binary name | `llama-server` | Renamed from `server` in mid-2024. |
| Default port | 8080 | |
| Default host | 127.0.0.1 | |

### Starting it with a GGUF and GPU offload

```bash
llama-server \
  --model /var/lucid/models/llama-3.2-3b-instruct-q4_k_m.gguf \
  --host 127.0.0.1 \
  --port 18080 \
  --ctx-size 8192 \
  --n-gpu-layers all \
  --parallel 4 \
  --cont-batching \
  --jinja \
  --metrics \
  --api-key "$LUCID_LLAMA_KEY"
```

For embeddings-only mode, swap to:
```bash
llama-server --model nomic-embed-text-v1.5.Q4_K_M.gguf --embedding --port 18081
```

For an OpenAI-compatible reranker:
```bash
llama-server --model bge-reranker-v2-m3.Q4_K_M.gguf --reranking --port 18082
```

### Streaming format

**SSE.** Different from Ollama native (which is NDJSON), same as Ollama's `/v1/*` path. The native `/completion` endpoint streams:

```
data: {"content":"The","tokens":[464],"stop":false}

data: {"content":" sky","tokens":[6766],"stop":false}

data: {"content":"","tokens":[],"stop":true,"stop_type":"eos","timings":{...}}
```

Headers: `Content-Type: text/event-stream`, `Transfer-Encoding: chunked`, `Cache-Control: no-cache`, `Connection: keep-alive`. The `data:` prefix and double-newline framing are real SSE — clients must use an SSE parser, not a JSON-line splitter.

OpenAI-compat `/v1/chat/completions` streaming uses the standard OpenAI SSE format with `data: [DONE]` sentinel.

### CLI flags we care about

| Flag | Purpose |
| --- | --- |
| `-m, --model FNAME` | GGUF path |
| `-hf, --hf-repo user/model[:quant]` | Pull from HF directly |
| `-mm, --mmproj FILE` | Multimodal projector (vision) |
| `-c, --ctx-size N` | Context length (0 = read from model metadata) |
| `-n, --n-predict N` | Max generated tokens (-1 = unbounded) |
| `-b, --batch-size N` | Logical batch (default 2048) |
| `-ub, --ubatch-size N` | Physical batch (default 512) |
| `-ngl, --n-gpu-layers N` | Layers on GPU. Accepts `auto` or `all`. |
| `-dev, --device d1,d2` | Specific GPUs |
| `-ts, --tensor-split N0,N1` | Split across GPUs |
| `--host HOST` | Bind (defaults 127.0.0.1; UNIX socket if ends in `.sock`) |
| `--port PORT` | Listen port |
| `-np, --parallel N` | Concurrent request slots (-1 = auto from ctx) |
| `-to, --timeout N` | Read/write timeout, seconds (default 600) |
| `--api-key KEY` | Bearer auth, comma-separated allowed |
| `--jinja` | Enable Jinja chat templates (required for tool calling) |
| `--embedding` | Embeddings-only server mode |
| `--reranking` | Enable rerank endpoint |
| `-cb, --cont-batching` | Continuous batching (default on) |
| `--cache-prompt` | KV cache reuse across requests (default on) |
| `--metrics` | Expose `/metrics` (Prometheus) |
| `--slots` | Expose `/slots` (default on) |
| `--spec-draft-model FNAME` | Speculative-decoding draft model |
| `-lcs, --lookup-cache-static FNAME` | Static n-gram lookup decoding |
| `--no-models-autoload` | In router mode, require explicit load |

### HTTP endpoints

**Health & info**
- `GET /health` — `{"status":"ok"}` when ready; **HTTP 503 while the model is still loading**. Poll this on startup.
- `GET /v1/models` — OpenAI-compat model listing.
- `GET /props` — global props: model path, chat template, modalities.

**Generation**
- `POST /completion` — native; accepts string or token-array prompt; streams via `"stream": true`. Multimodal via `{"prompt_string":"...","multimodal_data":["base64..."]}`.
- `POST /v1/completions` — OpenAI compat.
- `POST /v1/chat/completions` — OpenAI compat (with tool use when `--jinja` is set; returns `reasoning_content` for reasoning models).
- `POST /v1/embeddings` — OpenAI compat embeddings (requires pooling ≠ none).
- `POST /v1/messages` — Anthropic Messages compat (incl. tool use).
- `POST /v1/responses` — converted to chat completions internally.
- `POST /embedding` — native embeddings (multimodal aware).
- `POST /reranking` (alias `/rerank`) — document reranking.
- `POST /infill` — code infilling (prefix/suffix).

**Utilities**
- `POST /tokenize`, `POST /detokenize`, `POST /apply-template`.

**Monitoring**
- `GET /slots` — per-slot state, throughput, current params.
- `GET /metrics` — Prometheus format (`--metrics` required).
- `POST /slots/{id}?action=save|restore|erase` — persist/clear KV cache.

**LoRA adapters**
- `GET /lora-adapters`, `POST /lora-adapters` (update scales).

### Failure modes

- **OOM at load** — process exits non-zero before binding the port. Detection: parent must wait for `/health` to return 200 within a timeout; treat connect-refused after N seconds as OOM.
- **Prompt overflows context** — response includes `"truncated": true`; tokens are discarded per `--keep` policy. The request does *not* fail.
- **No available slot** — request queues by default; if you'd rather see a 503, append `?fail_on_no_slot=1` to the URL.
- **Generation timeout** — controlled by `t_max_predict_ms` parameter (per-request). Stops at newline if exceeded.
- **Generic hang** — there's no liveness ping endpoint distinct from `/health`. Use `/slots` to confirm the worker is making progress (each slot exposes `is_processing` and `n_decoded`).
- **HTTP timeout** — the `-to` flag (default 600s) closes idle connections. Long-running streaming under this is fine; long *think* before first token can hit it on big models.

### OpenAI-compatible mode

Always on. `--api-key` (single key or comma-separated) enables Bearer auth identically to OpenAI:
```
Authorization: Bearer <key>
```
Without `--api-key`, the OpenAI-compat path is open. The `/v1/...` routes mirror OpenAI's API including SSE streaming with `data: [DONE]` terminator. Tool calling requires `--jinja`.

### Router mode (multi-model on one server)

Launch without `-m`:
```bash
llama-server --models-dir /var/lucid/models
```
Requests then carry `"model": "..."` in the JSON body (or `?model=` query). Models auto-load on first hit unless `--no-models-autoload`. **Useful for LUCID's M5/M6 (local-or-DHT router + model registry)** — one llama-server per host can lazily load whatever the DHT routes to it.

### Sources

- [llama.cpp tools/server README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)
- [llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases)
- [llama.cpp main README](https://github.com/ggml-org/llama.cpp)
- [Glukhov: llama.cpp quickstart](https://www.glukhov.org/llm-hosting/llama-cpp/)
- [Unsloth llama-server guide](https://unsloth.ai/docs/basics/inference-and-deployment/llama-server-and-openai-endpoint)

### Gotchas

1. **Streaming format is SSE, not NDJSON.** If LUCID translates llama-server output into the Ollama native `/api/chat` shape, we must rewrite frames: strip `data: ` prefix, parse JSON, drop the `[DONE]` sentinel, re-emit one NDJSON line per token with the Ollama field names (`message.content`, `done`, `done_reason`).
2. **`--jinja` is required for tool calling and modern chat templates.** Without it, chat-completion responses ignore tool definitions silently. Always set it.
3. **`/health` returns 503 during model load.** That's not an error — it means "loading." Our supervisor must distinguish 503 (wait) from connect-refused (process died).
4. **Default `--host` is 127.0.0.1.** Good for security; bad if you forget and try to reach it across a Docker boundary. Bind explicitly.
5. **`--n-gpu-layers all` is supported but model-dependent.** A model larger than VRAM with `all` will fall back to partial offload silently and run slowly. Watch the load-time log line `offloaded N/M layers to GPU`.
6. **No request body size cap by default** — a malicious client can send a huge prompt that blows the context. Pair with reverse proxy limits.
7. **Slot queueing is silent.** A request with no available slot will sit happily for minutes. For LUCID's per-request SLAs, set `?fail_on_no_slot=1` and let the router pick another worker.

---

## Cross-cutting notes

- **Streaming protocols are different across our three integrations.** libp2p is binary framed (yamux/quic substreams). Ollama native is NDJSON over chunked HTTP. Ollama OpenAI-compat and llama-server are SSE. LUCID's worker abstraction must paper over this — design the internal `Worker::infer()` trait around an async stream of structured `Token` events, and let each adapter (Ollama-native client, llama-server SSE client) do the format-specific parsing once at the edge.
- **MSRV 1.88 for libp2p forces the whole workspace** to that toolchain. Worth doing first in phase-core M1 so we don't paint ourselves into a corner.
- **Pin everything.** llama.cpp ships multiple builds per day, Ollama ships weekly. Pin to specific versions in CI; let dependabot propose bumps deliberately.

---

## Open questions / lower confidence

- **libp2p 0.57 patch versions:** The CHANGELOG shows 0.57.0; whether 0.57.1+ have shipped silently with bug fixes was not confirmable from docs.rs (still cached at 0.56). Check crates.io directly before pinning.
- **Ollama v0.30-rc architecture:** 0.30 reportedly embeds llama.cpp directly rather than forking. We do not target it for LUCID v0.1, but if it goes stable during our build window we should re-evaluate whether to skip ahead.
- **Quinn version in libp2p-quic 0.14:** The 0.10.3 entry mentions Quinn 0.11. Whether 0.14 has moved to Quinn 0.12 was not confirmed from primary sources.
- **NetworkBehaviour derive macro:** We did not find a CHANGELOG entry calling out a breaking change to the derive macro itself between 0.54 and 0.57, but third-party reports suggest the macro is slightly stricter about field visibility now. Worth a quick smoke compile after the upgrade rather than relying on docs.
