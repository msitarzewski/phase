# Release: Phase Netboot Provider (plasmd as Boot Artifact Server)

**Scope:** Extend plasmd to serve boot artifacts (kernel, initramfs, rootfs) alongside WASM job execution. Enable DHT-based internet booting where any plasmd node can be a provider.

**Outcome:** A unified plasmd daemon that:
1. Executes WASM jobs (existing)
2. Serves boot artifacts over HTTP (new)
3. Advertises both capabilities in DHT (new)
4. Enables self-hosting: machines boot from network, then become providers themselves

**Target Platforms:** macOS ARM64, Linux x86_64/ARM64

---

## Vision

```
┌─────────────────────────────────────────────────────────────────┐
│                        PLASMD NODE                               │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐  │
│  │  WASM Runtime   │  │  Boot Artifact  │  │  DHT Discovery  │  │
│  │  (wasmtime)     │  │  Server (HTTP)  │  │  (libp2p kad)   │  │
│  └────────┬────────┘  └────────┬────────┘  └────────┬────────┘  │
│           │                    │                    │           │
│           └────────────────────┴────────────────────┘           │
│                              │                                   │
│                    Advertises in DHT:                            │
│                    • /phase/capability/{arch}/{runtime} (jobs)   │
│                    • /phase/{channel}/{arch}/manifest (boot)     │
└─────────────────────────────────────────────────────────────────┘
                               ↑
         ┌─────────────────────┴─────────────────────┐
         │                                           │
    ┌────┴────┐                               ┌──────┴──────┐
    │ Phase   │  Discovers via DHT            │ PHP Client  │
    │ Boot    │  Fetches kernel/initramfs     │ Submits     │
    │ USB/VM  │  Verifies signatures          │ WASM jobs   │
    │         │  kexec into target OS         │             │
    └────┬────┘                               └─────────────┘
         │
         ↓ (after kexec)
    ┌─────────┐
    │ BECOMES │  ← Self-hosting loop!
    │ PLASMD  │    Now serves boot artifacts
    │ NODE    │    AND runs WASM jobs
    └─────────┘
```

**The Loop:** Boot from network → run plasmd → serve others → they boot → repeat.

---

## Milestones

| ID | Name | Description |
|----|------|-------------|
| M1 | HTTP Artifact Server | Add HTTP server to plasmd for serving boot files |
| M2 | Manifest Generation & Signing | Generate and sign boot manifests (Ed25519) |
| M3 | DHT/mDNS Advertisement | Advertise boot artifacts in DHT and LAN |
| M4 | CLI & Configuration | Provider management commands and config |
| M5 | Cross-Platform & Integration | macOS + Linux testing, e2e with Phase Boot |
| M6 | Documentation | Setup guides, architecture docs, troubleshooting |

---

## Definition of Done

1. **macOS ARM64**: `plasmd serve` runs on Mac, serves boot artifacts, discoverable via DHT
2. **Linux x86_64**: Same functionality on Linux servers
3. **Phase Boot integration**: VM discovers Mac provider, fetches artifacts, kexecs successfully
4. **Self-hosting**: Booted system can immediately become a provider
5. **Multi-provider**: Multiple providers on network, clients select best match
6. **Signed manifests**: All manifests Ed25519 signed, verification enforced
7. **Documentation**: Complete setup guides for provider operation

---

## Architecture Integration

### Existing Components (from Phase Boot)
- `phase-discover` — Queries DHT for manifest URL ✅
- `phase-fetch` — Downloads artifacts with hash verification ✅
- `phase-verify` — Verifies manifest signatures ✅
- `kexec-boot.sh` — Loads and executes new kernel ✅

### New Components (this release)
- **HTTP artifact server** — Embedded in plasmd, serves files
- **Manifest generator** — Creates manifest.json from artifact directory
- **Manifest signer** — Signs manifest with node's Ed25519 key
- **DHT boot advertiser** — Publishes manifest URL to DHT
- **mDNS advertiser** — Announces `_phase-image._tcp` for LAN discovery

