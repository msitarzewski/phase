# Task 6 — Testing & Validation


**Agent**: Networking Agent, Tooling Agent
**Estimated**: 5 days

#### 6.1 Local network test (mDNS)
- [ ] Setup: 2-node LAN
  - Node 1: Mock provider advertising `_phase-image._tcp` with manifest URL
  - Node 2: Phase Boot USB (Local Mode)
- [ ] Validation:
  - Boot to initramfs
  - Network up within 10 seconds
  - mDNS discovers provider
  - Manifest fetched and parsed
  - Console displays manifest summary

**Dependencies**: All Task 2 items (mDNS)
**Output**: Local network test results

#### 6.2 WAN test (DHT)
- [ ] Setup:
  - Bootstrap nodes running
  - Provider node advertising manifest on DHT
  - Phase Boot USB (Internet Mode)
- [ ] Validation:
  - Boot to initramfs
  - Network up within 10 seconds
  - libp2p client bootstraps to DHT
  - Manifest discovered via DHT query
  - Manifest fetched and parsed
  - Console displays manifest summary

**Dependencies**: All Task 3 items (libp2p DHT)
**Output**: WAN test results

#### 6.3 Private mode test
- [ ] Setup: Same as 6.2, but boot with Private Mode entry
- [ ] Validation:
  - Ephemeral identity used (different PeerID each boot)
  - No writes to cache partition (mount read-only)
  - Manifest discovery functional via DHT
  - Console warns about privacy implications

**Dependencies**: Task 3.5 (ephemeral identity), Task 5.3 (cache disabled)
**Output**: Private mode test results

#### 6.4 Fallback scenarios
- [ ] Test: No network available
  - Validation: Init script waits 30s, then drops to shell with diagnostics
- [ ] Test: mDNS no providers (Local Mode)
  - Validation: Timeout after 10s, display error, offer manual URL entry
- [ ] Test: DHT no manifest found (Internet Mode)
  - Validation: Retry 3x, timeout, display error, fallback to manual URL

**Dependencies**: Tasks 1.4, 4.2
**Output**: Fallback test results

#### 6.5 Cross-architecture testing
- [ ] Test x86_64:
  - QEMU: `make test-qemu-x86` with network bridge
  - Physical: Intel NUC or generic PC
- [ ] Test ARM64:
  - QEMU: `make test-qemu-arm` with network bridge
  - Physical: Raspberry Pi 4 with Wi-Fi

**Dependencies**: All M2 tasks
**Output**: Cross-arch test results

---
