# Phase network protocols and wire formats

Every Phase node communicates over `libp2p 0.57` with Noise XX encryption (TCP and Circuit Relay v2) or TLS 1.3 (QUIC v1). The networking substrate (`crates/phase-net`) is completely workload-neutral: it registers seven custom `/phase/*` protocols alongside standard libp2p discovery and NAT traversal behaviours.

## Transport and swarm bounds (`phase-net`)

- **Transports:** TCP + Noise XX + Yamux, QUIC v1 (TLS 1.3), Circuit Relay v2 Client + Noise XX + Yamux.
- **Idle connection timeout:** `60s`.
- **Connection limits (`connection_limits::Behaviour`):** max `32` pending inbound, `32` pending outbound, `128` established inbound, `128` established outbound, `4` per peer, `256` total.
- **Standard libp2p behaviours:**
  - **Kademlia DHT:** Forced `KademliaMode::Server` with `MemoryStore`; key `1..=512 B`, value `1..=64 KiB`, `256` values / `4 MiB` per `get_record`, `64` providers per query, `32` concurrent provider ops, `10s` op timeout.
  - **Identify:** Protocol `/phase/reachability/1.0.0`, `agent_version = "phase-net/<version>"`, signed peer records enabled, filters observed addresses to reject unspecified/multicast/broadcast/zero-port/unix/memory addresses.
  - **Circuit Relay v2:** Client enabled by default; Server strictly restricted to `ReachabilityRole::Infrastructure` (defaults: `128` reservations / `2` per peer / `1h`; `256` circuits / `4` per peer / `2m` / `64 MiB`).
  - **DCUtR:** Direct hole-punching upgrade (disabled when `--outbound-only`).
  - **AutoNAT v2:** Client enabled by default (disabled when `--outbound-only`); Server restricted to `Infrastructure`.
  - **mDNS:** Local LAN peer discovery (disabled when `--outbound-only`).
  - **Rendezvous:** Client enabled (`discover` limit `1..=256`, `lucidd` namespace `"phase-lucid-workers"`); Server explicitly disabled (`None`) in `lucidd` infrastructure mode pending upstream registration-count quotas.

## Custom `/phase/*` libp2p protocols

| Protocol ID | Transport & Wire Framing | Size & Timeout Bounds | Admission & Auth Gates |
|---|---|---|---|
| `/phase/job-relay/2.0.0` | Raw `libp2p_stream`, 4-byte BE length-prefixed **postcard** frames (`JobRelayStreamOpen`, `JobRelayStreamFrame`, `JobRelayStreamControl`: `Cancel` / `ReceiptAck`); inner manifest/event/receipt are `serde_json`. | Open ≤ `256 KiB` (+1 KiB wire), Event ≤ `1 MiB` (+4 KiB wire), Receipt ≤ `256 KiB`, Reason ≤ `1 KiB`; idle `250ms..=60s` (default `30s`), deadline ≤ `15m`. | Shared stream pool (`64` global / `4` per peer). Fail-closed `JobRelayStreamValidator` enforces exact `job_id`, contiguous `sequence` from 0, `Event` → `Receipt` ordering. `lucidd` enforces `verify_for_remote_execution()`, `signer_pubkey → PeerId` binding, default-deny allowlist/enrollment, replay cache, token/prompt caps, and `PolicyEngine`. |
| `/phase/job-relay/1.0.0` | `request_response::cbor` outer (`JobRelayRequest` / `JobRelayResponse`); inner `SignedManifest<JobSpec>`, `Vec<JobEvent>`, `SignedReceipt<JobResult>` are `serde_json`. | Request ≤ `256 KiB`, response ≤ `8 MiB`, `5m` timeout. | Legacy batch compatibility. Shares `prepare_inbound_relay` authz, concurrency gate, and replay cache with v2. |
| `/phase/blob/1.0.0` | Raw `libp2p_stream`, 4-byte BE length-prefixed **postcard** frames (`BlobStreamRequest` → `Accepted` / `Rejected` / `Chunk*` / `Eof`). | Chunk `1..=64 KiB`, metadata ≤ `4 KiB`, idle `250ms..=60s` (default `30s`), deadline ≤ `1h`. | Stream pool (`64` / `4`) + fixed-window per-peer egress cap `16 MiB/s`. `BlobStreamValidator` enforces exact `content_id`, sequential cursor offsets, and `Eof` at `total_size`. `lucidd::ContentPlane` adds `BlobServeGate` (`4` global / `1` per stripe across 256 stripes), rejects non-empty metadata, and serves only verified installed CIDs when `--serve-model-content` is enabled. |
| `/phase/shard-stage/1.0.0` | Raw `libp2p_stream`, 4-byte BE length-prefixed **postcard** frames (`ShardStageRequest` → `ShardStageResponse`: `Completed` / `Rejected` / `Failed`); inner plan & receipt are `serde_json`. | Plan ≤ `256 KiB`, payload ≤ `8 MiB`, receipt ≤ `64 KiB`, reason ≤ `1 KiB`; idle `250ms..=120s` (default `30s`), deadline ≤ `15m`. | Stream pool (`64` / `4`). Enforces default-deny coordinator authz (`coordinator_pubkey_hex → caller_peer`), verified `SignedShardPartitionPlan`, Tier-1 `TrustedCircle` only, exact `stage_index` + local `PeerId`/pubkey binding, and `PolicyEngine::should_serve`. |
| `/phase/shard-rpc/1.0.0` | Raw `libp2p_stream`: 4-byte BE length-prefixed **postcard** handshake (`ShardRpcOpen` ≤ 256 B → `ShardRpcDecision` ≤ 1280 B), then **raw spliced bidirectional byte stream** (`64 KiB` splice buffer). | Idle `1s..=2h` (default `30m`), session `1s..=24h` (default `12h`). | Dedicated pools: `32`/`8` pending handshakes, `32`/`8` inbound sessions, `128` outbound. Synchronous `peer_filter` drops non-enrolled peers before reading 1 byte; **never** honors `allow_unauthenticated_jobs`; requires live `cluster_enabled = true` and `!manual_pause`, re-checked every `250 ms` with immediate teardown on revocation. |
| `/phase/job-offer/1.0.0` | `request_response::json` (`JobOffer` → `JobResponse`). | `64 KiB` max request & response. | Legacy WASM capability check (arch, runtime prefix, CPU, memory). No signature or submitter authz gate. |
| `/phase/reachability/1.0.0` | libp2p Identify protobuf (`new_with_signed_peer_record`). | Standard Identify bounds. | Rejects non-observable addresses (unspecified, multicast, broadcast, zero-port, unix, memory). |

