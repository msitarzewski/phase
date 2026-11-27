# lucidd

The **LUCID inference daemon** — the open GPU-inference flagship built on the Phase substrate. `lucidd` presents the Ollama API on `:11434`, runs inference and embeddings through pluggable workers, routes each request locally or across the Phase DHT, and returns cryptographically verifiable signed receipts.

**Current checkout: LUCID v0.2 active physical qualification (`v0.2.0-rc8+`, reviewed 2026-11-27).** Phase Core and LUCID v0.1/v0.1.1 are shipped history. Physical 5-node fleet tests (`Runs #1–#12`: `120/120` verified Ed25519 receipts, `10.18 GB` P2P pull/resume, `71.3 tok/s` cellular+ProtonVPN MLX, `98.0 tok/s` 2-Mac burst, and `5.7 → 16.4 → 21.3 tok/s` real sliced 2-Mac `gemma-4-12B`) are recorded in [fleet-progress-report.md](../../memory-bank/releases/lucid-v0.2/fleet-progress-report.md). See the [authoritative tracker](../../memory-bank/releases/lucid-v0.2/README.md#progress-tracker) and [multi-agent sprint plan](../../memory-bank/releases/lucid-v0.2/multi-agent-sprint-plan.md).

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
| `GET /api/tags` | JSON | List local and cached/discovered remote models |
| `POST /api/show` | JSON | Verified installed/loaded/remote model metadata; unknown models fail explicitly |
| `GET /api/ps` | JSON | Honest empty list until runtime residency telemetry is wired |
| `GET /api/version` | JSON | Capability sniff |
| `GET /api/lucid/status` | JSON | Bearer-authenticated local node/enrollment/policy status |
| `GET /api/lucid/probe` | JSON | Read-only hardware, GPU backend, runtime, & local GGUF discovery |
| `GET /api/lucid/models` | JSON | Combined local + discovered network models (`peer_count`, CID, capabilities) |
| `POST /api/lucid/provide` | JSON | Permission-gated (`confirm_permission: true`) model serving opt-in & Ollama/LM-Studio hot-import |
| `POST /api/lucid/pull` | JSON | Permission-gated P2P verified model download/resume |
| `GET /api/lucid/peers` | JSON | Connected libp2p peers & active transport paths (`direct` / `relayed`) |
| `POST /api/lucid/settings` | JSON | Hot-update `policy.toml` / `node.toml` (`manual_pause`, `thermal_limit_c`, `cluster_enabled`, etc.) |
| `GET /api/lucid/runtime` | JSON | Inspect pinned `llama.cpp b12651` runtime status |
| `POST /api/lucid/runtime/setup` | JSON | Permission-gated SHA-256-verified `llama.cpp b12651` download or pin |
| `POST /api/lucid/enroll` | JSON | Bearer-authenticated opaque network invitation enrollment |
| `POST /api/lucid/invitations` | JSON | Bearer-authenticated one-time invitation creation on a serving or shard-worker node |
| `GET /api/lucid/cluster` | JSON | Tier-1 Trusted-Circle cluster status, `cluster_enabled`, loopback bridge status, & inspectable GGUF layer counts |
| `POST /api/lucid/cluster/plan` | JSON | Generate & sign a Tier-1 `SignedShardPartitionPlan` (`UntrustedSpotChecked` fails closed with `403`) |
| `POST /api/lucid/cluster/execute` | JSON | Opt-in (`cluster_enabled = true`) cross-machine `/phase/shard-stage/1.0.0` execution returning chained Ed25519 stage receipts |

**Response headers:**
- `X-Lucid-Routed-Via` — `local` or `peer:<short-id>` (where the job actually ran)
- `X-Lucid-Receipt-Verified` — `pending` for a live remote stream; `true` / `false` / `unverifiable` where a final verdict is available. The live stream carries its final `x_lucid_receipt_verified` verdict in-band; the initial header is not proof of success.
- `X-Phase-Receipt` — base64 commitment (non-streaming paths)

The Ollama HTTP surface is **unauthenticated** and binds `127.0.0.1` by default. Binding a non-loopback address logs a loud warning — put an authenticating reverse proxy in front if you expose it.

---

## Workers

- **`llama-cpp`** (`--worker llama-cpp`) — real inference + embeddings by managing `llama-server` subprocesses (Metal, CUDA, ROCm, Vulkan, CPU) with per-spawn `LLAMA_API_KEY`, `--cors-origins localhost --no-cors-credentials --no-webui --no-slots`, cross-tenant KV-cache isolation (`--no-cache-prompt --cache-ram 0 --cache-reuse 0` + `cache_prompt: false`), `--jinja`, honest resident-model reporting for `/api/ps`, crash supervision, `/health` polling, per-request hang kill + RAII port release, LRU eviction, Ollama CUDA-12 library auto-discovery, and opt-in `cluster_enabled`-gated Tier-1 Trusted-Circle sliced-layer offload via `--llama-rpc-peers` (native Noise-encrypted `/phase/shard-rpc/1.0.0` libp2p tunnel; refuses loads/respawns if a configured peer device fails to attach rather than silently falling back locally) or loopback-only `--llama-rpc-servers` + `--llama-tensor-split` *(note: `llama.cpp` indexes remote RPC devices before local GPUs in `--tensor-split`)*. On contributor nodes with a SHA-256-pinned `ggml-rpc-server` alongside `llama-server`, enabling `cluster_enabled = true` (off by default) automatically manages a strictly loopback `ggml-rpc-server` behind `/phase/shard-rpc/1.0.0` for explicitly enrolled peers only and kills/reaps it on opt-out or daemon exit (including a pipe-guarded teardown on `SIGKILL`).
- **`echo`** (`--worker echo`) — no GPU, no model. Reverses the last message (chat) and emits deterministic SHA-256-seeded vectors (embeddings). Explicit development/CI/demo selection; it cannot count as real-model acceptance.
- **`mlx`** (`--worker mlx`) — macOS ARM64 adapter for a pinned `mlx_lm.server` entry point and immutable canonical model bundle (supports `mlx-lm 0.32.x` `delta.reasoning`, hardware-accelerated `ring` SHA-256 hashing, and `SIGTERM`/`SIGINT` graceful child reaping). Physically verified on Apple M5 Max (`66.7 tok/s` local, `71.3 tok/s` over Cellular+ProtonVPN → Cloud Relay → Ubuntu Desktop). See [mlx-backend.md](../../memory-bank/releases/lucid-v0.2/mlx-backend.md).

The llama.cpp and echo workers serve inference and embeddings; MLX serves inference. Workers sign receipts with the node identity, so peer-served receipts bind to the serving node's PeerId.

**Tier-1 `ShardWorker` & Research Backends:** Tier-1 Trusted-Circle sharding (`trusted-circle-sharded`) is physically verified both at the protocol/chained-receipt layer (`/phase/shard-stage/1.0.0`, `POST /api/lucid/cluster/execute`) and with real sliced multi-Mac GGUF inference (`--llama-rpc-servers` + `ggml-rpc-server`, `135.6 GiB` pooled Metal 4, `5.7 → 16.4 → 21.3 tok/s`). Tier-2 `UntrustedSpotChecked` remains strictly fail-closed pending partial-compute verification ADR approval; native libp2p `/phase/shard-rpc/1.0.0` tunneling is the next implementation milestone. `CoreAIWorker` (Apple Neural Engine) remains a non-blocking research gate. See [shardworker-adr.md](../../memory-bank/releases/lucid-v0.2/shardworker-adr.md) and [decisions.md](../../memory-bank/decisions.md).

---

## Routing & verification

Per-request decision order (`router.rs`):

1. `local-only` requested but model not loaded locally → **refuse** (privacy posture).
2. Operator policy (`should_serve_self`) pauses → **refuse** with a structured reason.
3. Model loaded locally → **serve local**.
4. Resolve the signed model alias to an exact CID and discover eligible serving peers → **live relay**, with ordered fallback peers for failover before any output begins. v1 batch compatibility is explicit; it cannot count as live-stream acceptance.
5. Nobody serves it → **refuse**.

**Inbound relay (serving side)** verifies the manifest, requires an operator-authorized signer **and** binds that signer to the delivering libp2p PeerId (`src/router/mod.rs`). Identity binding does not authorize a stranger. Token budgets outside the configured ceiling are rejected; prompt and concurrency bounds apply before dispatch. The explicit `allow_unauthenticated_jobs = true` escape hatch bypasses authorization for isolated development; it is not the public-network default.

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
--llama-rpc-servers <ADDR,...>   loopback-only (`127.0.0.1:PORT`) `ggml-rpc-server` endpoints for Tier-1 layer slicing
--llama-tensor-split <W0,W1,...> GPU layer split ratios (note: in llama.cpp, RPC0 precedes local MTL0)
--shard-stage-bridge-port <U16>  loopback port (`127.0.0.1:PORT`) for opt-in `/phase/shard-stage/1.0.0` stage runner
--mlx-server-binary <PATH>       pinned `mlx_lm.server` entry point (required with `--worker mlx`)
--mlx-model-bundle <PATH>        read-only canonical MLX model bundle (required with `--worker mlx`)
--mlx-model-alias <ALIAS>        canonical alias for MLX bundle (required with `--worker mlx`)
--mlx-server-port <U16>          loopback port for `mlx_lm.server` (default 0 = ephemeral)
--policy-config <PATH>           policy.toml override
--node-config <PATH>             node.toml override
--control-token-file <PATH>      private bearer-token file for `/api/lucid/*` local control endpoints
--identity-path <PATH>           persistent libp2p identity (default ~/.config/phase/identity.key)
--libp2p-port <U16>              listen port (default 0 = ephemeral; set e.g. 4001 for WAN)
--listen-address <MULTIADDR>     explicit direct listen multiaddr (repeatable)
--outbound-only                  outbound + reserved relay circuits only (no direct listeners/mDNS/DCUtR)
--bootstrap-peer <MULTIADDR>     dial on startup (repeatable)
--relay-peer <MULTIADDR>         peer-pinned relay candidate (repeatable)
--bootstrap-dns <DOMAIN>         TXT-record bootstrap, PeerID-pinned (repeatable; SEC-09)
--no-default-bootstrap           disable the foundation domain for isolated/offline tests
--no-bootstrap-relays            use only explicitly configured relay candidates
--external-address <MULTIADDR>   public address (required in infrastructure mode)
--evidence-store <PATH>          observer-local bounded execution-evidence store
--pin-peer <PEER_ID>             locally pin a serving peer (repeatable)
--block-peer <PEER_ID>           locally block a serving peer (repeatable)
--redundant-verification-permille <U16>  deterministic spot-check rate (default 0)
--dns-fallback                   opt into public DNS resolvers (default off, fail-closed)
```

Subcommands: `lucidd probe`, `lucidd models`, `lucidd peers`, `lucidd invite`, `lucidd join <URI>`, `lucidd bench`, `lucidd cluster {status,inspect,plan}`.
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

---

## Release Signing (Maintainers)

No signing credentials, Apple IDs, Team IDs, or Azure identifiers live in this repository.

- **Windows (`lucidd.exe`, Azure Artifact Signing — CI & local)**:
  Configure secrets in the GitHub **`release`** environment (restricted to release tags / maintainer approval):
  - `LUCID_AZURE_SIGN_ENDPOINT`, `LUCID_AZURE_SIGN_ACCOUNT`, `LUCID_AZURE_SIGN_PROFILE`
  - Auth via GitHub OIDC (`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` — no client secret) **or** Service Principal (`AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_CLIENT_SECRET`).
  - The same script runs locally in Windows with `az login`:
    `pwsh -NoProfile -File scripts/sign-windows-binary.ps1 -Path target\x86_64-pc-windows-msvc\release\lucidd.exe`
- **macOS (`lucidd` CLI, `LUCID.app`, `.dmg`, Apple Developer ID + notarization — local Keychain only)**:
  CI intentionally holds zero Apple credentials. After a tagged release builds, sign, notarize, staple, verify, and republish from a Mac holding the Developer ID certificate and a `notarytool` Keychain profile:
  ```bash
  # Keep personal IDs outside the repo, e.g. in ~/.config/lucid/signing.env:
  #   export LUCID_APPLE_TEAM_ID="<10-char-team-id>"
  #   export LUCID_APPLE_NOTARY_PROFILE="<keychain-profile>"
  scripts/sign-macos-release.sh --from-release vX.Y.Z --dry-run
  scripts/sign-macos-release.sh --from-release vX.Y.Z --publish vX.Y.Z
  ```
