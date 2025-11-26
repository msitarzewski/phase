# Task 4 — Troubleshooting Guide


**Agent**: Docs Agent
**Estimated**: 5 days

#### 4.1 Common issues: Boot failures
- [ ] Document: `boot/docs/troubleshooting.md`
- [ ] Issues:
  - **USB not booting**: UEFI settings (disable Secure Boot, enable USB boot, check boot order)
  - **Boot menu not appearing**: BIOS/UEFI key (F12, F2, ESC varies by vendor)
  - **Kernel panic**: Hardware compatibility (driver missing), try different kernel params
  - **kexec fails**: Firmware doesn't support kexec, try chainload fallback
- [ ] Solutions: Step-by-step fixes, screenshots

**Dependencies**: M1, M4
**Output**: Boot failure troubleshooting section

#### 4.2 Common issues: Network
- [ ] Issues:
  - **No network**: No DHCP server, wrong interface, driver missing
  - **Wi-Fi not connecting**: Wrong SSID/passphrase, driver missing, captive portal
  - **Discovery fails**: No providers on network (LAN), bootstrap nodes down (WAN)
  - **Fetch timeout**: Mirrors down, slow connection, firewall blocking
- [ ] Solutions: Diagnostic commands (`ip link`, `ip addr`, `ping`), manual config steps

**Dependencies**: M2, M3
**Output**: Network troubleshooting section

#### 4.3 Common issues: Verification
- [ ] Issues:
  - **Signature verification failed**: Wrong key, manifest tampered, manifest unsigned
  - **Hash mismatch**: Artifact corrupted, wrong artifact served by mirror
  - **Rollback detected**: Trying to boot older manifest version (intentional protection)
- [ ] Solutions: Re-fetch manifest, verify checksums, check mirror integrity

**Dependencies**: M3
**Output**: Verification troubleshooting section

#### 4.4 Common issues: Plasm
- [ ] Issues:
  - **Plasm not starting**: Missing dependencies, config error, port conflict
  - **Job discovery fails**: Network down, no providers, wrong channel
  - **Job execution fails**: WASM corrupt, resource limit exceeded, timeout
  - **Receipt signature invalid**: Wrong key, receipt tampered
- [ ] Solutions: Check journalctl logs, verify Plasm config, re-fetch WASM

**Dependencies**: M6
**Output**: Plasm troubleshooting section

#### 4.5 Error code reference
- [ ] Create error code table:
  - `BOOT-001`: USB not bootable → Check UEFI settings
  - `NET-001`: Network not available → Check DHCP, drivers
  - `VFY-001`: Signature verification failed → Re-fetch manifest, check keys
  - `KEXEC-001`: kexec load failed → Check firmware support, try fallback
  - `PLASM-001`: Plasm startup failed → Check journalctl logs
- [ ] Link error codes to troubleshooting sections

**Dependencies**: Tasks 4.1-4.4
**Output**: Error code reference table

---
