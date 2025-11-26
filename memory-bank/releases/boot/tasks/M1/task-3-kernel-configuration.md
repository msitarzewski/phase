# Task 3 — Kernel Configuration & Build


**Agent**: Kernel Agent
**Estimated**: 7 days

#### 3.1 x86_64 kernel configuration
- [ ] Base config: `boot/kernel/config-x86_64`
- [ ] Enable required features:
  - kexec support (`CONFIG_KEXEC=y`)
  - OverlayFS (`CONFIG_OVERLAY_FS=y`)
  - Network drivers (Intel, Realtek, Broadcom)
  - NVMe, SATA, USB storage
  - EFI stub (`CONFIG_EFI_STUB=y`)
  - EFI variables (`CONFIG_EFIVAR_FS=y`)
- [ ] Disable unnecessary features:
  - Legacy BIOS support
  - Sound drivers (initially)
  - Graphics drivers beyond EFIFB

**Dependencies**: None
**Output**: Kernel config file

#### 3.2 ARM64 kernel configuration
- [ ] Base config: `boot/kernel/config-arm64`
- [ ] Enable required features:
  - Same kexec, overlayfs, storage as x86_64
  - ARM64-specific: Device Tree support
  - Common ARM64 NICs (USB Ethernet, built-in)
- [ ] Device Tree Blobs (DTBs):
  - Raspberry Pi 4 (`bcm2711-rpi-4-b.dtb`)
  - Generic ARM64 boards (add as tested)
- [ ] Store DTBs in: `boot/esp/dtbs/`

**Dependencies**: None
**Output**: Kernel config file, DTB list

#### 3.3 Kernel build automation
- [ ] Script: `boot/scripts/build-kernel.sh`
- [ ] Arguments: `--arch [x86_64|arm64]`
- [ ] Steps:
  - Download kernel source (latest stable, e.g., 6.6.x)
  - Apply config
  - Build kernel with parallel jobs
  - Extract `vmlinuz` (x86_64) or `Image.gz` (arm64)
  - Copy to ESP: `boot/esp/kernel-{arch}.efi`

**Dependencies**: Tasks 3.1, 3.2
**Output**: Kernel build script, kernel binaries

#### 3.4 Kernel module selection
- [ ] Identify essential modules (network, storage, USB)
- [ ] Build as built-in (not modules) to minimize initramfs size
- [ ] Document module decisions in `boot/kernel/MODULES.md`

**Dependencies**: Tasks 3.1, 3.2
**Output**: Module documentation

#### 3.5 Test kernel boot in QEMU
- [ ] Test x86_64 kernel: `qemu-system-x86_64 -kernel ... -append "..."`
- [ ] Test arm64 kernel: `qemu-system-aarch64 -kernel ... -dtb ...`
- [ ] Verify kexec available: `cat /proc/sys/kernel/kexec_load_disabled`

**Dependencies**: Task 3.3
**Output**: QEMU test commands documented

---