### DHT Key Scheme
```
Boot artifacts:  /phase/{channel}/{arch}/manifest
                 Example: /phase/stable/arm64/manifest
                 Value: http://{peer-ip}:8080/manifest.json

WASM capabilities: /phase/capability/{arch}/{runtime}
                   Example: /phase/capability/arm64/wasmtime-27
                   Value: (provider records)
```

### Manifest Schema (from M3_verify_fetch.md)
```json
{
  "version": "2025.11.26",
  "manifest_version": 1,
  "channel": "stable",
  "arch": "arm64",
  "artifacts": {
    "kernel": {
      "hash": "sha256:abc123...",
      "size": 8388608,
      "path": "/kernel"
    },
    "initramfs": {
      "hash": "sha256:def456...",
      "size": 4194304,
      "path": "/initramfs"
    },
    "rootfs": {
      "hash": "sha256:ghi789...",
      "size": 536870912,
      "path": "/rootfs"
    }
  },
  "signatures": [
    {
      "keyid": "ed25519:...",
      "sig": "base64..."
    }
  ]
}
```

---

## HTTP Endpoints

```
GET /                       → Provider status/info
GET /manifest.json          → Signed boot manifest
GET /kernel                 → Kernel image (vmlinuz)
GET /initramfs              → Initramfs image
GET /rootfs                 → Root filesystem (squashfs)
GET /health                 → Health check for load balancers
```

---

## CLI Commands

```bash
# Start provider mode
plasmd serve \
  --artifacts /path/to/boot-artifacts \
  --channel stable \
  --arch arm64 \
  --port 8080

# Check provider status
plasmd provider status

# List advertised artifacts
plasmd provider list

# Generate manifest without serving
plasmd manifest generate \
  --artifacts /path/to/boot-artifacts \
  --output manifest.json \
  --sign
```

---

## Configuration

### Provider Config (`/etc/plasm/provider.toml`)
```toml
[provider]
enabled = true
port = 8080
artifacts_dir = "/var/lib/plasm/boot-artifacts"

[provider.channels.stable]
arch = ["arm64", "x86_64"]
kernel = "vmlinuz-stable"
initramfs = "initramfs-stable.img"
rootfs = "rootfs-stable.sqfs"

[provider.channels.testing]
arch = ["arm64"]
kernel = "vmlinuz-testing"
initramfs = "initramfs-testing.img"

[provider.advertisement]
dht = true
mdns = true
```

---

## Test Scenarios

| Scenario | Description | Expected |
|----------|-------------|----------|
| LAN Discovery | VM discovers Mac via mDNS | Manifest URL returned |
| WAN Discovery | VM discovers via DHT bootstrap | Manifest URL returned |
| Artifact Fetch | phase-fetch downloads from provider | All files verified |
| Signature Verify | Tampered manifest | Rejected with error |
| Multi-Provider | 2+ providers on network | Client picks best |
| Self-Hosting | Boot via provider, become provider | Full loop works |
| Cross-Arch | ARM64 VM, x86_64 provider | Correct arch served |

---

## Dependencies

- **Phase Boot** (M1-M4): Consumer side complete
- **plasmd networking**: libp2p/Kademlia already integrated
- **Ed25519 signing**: Already in plasmd for job receipts

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Large file serving performance | Range requests, chunked transfer |
| NAT traversal for providers | QUIC, relay nodes, UPnP |
| Key management complexity | Reuse existing plasmd identity |
| Firewall blocking HTTP port | Configurable port, docs for port forwarding |

---

## Release Owner

**Owner:** Michael S.
**Contributors:** Runtime (Plasm), Networking, Security/Signing

---

## Files

- [index.yaml](./index.yaml) — Machine-readable milestone index
- [agents_matrix.md](./agents_matrix.md) — Responsibility assignments
- [tasks/M1/](./tasks/M1/) — HTTP Artifact Server tasks
- [tasks/M2/](./tasks/M2/) — Manifest Generation & Signing tasks
- [tasks/M3/](./tasks/M3/) — DHT/mDNS Advertisement tasks
- [tasks/M4/](./tasks/M4/) — CLI & Configuration tasks
- [tasks/M5/](./tasks/M5/) — Cross-Platform & Integration tasks
- [tasks/M6/](./tasks/M6/) — Documentation tasks
