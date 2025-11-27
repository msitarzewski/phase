# Task 2 — Architecture Documentation

**Agent**: Docs Agent
**Estimated**: 2 days

## 2.1 Create architecture overview

- [ ] Create `docs/architecture.md`:

```markdown
# Phase Netboot Architecture

## Overview

Phase Netboot enables distributed network booting where any machine can be
a boot artifact provider. The system consists of:

1. **Provider (plasmd)**: Serves boot artifacts and advertises via DHT/mDNS
2. **Client (Phase Boot)**: Discovers providers, fetches artifacts, boots
3. **DHT Network**: Kademlia DHT for internet-wide discovery
4. **mDNS**: Local network discovery

## Component Diagram

\`\`\`
┌─────────────────────────────────────────────────────────────────┐
│                        PLASMD NODE                               │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐  │
│  │  WASM Runtime   │  │  Boot Artifact  │  │  DHT Discovery  │  │
│  │  (wasmtime)     │  │  Server (HTTP)  │  │  (libp2p kad)   │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘  │
│                              │                                   │
│                    Advertises in DHT + mDNS                      │
└─────────────────────────────────────────────────────────────────┘

## Discovery Flow

1. Client boots with minimal kernel
2. Network initializes (DHCP)
3. phase-discover queries:
   - mDNS: _phase-image._tcp.local (LAN)
   - DHT: /phase/{channel}/{arch}/manifest (WAN)
4. Receives manifest URL
5. phase-fetch downloads kernel, initramfs
6. phase-verify checks Ed25519 signature
7. kexec loads new kernel
8. Boot complete

## Self-Hosting Loop

Machines that boot via Phase Netboot can become providers themselves:

Boot → Fetch → kexec → Run plasmd → Advertise → Serve others

## Security Model

- Manifests signed with Ed25519
- Artifacts verified by SHA256 hash
- No unsigned content executed
- Provider identity = signing key
\`\`\`

**Dependencies**: M5 complete
**Output**: Architecture document

---

## 2.2 Create data flow diagrams

- [ ] Discovery flow diagram
- [ ] Fetch and verification flow
- [ ] Self-hosting loop diagram
- [ ] Multi-provider topology

**Dependencies**: Task 2.1
**Output**: Diagrams

---

## 2.3 Document DHT key scheme

- [ ] Create `docs/dht-keys.md`:
  - Boot manifest keys
  - WASM capability keys
  - Record format
  - TTL and refresh

**Dependencies**: Task 2.2
**Output**: DHT documentation

---

## Validation Checklist

- [ ] Architecture clear to new developers
- [ ] Diagrams accurate and readable
- [ ] DHT scheme documented
- [ ] Security model explained
