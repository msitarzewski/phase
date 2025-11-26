# Task 5 — Secure Boot Integration


**Agent**: Security Agent, Systems Agent
**Estimated**: 8 days

#### 5.1 Secure Boot overview
- [ ] Document: `boot/docs/secure-boot.md`
- [ ] Content:
  - **What is Secure Boot**: UEFI feature that verifies bootloader signatures
  - **Why Secure Boot**: Prevents boot-time malware, firmware rootkits
  - **Two paths**:
    - **Shim path**: Use Microsoft-signed shim bootloader (easier, trust MS CA)
    - **Owner keys path**: Self-sign bootloader, enroll keys in firmware (full control)
  - **Trade-offs**: Shim simpler but trusts MS; owner keys complex but independent

**Dependencies**: None
**Output**: Secure Boot overview documentation

#### 5.2 Path 1: Microsoft-signed shim
- [ ] Shim bootloader: `shim.efi` (pre-signed by Microsoft)
- [ ] Obtain shim:
  - Source: Distribution packages (Ubuntu, Fedora) or build from source
  - Verify signature: `sbverify --cert /path/to/MS-cert shim.efi`
- [ ] Integration:
  - Install shim: `boot/esp/EFI/BOOT/BOOTX64.EFI` (replace systemd-boot)
  - Shim chainloads: `grubx64.efi` or `systemd-boot.efi`
  - Sign Phase bootloader with MOK (Machine Owner Key):
    - Generate MOK: `openssl req -new -x509 -newkey rsa:2048 -keyout MOK.key -out MOK.crt -nodes -days 3650`
    - Sign bootloader: `sbsign --key MOK.key --cert MOK.crt --output systemd-boot.efi.signed systemd-boot.efi`
    - Install signed bootloader
- [ ] Enroll MOK:
  - Boot with shim → MOK management menu
  - Enroll MOK certificate: Import `MOK.crt`
  - Reboot: Secure Boot enabled, Phase Boot signed with MOK

**Dependencies**: Task 5.1, M1 Task 2.1 (bootloader)
**Output**: Shim path documentation and scripts

#### 5.3 Path 2: Owner-enrolled keys
- [ ] Generate signing keys:
  - Platform Key (PK): Root of trust, controls Secure Boot on/off
  - Key Exchange Key (KEK): Intermediate, signs db updates
  - Signature Database (db): Authorized signatures for bootloaders
  - Script: `boot/scripts/generate-sb-keys.sh`
- [ ] Sign Phase bootloader:
  - Sign with db key: `sbsign --key db.key --cert db.crt --output systemd-boot.efi.signed systemd-boot.efi`
- [ ] Enroll keys in firmware:
  - Enter UEFI firmware setup
  - Clear existing keys (optional, removes MS keys)
  - Enroll PK, KEK, db keys (UEFI interface varies by vendor)
  - Enable Secure Boot
- [ ] Verification: Boot with Secure Boot enabled

**Dependencies**: Task 5.1, M1 Task 2.1
**Output**: Owner keys path documentation and scripts

#### 5.4 Sign Phase kernels
- [ ] Sign seed kernels (M1):
  - `sbsign --key db.key --cert db.crt --output kernel-x86_64.efi.signed kernel-x86_64.efi`
  - `sbsign --key db.key --cert db.crt --output kernel-arm64.efi.signed kernel-arm64.efi`
- [ ] Update bootloader configs: Point to `.signed` kernels
- [ ] Sign target kernels (M4): Same process

**Dependencies**: Task 5.3, M1 Task 3.3 (kernels)
**Output**: Signed kernel binaries

#### 5.5 Test Secure Boot
- [ ] Test shim path:
  - Hardware: x86_64 PC with Secure Boot support
  - Enable Secure Boot in UEFI
  - Boot Phase USB with shim
  - Validation: Boot succeeds, no signature errors
- [ ] Test owner keys path:
  - Hardware: x86_64 PC with Secure Boot support
  - Enroll owner keys
  - Enable Secure Boot
  - Boot Phase USB with signed bootloader/kernels
  - Validation: Boot succeeds
- [ ] Test signature rejection:
  - Boot with unsigned bootloader → Expected: Secure Boot blocks boot
  - Boot with wrong signature → Expected: Secure Boot blocks boot

**Dependencies**: Tasks 5.2-5.4
**Output**: Secure Boot test results

#### 5.6 Secure Boot + kexec
- [ ] Challenge: kexec bypasses Secure Boot verification
- [ ] Mitigation options:
  - **Option A**: Disable kexec in Secure Boot mode (use traditional boot)
  - **Option B**: Sign target kernel with same keys (kexec still bypasses, but kernel verified at load)
  - **Option C**: Use `kexec_file_load` with signature verification (kernel support required)
- [ ] Recommendation: Option B for MVP (sign target kernels), document limitation
- [ ] Future: Explore Option C (requires kernel patches, more complex)

**Dependencies**: Task 5.5, M4 kexec
**Output**: Secure Boot + kexec documentation

---