## Protocol flow details

### `/phase/job-relay/2.0.0`

1. Requester opens a stream and sends `JobRelayStreamOpen` containing `job_id` and `SignedManifest<JobSpec>` (`serde_json`).
2. Worker validates authorization, replay cache, manifest expiry/clock skew, and policy, then sends `Accepted` (plus periodic `Progress("model_load")` keepalives while cold-loading a model past the 30-second idle timer).
3. Worker streams ordered `Event { sequence: 0, 1, ... }` frames containing `JobEvent` chunks.
4. Requester may send `JobRelayStreamControl::Cancel` at any time (for example, clicking **Stop** in Desktop Chat) to abort inference cleanly.
5. Upon completion, worker sends a final `Receipt` frame with the Ed25519 `SignedReceipt<JobResult>`; requester replays the chunk commitment and verifies all four bindings before upgrading `X-Lucid-Trust-Label`.

### `/phase/shard-stage/1.0.0`

Coordinator sends `ShardStageRequest` containing a verified `SignedShardPartitionPlan`, `stage_index`, and opaque stage activation payload to each stage worker in sequence. Each worker validates that it owns `stage_index`, forwards the payload to its local loopback stage bridge (`--shard-stage-bridge-port`) with `x-lucid-plan-id`, `x-lucid-job-id`, `x-lucid-model-cid`, `x-lucid-stage-index`, and `x-lucid-layer-range` headers, signs a `SignedShardStageReceipt` binding input and output commitments, and returns the stage output.

## Complete Kademlia DHT key namespaces

Every DHT record is verified at the application layer by Ed25519 signature (Kademlia storage itself is untrusted).

| DHT Key Pattern | Kademlia Op | Owner & Purpose |
|---|---|---|
| `/phase/capability/<arch>/<kind_label>` | `start_providing` / `get_providers` | `phase-net` — workload capability discovery |
| `/phase/<channel>/<arch>/manifest` | `put_record` / `get_record` | `phase-artifact-server` / `plasm` — Phase Boot channel manifests |
| `/phase/blob/<sha256_hex>` | `put_record` / `get_record` | `phase-artifact-server` — HTTP blob source hints |
| `phase/model/<32-byte ModelCid>` | `put_record` / `get_record` | `lucidd` — `SignedModelAdvertisement` (schema v2, ≤16 KiB, 15m max TTL) |
| `phase/model-alias/v1/<SHA256("phase:model-alias-key:v1\0" \|\| alias)>` | `put_record` / `get_record` | `lucidd` — `SignedAliasRecord` (schema v1, ≤4 KiB, 24h max TTL, sequence-ordered) |
| `phase/content-provider/v1/<32-byte ModelCid>` | `put_record` / `get_record` | `lucidd` — `SignedContentProviderRecord` (schema v1, ≤4 KiB, 24h max TTL, PeerId-bound) |
| `phase/lucid/catalog/providers/v1` | `start_providing` / `get_providers` | `lucidd` — catalog publisher provider discovery |
| `phase/lucid/catalog/publisher/v1/<publisher>` | `put_record` / `get_record` | `lucidd` — `SignedCatalog` (schema v1, ≤64 models, ≤64 KiB, 5m TTL) |
| `phase/lucid-enrollment/request/v1/<issuer>/<id>` | `put_record` / `get_record` | `lucidd` — doubly signed circle `EnrollmentRequest` mailbox |
| `phase/lucid-enrollment/approval/v1/<issuer>/<id>/<requester>` | `put_record` / `get_record` | `lucidd` — issuer-signed circle `Approval` mailbox |
