# Milestone M1 — Boot Stub & Media Layout

**Status**: 🔵 PLANNED
**Owner**: Systems Agent (primary), Kernel Agent (kernel configs)
**Dependencies**: None (foundational milestone)
**Estimated Effort**: 3-4 weeks

## Intent Summary
Create dual-architecture (x86_64 + ARM64) USB bootable image with UEFI bootloader, seed kernel+initramfs, and partition layout supporting Internet/Local/Private boot modes.

---

## Acceptance Highlights
1. USB image boots on x86_64 UEFI hardware (Intel NUC, generic PC)
2. USB image boots on ARM64 UEFI hardware (Raspberry Pi 4, generic ARM64 board)
3. Boot menu displays 3 entries: Internet Mode, Local Mode, Private Mode
4. Selecting any mode boots into seed initramfs shell with:
   - Network tools available (ip, ping, dhcpcd)
   - kexec binary present and functional
   - 200MB+ free RAM for future fetch operations
5. Partition structure verified:
   - ESP (FAT32, ~256MB) with bootloader + kernels
   - Seed (SquashFS, ~300MB, read-only) with minimal userspace
   - Cache (ext4, remaining space, optional) for future CAS

## Tasks
1. [Project Structure & Build System](task-1-project-structure.md)
2. [ESP (EFI System Partition) Setup](task-2-esp-setup.md)
3. [Kernel Configuration & Build](task-3-kernel-configuration.md)
4. [Initramfs Construction](task-4-initramfs-construction.md)
5. [Seed Rootfs (SquashFS)](task-5-seed-rootfs.md)
6. [USB Image Assembly](task-6-usb-image.md)
7. [Testing & Validation](task-7-testing-and.md)
