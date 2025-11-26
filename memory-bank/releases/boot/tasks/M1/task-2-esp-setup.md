# Task 2 — ESP (EFI System Partition) Setup


**Agent**: Systems Agent
**Estimated**: 5 days

#### 2.1 Choose and install bootloader
- [ ] **Decision**: systemd-boot (primary) + GRUB (fallback)
  - Rationale: systemd-boot simpler, GRUB handles ARM64 quirks better
- [ ] Obtain bootloader binaries:
  - `BOOTX64.EFI` (systemd-boot for x86_64)
  - `BOOTAA64.EFI` (systemd-boot for ARM64)
  - GRUB EFI binaries for both architectures
- [ ] Install to ESP skeleton: `boot/esp/EFI/BOOT/`

**Dependencies**: None
**Output**: `boot/esp/EFI/BOOT/BOOTX64.EFI`, `boot/esp/EFI/BOOT/BOOTAA64.EFI`

#### 2.2 Create loader configuration
- [ ] File: `boot/esp/loader/loader.conf`
- [ ] Settings:
  - Default timeout: 10 seconds
  - Default entry: Internet Mode
  - Console mode: max (for compatibility)
  - Editor: no (security)

**Dependencies**: Task 2.1
**Output**: `boot/esp/loader/loader.conf`

#### 2.3 Boot entry: Internet Mode
- [ ] File: `boot/esp/loader/entries/internet.conf`
- [ ] Entry configuration:
  - Title: "Phase Boot — Internet Mode"
  - Linux: `/kernel-x86_64.efi` or `/kernel-arm64.efi`
  - Initrd: `/initramfs-x86_64.img` or `/initramfs-arm64.img`
  - Options: `phase.mode=internet phase.channel=stable`

**Dependencies**: Task 2.2
**Output**: Boot entry file

#### 2.4 Boot entry: Local Mode
- [ ] File: `boot/esp/loader/entries/local.conf`
- [ ] Entry configuration:
  - Title: "Phase Boot — Local Mode"
  - Options: `phase.mode=local phase.channel=stable phase.cache=enabled`

**Dependencies**: Task 2.2
**Output**: Boot entry file

#### 2.5 Boot entry: Private Mode
- [ ] File: `boot/esp/loader/entries/private.conf`
- [ ] Entry configuration:
  - Title: "Phase Boot — Private Mode"
  - Options: `phase.mode=private phase.cache=disabled phase.nowrite=true`

**Dependencies**: Task 2.2
**Output**: Boot entry file

#### 2.6 GRUB fallback configuration
- [ ] File: `boot/esp/EFI/BOOT/grub.cfg`
- [ ] Menu entries mirroring systemd-boot entries
- [ ] Fallback chainload logic if systemd-boot fails

**Dependencies**: Task 2.1
**Output**: GRUB configuration file

---
