# Task 2 — Architecture Documentation


**Agent**: Docs Agent, Systems Agent
**Estimated**: 6 days

#### 2.1 Boot flow diagram
- [ ] Document: `boot/docs/architecture-boot-flow.md`
- [ ] Diagram: ASCII art or image (mermaid, plantuml)
  ```
  UEFI Firmware
       ↓
  Bootloader (systemd-boot/GRUB)
       ↓
  Seed Kernel (M1)
       ↓
  Seed Initramfs (M1)
       ↓ [Parse mode: Internet/Local/Private]
  Network Bring-up (M2)
       ↓
  Discovery (mDNS/DHT) (M2)
       ↓
  Manifest Fetch (M2)
       ↓
  Manifest Verification (M3)
       ↓
  Artifact Fetch (kernel, initramfs, rootfs) (M3)
       ↓
  OverlayFS Setup (M4)
       ↓
  kexec Load (M4)
       ↓
  kexec Exec (handoff) (M4)
       ↓
  Target Kernel
       ↓
  Target Initramfs → Target Rootfs
       ↓
  Plasm Daemon (M6)
       ↓
  Hello Job Execution (M6)
  ```
- [ ] Explanation: Text describing each step, referencing milestones

**Dependencies**: M1-M6 complete
**Output**: Boot flow diagram and explanation

#### 2.2 Component diagram
- [ ] Document: `boot/docs/architecture-components.md`
- [ ] Diagram: Component interactions
  ```
  ┌─────────────────────────────────────────┐
  │          UEFI Firmware Layer            │
  │  (systemd-boot, GRUB, Secure Boot)      │
  └──────────────┬──────────────────────────┘
                 ↓
  ┌─────────────────────────────────────────┐
  │         Seed Environment (M1)           │
  │  (Kernel, Initramfs, BusyBox, kexec)    │
  └──────────────┬──────────────────────────┘
                 ↓
  ┌─────────────────────────────────────────┐
  │      Discovery & Verification           │
  │  (mDNS, DHT, phase-verify, phase-fetch) │
  └──────────────┬──────────────────────────┘
                 ↓
  ┌─────────────────────────────────────────┐
  │       Target Environment (M4)           │
  │  (Kernel, Rootfs, OverlayFS, Plasm)     │
  └──────────────┬──────────────────────────┘
                 ↓
  ┌─────────────────────────────────────────┐
  │        Plasm Daemon (M6)                │
  │  (WASM Runtime, libp2p, Receipts)       │
  └─────────────────────────────────────────┘
  ```
- [ ] Component descriptions: Purpose, inputs, outputs, dependencies

**Dependencies**: M1-M6 complete
**Output**: Component diagram and descriptions

#### 2.3 Network architecture
- [ ] Document: `boot/docs/architecture-network.md`
- [ ] Content:
  - Discovery mechanisms: mDNS (LAN), Kademlia DHT (WAN)
  - Transport: QUIC + Noise encryption (M2)
  - Fetch pipeline: HTTPS mirrors, IPFS fallback (M3)
  - Plasm networking: libp2p peers, job discovery (M6)
- [ ] Diagrams: Network flow for each mode (Internet, Local, Private)

**Dependencies**: M2, M3, M6
**Output**: Network architecture documentation

#### 2.4 Security architecture
- [ ] Document: `boot/docs/architecture-security.md`
- [ ] Content:
  - Trust model: Root keys, targets keys (M3)
  - Verification pipeline: Manifest signatures, artifact hashes (M3)
  - Sandboxing: WASM isolation, resource limits (M6)
  - Mode policies: Internet (full), Local (LAN-only), Private (ephemeral) (M4)
  - Secure Boot integration: Shim, MOK, owner keys (M7)
- [ ] Threat mitigations: Per attack vector (see Task 3)

**Dependencies**: M3, M4, M6, Task 3
**Output**: Security architecture documentation

#### 2.5 Data flow diagrams
- [ ] Document: `boot/docs/architecture-data-flow.md`
- [ ] Diagrams:
  - Manifest discovery and fetch (M2 → M3)
  - Artifact fetch and cache (M3 → M4)
  - Job execution and receipt (M6)
- [ ] Annotations: Data formats (JSON, binary), verification points, caching

**Dependencies**: M2, M3, M6
**Output**: Data flow diagrams

---
