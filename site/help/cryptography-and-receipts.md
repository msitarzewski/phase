# Cryptography, receipts and verification

Every identity, job request, streamed result, shard plan, model advertisement, and circle invitation in Phase is cryptographically signed with **Ed25519** (`ed25519-dalek` 2.2) and hashed with **SHA-256** (`sha2` 0.10, plus hardware-accelerated `ring` 0.17 for multi-gigabyte blob hashing).

## Root identity and PeerId binding (`phase-identity`)

- A node's root identity is a 32-byte Ed25519 secret stored at `<config_dir>/phase/identity.key`.
- `NodeIdentity::load_or_create` creates keys atomically via `O_CREAT | O_EXCL` at mode `0600` (no post-write `chmod` window), `fsync`, and atomic `hard_link` inside a mode-`0700` directory. Its `Debug` implementation redacts the secret scalar.
- `phase-net` initializes the libp2p swarm identity from the exact same 32-byte secret, so every node's libp2p `PeerId` is mathematically derived from its Ed25519 verifying key.
- Verifiers reconstruct `PeerId` from raw 32-byte public keys and fail closed if a signed record's public key does not match the delivering or claimed `PeerId`.

## SignedManifest and SignedReceipt

### `SignedManifest<T>` (`phase-manifest`)

- **Wire format:** JSON with `schema_version = 1`, `payload: T`, hex `signer_pubkey`, hex `signature`, RFC 3339 `created_at`, and optional `expires_at`.
- **Canonical signing message:**
  ```text
  b"phase-manifest:v1:" || canonical_json({schema_version, payload, created_at, expires_at})
  ```
  where `canonical_json` recursively sorts all JSON object keys lexicographically with no insignificant whitespace.
- **`manifest_hash()`:** `SHA-256(signing_message)`.
- **Remote execution policy (`verify_for_remote_execution`):** enforces `schema_version == 1`, valid Ed25519 signature, mandatory `expires_at`, maximum future clock skew of `5 minutes`, positive lifetime, and maximum TTL of `15 minutes`.

### `SignedReceipt<T>` (`phase-receipt`)

- **Wire format:** JSON with `schema_version = 1`, `result: T`, hex `job_id`, hex `worker_pubkey`, hex `signature`, and RFC 3339 `completed_at`.
- **Canonical signing message:**
  ```text
  b"phase-receipt:v1:" || canonical_json({schema_version, result, job_id, completed_at})
  ```

### Streaming `CommitmentAccumulator` (`phase-protocol`)

Every streamed chunk is folded into a running SHA-256 chain so both worker and requester verify exact ordered output integrity in constant memory:

```text
state_0 = SHA256("phase-protocol:v1:commitment")
state_n = SHA256(state_{n-1} || seq_be64 || len(kind)_be32 || kind || len(data)_be32 || data)
```

### Four-way receipt verify+bind (`lucidd`)

Before upgrading a remote response to `remote-receipt-verified`, `lucidd`'s router enforces all four checks:

1. **Signature & schema:** `receipt.verify()` passes (`schema_version == 1`, valid Ed25519 signature).
2. **Job binding:** `receipt.job_id == manifest_hash` and `job_spec_hash == manifest_hash`.
3. **Peer binding:** `PeerId::from(receipt.worker_pubkey) == dispatched_peer`.
4. **Commitment replay:** Replaying all received `OutputChunk`s (strict contiguous `seq` from `0`) produces the exact `(output_commitment, chunk_count)` sealed inside `receipt.result`.

## Composite shard plans and stage receipts

- **`SignedShardPartitionPlan`:** Coordinator signs `SHA256("phase/lucid-shard-plan/v1\0" || json(plan))`. Verification enforces `schema_version == 1`, `trust_tier == TrustedCircle`, TTL `1ms..=24h`, `2..=16` stages, and contiguous layer coverage `0..total_layers` with zero gaps or overlaps.
- **`SignedShardStageReceipt`:** Each stage worker signs `SHA256("phase/lucid-shard-stage-receipt/v1\0" || json(body))`. `verify_composite_shard_chain` verifies every stage's signature, `(plan_id, job_id, model_cid_hex, stage_index, start_layer, end_layer, peer_id, worker_pubkey_hex)`, and strict handoff equality (`stage[i].output_commitment_hex == stage[i+1].input_commitment_hex`).

## Signed DHT records, rollback and equivocation protection

Because Kademlia storage is unauthenticated, `lucidd` verifies Ed25519 signatures and enforces replay/equivocation checks locally:

- **`SignedModelAdvertisement` (schema v2):** Postcard-encoded (≤16 KiB), 15m max TTL, 5m max future skew, `PeerId` bound to `pubkey`.
- **`SignedAliasRecord` (schema v1):** Postcard-encoded (≤4 KiB), 24h max TTL, non-zero monotonic `sequence`. Tracks `(normalized_alias, publisher_pubkey) → (max_sequence, fingerprint)` where `fingerprint = SHA256("phase:alias-replay-fingerprint:v1\0" || postcard(record))` (up to 16,384 entries). Lower sequences are rejected; conflicting fingerprints at the same sequence **fail closed as equivocation**. Persists atomically at mode `0600` (`--alias-replay-state`). Multi-publisher CID/size disagreement returns `409 Conflict` (`AliasConflict`).
- **`SignedContentProviderRecord` (schema v1):** Postcard-encoded (≤4 KiB), 24h max TTL, `provider_peer_id` bound to `provider_pubkey`, monotonic `sequence` + equivocation check (in-memory, up to 16,384 entries).
- **`SignedCatalog` (schema v1):** Signed over `b"phase:lucid:model-catalog:v1\0" || postcard(...)`, 5m TTL, `PeerId`-bound, ≤64 models, ≤64 KiB.

## Additional domain-separated constructions

- **Delegated circle enrollment:** `phase/lucid-enrollment/{invitation,request,approval}/v1` (`Invitation` signed by issuer, `EnrollmentRequest` dual-signed by ephemeral delegation key and requester, `Approval` signed by issuer binding `request_hash`).
- **MLX canonical bundle CID:** `b"phase/mlx-bundle-root/v1\0"` over sorted ASCII paths, sizes, and file SHA-256s.
- **Deterministic spot-check sampling:** `b"lucid-redundant-sample:v1\0"`.
- **Local evidence store:** Magic `b"LUCEV001"`, per-record checksum `b"lucid-evidence-record:v1\0"`, event ID `b"lucid-evidence-event-id:v1\0"` (stores protocol outcomes only, never prompts or outputs).

## What verification does NOT prove

> [!IMPORTANT]
> Be clear about the exact cryptographic boundary:
> - A verified `SignedReceipt` proves **attributable transcript integrity**—that the specific `PeerId` signed the exact bytes you received for your exact manifest.
> - It does **not** mathematically prove that the remote peer ran an unaltered GGUF weight file or full precision. Deterministic redundant spot-checking (`--redundant-verification-permille`) adds local consistency evidence (`remote-spot-checked`) and applies a symmetric local reputation penalty on verified two-peer disagreement, not a universal zero-knowledge proof of inference.
> - Remote job replay caches (`RemoteReplayCache`) and `ContentProviderRecord` rollback state are currently in-memory (bounded by TTL); only `AliasRecord` replay state persists across restarts.
