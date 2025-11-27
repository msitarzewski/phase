# Milestone M2 — Manifest Generation & Signing

**Status**: PLANNED
**Owner**: Security Agent (primary), Runtime Agent (integration)
**Dependencies**: M1 complete (HTTP server, artifact endpoints)
**Estimated Effort**: 2 weeks

## Intent Summary

Generate boot manifests from artifact directories and sign them with Ed25519 keys. The manifest is the trust anchor - clients verify its signature before fetching any artifacts. Reuse existing plasmd Ed25519 keypair for signing.

---

## Acceptance Criteria

1. **Manifest generation**: Create manifest.json from artifact directory
2. **Schema compliance**: Manifest matches phase-verify expected schema
3. **Ed25519 signing**: Sign manifest with plasmd's existing keypair
4. **Hash computation**: SHA256 hashes for all artifacts
5. **Version/channel**: Proper metadata tagging
6. **Verification**: phase-verify accepts generated manifests

## Manifest Schema (from Phase Boot M3)

```json
{
  "version": "2025.11.26",
  "manifest_version": 1,
  "channel": "stable",
  "arch": "arm64",
  "created_at": "2025-11-26T12:00:00Z",
  "expires_at": "2025-12-26T12:00:00Z",
  "artifacts": {
    "kernel": {
      "hash": "sha256:abc123...",
      "size": 8388608,
      "path": "/stable/arm64/kernel"
    },
    "initramfs": {
      "hash": "sha256:def456...",
      "size": 4194304,
      "path": "/stable/arm64/initramfs"
    },
    "rootfs": {
      "hash": "sha256:ghi789...",
      "size": 536870912,
      "path": "/stable/arm64/rootfs"
    }
  },
  "signatures": [
    {
      "keyid": "ed25519:abc123...",
      "sig": "base64..."
    }
  ],
  "provider": {
    "peer_id": "12D3KooW...",
    "name": "my-provider"
  }
}
```

---

## Tasks

1. [Manifest Schema](task-1-manifest-schema.md) — Define and implement schema
2. [Hash Computation](task-2-hash-computation.md) — SHA256 for artifacts
3. [Ed25519 Signing](task-3-signing.md) — Sign manifest with node key
4. [Manifest Generation](task-4-generation.md) — Build manifest from artifacts
5. [Manifest Endpoint](task-5-endpoint.md) — Serve /manifest.json
6. [Testing & Validation](task-6-testing.md) — Verify with phase-verify

---

## File Changes

### New Files
```
daemon/src/provider/
├── manifest.rs         # Manifest types and generation
├── signing.rs          # Ed25519 manifest signing
└── hash.rs             # SHA256 computation
```

### Modified Files
```
daemon/src/provider/mod.rs      # Export manifest module
daemon/src/provider/handlers.rs # Add manifest endpoint
daemon/src/provider/state.rs    # Cache computed hashes
```

---

## Key Design Decisions

### Reuse Existing Keypair
plasmd already has an Ed25519 keypair for signing job receipts. We'll reuse it for manifest signing:
- Same trust model (node identity = signing key)
- No additional key management
- `keyid` in manifest = node's public key

### Hash Caching
Computing SHA256 for large files (rootfs can be 500MB+) is expensive:
- Compute hashes on startup
- Cache in ProviderState
- Re-compute only when file mtime changes

### Manifest Expiration
Manifests include `expires_at` to prevent indefinite replay:
- Default: 30 days from creation
- Configurable via provider config
- Clients should reject expired manifests

---

## Integration Points

- **phase-verify**: Must accept generated manifests
- **phase-fetch**: Uses manifest artifact URLs
- **plasmd signing**: Reuse `network::signing` module
