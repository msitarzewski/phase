# Task 1 — Project Structure & Build System


**Agent**: Systems Agent
**Estimated**: 3 days

#### 1.1 Create boot/ directory structure
- [ ] Initialize `boot/` at repository root
- [ ] Subdirectories:
  - `boot/esp/` — ESP partition skeleton
  - `boot/kernel/` — Kernel configs for x86_64 and arm64
  - `boot/initramfs/` — Initramfs build scripts and files
  - `boot/rootfs/` — Seed rootfs (SquashFS) content
  - `boot/scripts/` — Build and image creation scripts
  - `boot/configs/` — Bootloader configs, mode definitions

**Integration**: Root-level Makefile will orchestrate boot/ builds

#### 1.2 Create Makefile with targets
- [ ] `make esp` — Build ESP partition contents
- [ ] `make kernel-x86_64` — Build x86_64 kernel
- [ ] `make kernel-arm64` — Build arm64 kernel
- [ ] `make initramfs` — Build initramfs for both architectures
- [ ] `make rootfs` — Build seed SquashFS
- [ ] `make usb` — Assemble full USB image
- [ ] `make clean` — Clean all build artifacts
- [ ] `make test-qemu-x86` — Test in QEMU x86_64
- [ ] `make test-qemu-arm` — Test in QEMU ARM64

**Dependencies**: None
**Output**: `boot/Makefile`, `boot/README.md` (build instructions)

#### 1.3 USB partition layout script
- [ ] Script: `boot/scripts/partition-layout.sh`
- [ ] Creates 3-partition GPT disk:
  - Partition 1: ESP (FAT32, 256MB, bootable)
  - Partition 2: Seed (SquashFS, 300MB, read-only)
  - Partition 3: Cache (ext4, remaining space, optional)
- [ ] Handles dual-boot structure (separate EFI binaries for x86_64/arm64)
- [ ] Sets partition UUIDs and labels

**Dependencies**: None
**Output**: Functional partitioning script, tested with loop devices

---
