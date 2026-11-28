# HTTP API reference

`lucidd` exposes two HTTP surfaces on loopback (`127.0.0.1:11434` by default for CLI; `127.0.0.1:11435` for LUCID Desktop's managed sidecar):

1. **Ollama-compatible inference API (`/api/*`)** — loopback-only, unauthenticated for compatibility with existing Ollama clients.
2. **LUCID private control API (`/api/lucid/*`)** — loopback-only, requires `Authorization: Bearer <64-hex-token>` (verified in constant time) and returns `Cache-Control: no-store`.

## Global transport and security guards

- **Loopback bind enforcement:** `lucidd` refuses non-loopback `LUCIDD_HOST` values at startup (only `127.0.0.1` and `::1`).
- **DNS-rebinding `Host` guard:** Missing, duplicated, or non-loopback `Host` headers (or URI authorities) return `403 Forbidden`. Allowed hosts: `localhost`, `127.0.0.1`, `[::1]` (with optional `:port`).
- **Loopback CORS:** Allows `http`/`https` origins on `localhost`/`127.0.0.1`/`::1`/`[::1]`, methods `GET`/`POST`, request headers `Content-Type`, `Authorization`, `x-lucid-local-only`, and exposes `x-phase-worker`, `x-lucid-routed-via`, `x-lucid-route-explanation`, `x-lucid-receipt-verified`, `x-lucid-trust-label`, `x-phase-receipt`.
- **Default request body cap:** `1 MiB` (`1,048,576` bytes); tighter or larger per-route caps are listed below.

## Custom HTTP headers and trust labels

### Request headers

| Header | Values | Effect |
|---|---|---|
| `X-Lucid-Local-Only` | `1`, `true`, `yes`, `on` (case-insensitive) | Refuses remote peer routing **and** refuses local RPC-offloaded (`trusted-circle-sharded`) execution (`503`). |
| `Authorization` | `Bearer <64-hex-token>` | Required on all `/api/lucid/*` control endpoints (`401` on failure). |

### Response headers, HTTP trailers, and terminal NDJSON fields

| Header / Field | Where Sent | Values & Meaning |
|---|---|---|
| `X-Lucid-Routed-Via` | Response header | `"local"`, `"peer"`, or `"trusted-circle-sharded"` (omitted on refusal; never leaks a peer ID). |
| `X-Lucid-Route-Explanation` | Response header | Sanitized ASCII routing summary (≤192 B; alphanumeric runs >32 chars redacted to `[redacted]`). |
| `X-Lucid-Receipt-Verified` | Response header & terminal NDJSON (`x_lucid_receipt_verified`) | `"pending"` on initial peer stream headers; final verdict `"true"`, `"false"`, or `"unverifiable"` (omitted on local path). |
| `X-Lucid-Trust-Label` | Initial header, HTTP trailer (`Trailer: x-lucid-trust-label`), & terminal NDJSON (`x_lucid_trust_label`) | One of the five canonical `TrustLabel` values below. |
| `X-Phase-Worker` | Streaming `/api/chat` & `/api/generate` | `"lucidd"`. |
| `X-Phase-Receipt` | Non-streaming `/api/chat`, `/api/generate`, `/api/embed`, `/api/embeddings` | Base64-encoded 32-byte `output_commitment`. |
| `x_phase_commitment` | Terminal NDJSON on streaming `/api/chat` & `/api/generate` | 64-hex SHA-256 streaming output commitment. |

### The five `X-Lucid-Trust-Label` values

1. `local` — executed locally on this machine without RPC offload.
2. `remote-pending` — initial header on a streaming peer job before terminal receipt verification completes.
3. `remote-receipt-verified` — terminal Ed25519 `SignedReceipt` verified against manifest hash, delivering `PeerId`, and replayed chunk commitment.
4. `remote-spot-checked` — receipt verified **and** a deterministic redundant spot-check peer agreed on the commitment.
5. `trusted-circle-sharded` — executed across opted-in Tier-1 Trusted-Circle GPUs (RPC offload or `/api/lucid/cluster/execute`).

## Ollama-compatible endpoints

| Method & Path | Request Shape & Limits | Response Shape |
|---|---|---|
| `GET /` | None | `200 text/plain`: `"lucidd ready: see /api/version"` |
| `GET /api/version` | None | `200`: `{"version": "<crate-version>"}` |
| `GET /api/tags` | None | `200`: `{"models": [{name, model, modified_at, size?, digest ("sha256:<cid>"), details: {format?, family, families, quantization_level}, x_lucid_access? ("authorized"\|"access_not_confirmed")}]}` |
| `POST /api/show` | `{model?, name?}` | `200`: `{details, capabilities, model_info: {"phase.model_cid", "phase.backend", "phase.context_length", "phase.max_concurrent", "phase.advertised_at", "phase.valid_until", "phase.verification"?, "phase.size_bytes"?, "phase.installed_at"?}}`. `404` unknown, `409` ambiguous alias. |
| `GET /api/ps` | None | `200`: `{"models": [{name, model, digest?}]}`. Reports only live, health-verified models; omits unknown `size`/`size_vram`/`expires_at`. |
| `POST /api/chat` | `{model, messages?: [{role, content?}], stream? (default true), options?}` (`deny_unknown_fields`). Rejects (`400`) `keep_alive`, `format`, `tools`, `images`, invalid roles (only `system\|user\|assistant\|tool`), >262,144 chars. | Streaming (`application/x-ndjson`): delta lines `{model, created_at, message: {role:"assistant", content}, done: false}`, terminal line `{..., done: true, done_reason, total_duration, prompt_eval_count, eval_count, eval_duration, x_lucid_receipt_verified?, x_phase_commitment?, x_lucid_trust_label?}` + trailer. Non-streaming (`stream: false`): single JSON object. |
| `POST /api/generate` | `{model, prompt?, stream? (default true), options?}`. Rejects (`400`) `system`, `keep_alive`, `raw`, `suffix`, `images`, `template`, >262,144 chars. | Same streaming/non-streaming contract as `/api/chat`, with `"response"` and `"context": []`. |
| `POST /api/embed` | `{model, input?: string \| string[]}`. Rejects (`400`) `truncate`, `dimensions`, `keep_alive`, `options`, >128 inputs, >262,144 total chars. Non-streaming only. | `200`: `{"model", "embeddings": [[f32, ...], ...]}` + `X-Phase-Receipt`. |
| `POST /api/embeddings` | `{model, prompt?}`. Non-streaming only. | `200`: `{"model", "embedding": [f32, ...]}` + `X-Phase-Receipt`. |
| `POST /api/pull` | `{model?, name?, stream? (default true), cid? (64-hex), publisher? (PeerId)}` | Imports local `<model_dir>/<name>.gguf` if present & unpinned, else verified P2P pull. Streaming NDJSON emits `resolving`, `selecting_provider`, `downloading`, `verifying`, `installing`, `registering`, `success`/`error`. |

### Allowed `options` on `/api/chat` and `/api/generate`

Only the following keys are permitted inside `options` (unknown keys return `400 Bad Request`):

- `num_predict` (`1..=8192`, default `512`, clamped by `policy.toml` `max_tokens_ceiling`)
- `temperature`, `top_p`, `top_k`, `min_p`, `seed`, `stop`, `repeat_penalty` / `repetition_penalty`

## LUCID private control endpoints (`/api/lucid/*`)

All routes below require `Authorization: Bearer <token>` and return `Cache-Control: no-store`. Request structs enforce `deny_unknown_fields`.

### Status, discovery, and models

| Method & Path | Cap | Request | Response |
|---|---|---|---|
| `GET /api/lucid/status` | — | None | `{state ("connected"\|"connecting"\|"offline"), peer_count, active_path, model_count, enrollment: {state, message}, onboarding_completed, providing_enabled, cluster_enabled, providing_models, paused_reason, policy}` |
| `GET /api/lucid/peers` | — | None | `{peer_count, active_path, nat, peers: [{peer_id, path, authorized}]}` |
| `GET /api/lucid/probe` | — | None | `SystemProbe` (`{hardware, existing_inference, runtime, discovered_models, onboarding_completed, providing_enabled}`) |
| `POST /api/lucid/probe/refresh` | `1 KiB` | Empty | Fresh `SystemProbe` |
| `GET /api/lucid/models` | — | None | `{models: [{id, name, cid, source ("local"\|"ollama"\|"lmstudio"\|"network"), size_bytes, quantization, peer_count, on_device, available_locally, providing, can_request, fits_hardware, local_path}], model_dirs, providing_enabled, onboarding_completed}` |
| `GET /api/lucid/runtime` | — | None | `{hardware, runtime}` (pinned `llama.cpp b12651` status) |

### Permission-gated mutations and settings

#### `POST /api/lucid/provide`
- **Cap:** `4 KiB` · **Gate:** requires `confirm_permission: true` when `enable: true` (else `428 Precondition Required`).
- **Request:** `{model_name, enable, confirm_permission?, make_available_locally?, local_path?}`
- **Response:** `{providing_enabled, approved_provide_models, imported_cid, runtime_ready, restart_required}`

#### `POST /api/lucid/pull`
- **Cap:** `4 KiB`
- **Request:** `{model, cid?, publisher?}`
- **Response:** `200`: `{status: "installed", model_id, cid, size_bytes}` (`503` if content plane disabled, `502` on transfer failure).

#### `POST /api/lucid/folders`
- **Cap:** `4 KiB`
- **Request:** `{action: "add" | "remove", path}` (runs `validate_model_dir` on add)
- **Response:** `{model_dirs, probe}`

#### `POST /api/lucid/settings`
- **Cap:** `4 KiB`
- **Request:** `{onboarding_completed?, manual_pause?, auto_pause_on_battery?, thermal_limit_c?: f32 | null (40..=105), serve_model_content?, cluster_enabled?}`
- **Response:** `{onboarding_completed, serve_model_content, cluster_enabled, policy: {manual_pause, auto_pause_on_battery, thermal_limit_c}}`

#### `POST /api/lucid/runtime/setup`
- **Cap:** `4 KiB` · **Gate:** requires `confirm_permission: true` (else `428`).
- **Request:** `{action: "pin_detected" | "download_pinned", confirm_permission?}`
- **Response:** `{detail, runtime}` (serialized via `runtime_lock`; persists `llama_server_binary` and `llama_server_sha256` to `node.toml`).

### Circle enrollment and invitations

#### `POST /api/lucid/invitations`
- **Cap:** `1 KiB`
- **Request:** `{ttl_seconds? (default 900, range 1..=3600)}`
- **Response:** `201 Created`: `{invitation ("lucid://join/..."), expires_at, grant_expires_at}` (`409 Conflict` if node has neither local models nor `shard_stage_bridge_port`).

#### `POST /api/lucid/enroll`
- **Cap:** `16 KiB`
- **Request:** `{invitation}`
- **Response:** `202 Accepted`: `{state, message}` (`400` on invalid/expired link).

### Tier-1 Trusted-Circle cluster endpoints

#### `GET /api/lucid/cluster`
- **Response:** `{cluster_enabled, shard_stage_bridge_configured, trust_tier: "trusted-circle", trust_label: "trusted-circle-sharded", untrusted_sharding_allowed: false, local_node, connected_peers, models}`

#### `POST /api/lucid/cluster/plan`
- **Cap:** `16 KiB` · **Gate:** rejects non-`trusted-circle` tiers and unverified `model_path` with `403 Forbidden`.
- **Request:** `{model_name, model_path?, model_cid_hex?, total_layers?, size_bytes?, trust_tier?, peers: [PeerShardCapacity]}`
- **Response:** `{verified: true, required_memory_bytes, signed_plan}`

#### `POST /api/lucid/cluster/execute`
- **Cap:** `4 MiB` · **Gate:** requires live `cluster_enabled == true` and `trust_tier == "trusted-circle"` (else `403`).
- **Request:** `{job_id?, signed_plan, input_base64, per_stage_timeout_ms? (default 30000, clamped 500..=60000)}`
- **Response:** `{verified: true, trust_label, job_id, output_base64, verification, receipts}` + `X-Lucid-Trust-Label: trusted-circle-sharded`.

## Phase Artifact Server HTTP API (`phase-artifact-server`)

Standalone content-addressed HTTP/1.1 & HTTP/2 server (plaintext, unauthenticated; designed for LAN/trusted artifact distribution and Phase Boot):

| Route | Purpose |
|---|---|
| `GET /`, `/health`, `/status` | Service health and store summary |
| `GET /manifest.json` | Store manifest |
| `GET /blobs/:prefix/:filename` | Immutable content-addressed blob with `Range: bytes=start-end` / `206 Partial Content` and `X-Artifact-Hash` |
| `GET /:channel/:arch/manifest.json`, `/:channel/:arch/:artifact` | Channel/architecture Phase Boot images with HTTP `Range` support |
