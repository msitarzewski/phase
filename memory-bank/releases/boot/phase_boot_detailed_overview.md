
# Phase Boot — Detailed Systems Overview

**Goal:** A dual-arch (x86_64 + ARM64/UEFI) bootable USB that launches a tiny seed OS, **discovers → verifies → fetches → boots** a trusted kernel+initramfs+rootfs via DHT/mDNS + content addressing, then optionally starts the **Plasma** runtime. Modes: **Internet**, **Local**, **Private**.

**Non-goals:** Native Apple Silicon bare-metal boot. (Apple Silicon runs via VM images/recipes.)

---

## 1) Files in `memory-bank/releases/boot/` and how to use them

These files are the **release plan**, not the code. They coordinate people/agents and define acceptance.

- **README.md** — High-level statement of scope, outcome, milestones, and DoD.
- **index.yaml** — Machine-readable index for automation. Lists milestones and pointers for agent orchestration.
- **M1_boot_stub.md** — Bootloader & media layout.
- **M2_discovery.md** — Network bring-up & discovery logic.
- **M3_verify_fetch.md** — Verification & fetch pipeline.
- **M4_kexec_modes.md** — Boot handoff & policy management.
- **M5_packaging_vm.md** — Artifact packaging & VM images.
- **M6_plasma_hello.md** — Post-boot "hello" job integration.
- **M7_docs_secureboot.md** — Docs, threat model, and Secure Boot.
- **agents_matrix.md** — Responsibility assignments.

---

## 2) Execution Plan (as a Pipeline)

**Boot pipeline (state machine):**
```
UEFI → loader menu
  ├─ Internet → Initramfs → Discover manifest (mDNS/DHT) → Verify → Fetch → kexec
  ├─ Local → Initramfs → Check cache/LAN peers → Verify → Fetch → kexec
  └─ Private → Initramfs → Tor optional → Verify → Fetch (tmpfs) → kexec
```

**Failure handling**
- Verify/fetch fails → drop to Seed Shell (diagnostics, manual network setup).
- kexec fails → attempt bootloader chainload fallback.

---

## 3) Deliverable Details

### 3.1 Boot Media Layout (M1)
**Partitions**
1. **ESP (FAT32):** UEFI bootloaders and menu entries.
2. **Seed (squashfs):** Seed kernel + initramfs + utilities.
3. **Cache (ext4):** Content-addressed store (off in Private mode).

**Kernel config**
- Must include kexec, overlayfs, wireguard, FUSE, etc.

**Loader entries**
- Three systemd-boot entries for Internet/Local/Private modes.

---

### 3.2 Network + Discovery (M2)
**Bring-up**
- Wired DHCP, Wi-Fi helper, captive portal detection.

**Discovery**
- mDNS for LAN, libp2p/Kademlia DHT for WAN.
- Preference order: LAN → WAN → Manual URL.

---

### 3.3 Verification + Fetch (M3)
**Manifest schema**
```json
{
  "version":"2025.11.12",
  "channel":"stable",
  "arch":"amd64",
  "artifacts": {"kernel":"sha256:...", "initramfs":"sha256:...", "rootfs":"sha256:..."},
  "signatures":[{"keyid":"ed25519:...", "sig":"base64..."}]
}
```

**Transports**
- HTTPS mirror → IPFS → BitTorrent (future).

**CAS Cache**
- Directory structure `/cas/sha256/<hash> → artifact`.

---

### 3.4 Handoff + Modes (M4)
**Orchestrator:** `fetch-verify-kexec`
- Runs full chain: net → discover → verify → fetch → kexec.

**Overlay Policy**
- Internet/Local: cache writes allowed.
- Private: overlay on tmpfs, no persistence.

---

### 3.5 Packaging & VMs (M5)
- USB, QCOW2, and Parallels images.
- Checksums + signatures for all artifacts.
- UEFI boot verified on Parallels/UTM/QEMU.

---

### 3.6 Plasma Hello Job (M6)
- Post-boot service runs Plasma with a hello-world WASM job.
- Receipts (hash, runtime, timing) stored unless Private mode.

---

### 3.7 Docs, Threat Model, Secure Boot (M7)
- Threats: tamper, rollback, poisoning, key compromise, privacy limits.
- Secure Boot: shim (MS-signed) and owner-enrolled keys.
- Troubleshooting: kexec fallback, missing drivers, network issues.

---

## 4) Interfaces for Agents

### Policy File `/etc/policy.toml`
```toml
mode = "internet"
channel = "stable"
cache_enabled = true
min_version = "2025.10.01"
allow_tor = false
```

### mDNS TXT record
`_phase-image._tcp.local`
```
arch=amd64
channel=stable
manifest_cid=bafy...
mirror=https://mirror.phase.dev/stable/amd64/manifest.json
```

### DHT key/value
```json
{
  "manifest_cid":"bafy...",
  "mirrors":["https://mirror.phase.dev/stable/..."],
  "providers":["/ip4/1.2.3.4/tcp/4001/p2p/..."]
}
```

---

## 5) Test Plan

| Scenario | Expected |
|-----------|-----------|
| Internet | Fetch and boot via WAN |
| LAN | Discover via mDNS |
| Local | Boot from cache |
| Private | No persistent writes |
| Tampered kernel | Abort |
| Expired manifest | Abort |
| Captive portal | Manual URL option |
| kexec fail | Fallback to chainload |

---

## 6) Detailed Tasks per Milestone

**M1:** Create boot image, ESP entries, initramfs with tools, Makefile automation.  
**M2:** Implement mDNS + DHT discovery; Wi-Fi setup; channel logic.  
**M3:** Build verification tools and content-addressed fetchers.  
**M4:** Create orchestrator for verify → fetch → kexec flow.  
**M5:** Package reproducible USB/VM images with checksums.  
**M6:** Integrate Plasma runtime hello job.  
**M7:** Write complete docs and security guidance.

---

## 7) Risks and Assumptions

| Assumption | Risk | Mitigation |
|-------------|------|-------------|
| UEFI ARM64 | Non-UEFI SBCs fail | Publish board table |
| kexec universal | Blocked on some | Chainload fallback |
| LAN provider exists | mDNS blocked | DHT/HTTPS fallback |
| Secure Boot config | User confusion | Detection + docs |

---

## 8) Success Criteria

- Boots verified kernel on x86_64 + ARM64.
- Plasma hello job runs successfully.
- Private mode leaves no traces.
- All builds signed and reproducible.

---

## 9) Suggested Next Repo Steps

1. Create `boot/` tree with esp/initramfs/manifests/scripts.
2. Add Makefile and build scripts.
3. Create sample manifests for both architectures.
4. Add reference LAN provider.
5. Add quickstarts and Secure Boot documentation.

---

**Summary:**  
This release formalizes the "Phase Boot" subproject—turning any machine into a verified, decentralized, peer-updated OS launcher. It introduces reproducible build standards, secure verification, and a smooth pathway into the distributed Plasma runtime layer.
