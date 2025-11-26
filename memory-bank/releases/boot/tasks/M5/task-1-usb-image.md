# Task 1 — USB Image Packaging


**Agent**: Release Agent, Systems Agent
**Estimated**: 5 days

#### 1.1 Finalize USB image structure
- [ ] Review: M1 Task 6.1 (image creation script)
- [ ] Ensure:
  - 3-partition GPT: ESP (256MB), Seed (300MB), Cache (remaining)
  - ESP populated: Bootloader, kernels, initramfs, DTBs, loader configs
  - Seed partition: SquashFS rootfs
  - Cache partition: Empty ext4 (initialized for CAS in M3)
- [ ] Validate: All M1-M4 artifacts included

**Dependencies**: M1 Task 6.1, M2-M4 artifacts
**Output**: Finalized USB image structure

#### 1.2 Build target: make release-usb
- [ ] Makefile target: `boot/Makefile`
- [ ] Target: `make release-usb SIZE=4G OUTPUT=phase-boot-usb.img`
- [ ] Steps:
  - Build all components: ESP, kernels, initramfs, rootfs (M1 targets)
  - Assemble USB image: `boot/scripts/write-usb.sh`
  - Compress image: `gzip -9 phase-boot-usb.img` → `phase-boot-usb.img.gz`
  - Generate checksum: `sha256sum phase-boot-usb.img.gz > phase-boot-usb.img.gz.sha256`
- [ ] Output artifacts:
  - `phase-boot-usb.img.gz` (compressed USB image)
  - `phase-boot-usb.img.gz.sha256` (checksum)

**Dependencies**: Task 1.1, M1 Makefile targets
**Output**: `make release-usb` target, USB image artifact

#### 1.3 Optimize image size
- [ ] Compression: Use gzip -9 or xz for maximum compression
- [ ] Sparse file: Use `dd conv=sparse` to skip zero blocks
- [ ] Rootfs: Strip debug symbols from binaries in SquashFS
- [ ] Target size: <1GB compressed (target: 800MB)
- [ ] Measure: `ls -lh phase-boot-usb.img.gz`

**Dependencies**: Task 1.2
**Output**: Size-optimized USB image

#### 1.4 Test USB image on hardware
- [ ] Write to USB stick: `gunzip -c phase-boot-usb.img.gz | sudo dd of=/dev/sdX bs=4M status=progress`
- [ ] Test x86_64: Intel NUC or generic PC
  - Validation: Full M1-M4 flow (boot → discover → verify → kexec)
- [ ] Test ARM64: Raspberry Pi 4 with UEFI firmware
  - Validation: Same as x86_64
- [ ] Document results: `boot/docs/tested-hardware.md`

**Dependencies**: Task 1.2
**Output**: Hardware test results, tested hardware list

---
