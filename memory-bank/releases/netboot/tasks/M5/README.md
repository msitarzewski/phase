# Milestone M5 — Cross-Platform & Integration Testing

**Status**: PLANNED
**Owner**: QA Agent (primary), Platform Agent (platform-specific)
**Dependencies**: M4 complete (CLI & configuration)
**Estimated Effort**: 2 weeks

## Intent Summary

Test provider functionality on target platforms (macOS ARM64, Linux x86_64) and verify end-to-end integration with Phase Boot VMs. This is the validation milestone - ensuring everything works together.

---

## Acceptance Criteria

1. **macOS ARM64**: Provider runs on Apple Silicon Macs
2. **Linux x86_64**: Provider runs on Linux servers
3. **End-to-end boot**: VM discovers Mac provider, fetches artifacts, boots
4. **Self-hosting**: Booted VM can become a provider
5. **Multi-provider**: Multiple providers on network work correctly

## Test Matrix

| Provider Platform | Client Platform | Discovery | Expected |
|-------------------|-----------------|-----------|----------|
| macOS ARM64 | QEMU ARM64 VM | mDNS | Works |
| macOS ARM64 | QEMU ARM64 VM | DHT | Works |
| Linux x86_64 | QEMU x86_64 VM | mDNS | Works |
| Linux x86_64 | QEMU x86_64 VM | DHT | Works |
| macOS ARM64 | Linux x86_64 | DHT | Works (cross-arch) |

---

## Tasks

1. [macOS ARM64 Testing](task-1-macos-arm64.md) — Test on Apple Silicon
2. [Linux x86_64 Testing](task-2-linux-x86.md) — Test on Linux servers
3. [Phase Boot Integration](task-3-boot-integration.md) — VM end-to-end tests
4. [Self-Hosting Test](task-4-self-hosting.md) — Boot and become provider
5. [Multi-Provider Test](task-5-multi-provider.md) — Multiple providers
