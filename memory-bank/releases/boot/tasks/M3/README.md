# Milestone M3 — Verification & Fetch Pipeline

**Status**: 🔵 PLANNED
**Owner**: Security Agent (primary), Transport Agent (fetch implementation)
**Dependencies**: M1 complete (initramfs), M2 complete (manifest discovery)
**Estimated Effort**: 4-5 weeks

## Intent Summary
Verify signed manifests using TUF-like trust model with Ed25519 signatures, fetch kernel/initramfs/rootfs artifacts by content hash from HTTPS mirrors or IPFS gateways, and implement content-addressable storage (CAS) cache with garbage collection policy.

---

## Acceptance Highlights
1. **Manifest verification**: Signature validation using Ed25519 public keys
2. **Rollback protection**: Manifest version must be ≥ locally cached version
3. **Artifact integrity**: SHA256 hash verification before use
4. **Tamper detection**: Boot aborts with clear error if signature/hash mismatch
5. **Multi-mirror fetch**: HTTPS mirrors tried first, IPFS gateway fallback
6. **CAS cache**: Downloaded artifacts cached by hash (disabled in Private Mode)
7. **Garbage collection**: Cache size limited, old artifacts evicted (LRU policy)
8. **Fetch resilience**: Retry with exponential backoff, mirror fallback

## Tasks
1. [Trust Model & Key Management](task-1-trust-model.md)
2. [Manifest Signing & Verification](task-2-manifest-signing.md)
3. [Artifact Fetcher](task-3-artifact-fetcher.md)
4. [Content-Addressable Storage (CAS) Cache](task-4-content-addressable.md)
5. [Integration: Verify → Fetch Pipeline](task-5-integration-verify.md)
6. [Testing & Validation](task-6-testing-and.md)
