# Task 6 — USB Image Assembly


**Agent**: Systems Agent
**Estimated**: 3 days

#### 6.1 Image creation script
- [ ] Script: `boot/scripts/write-usb.sh`
- [ ] Arguments: `--output phase-boot.img --size 4G`
- [ ] Steps:
  - Create sparse file: `dd if=/dev/zero of=phase-boot.img bs=1 count=0 seek=4G`
  - Partition: `sgdisk` or `parted` (GPT)
  - Format partitions: `mkfs.fat -F32` (ESP), `mkfs.ext4` (cache)
  - Mount and populate ESP with bootloader, kernels, initramfs, DTBs
  - Copy SquashFS to partition 2
  - Set bootable flag on ESP
  - Unmount and finalize

**Dependencies**: All prior tasks (ESP, kernels, initramfs, rootfs)
**Output**: USB image creation script

#### 6.2 Assemble artifacts
- [ ] Copy all built components into staging area:
  - Bootloader binaries → `staging/esp/EFI/BOOT/`
  - Loader configs → `staging/esp/loader/`
  - Kernels → `staging/esp/kernel-*.efi`
  - Initramfs → `staging/esp/initramfs-*.img`
  - DTBs → `staging/esp/dtbs/`
  - SquashFS → `staging/rootfs.sqfs`

**Dependencies**: Task 6.1
**Output**: Staging directory

#### 6.3 Generate checksums
- [ ] Script: `boot/scripts/checksum.sh`
- [ ] Generate SHA256 checksums for:
  - `phase-boot.img`
  - All ESP artifacts (bootloader, kernels, initramfs)
  - SquashFS image
- [ ] Output: `boot/phase-boot.img.sha256` (preparation for M3 signing)

**Dependencies**: Task 6.1
**Output**: Checksum file

#### 6.4 Write to physical USB (developer test)
- [ ] Script: `boot/scripts/write-to-usb.sh`
- [ ] Arguments: `--device /dev/sdX --image phase-boot.img`
- [ ] Safety checks:
  - Confirm device selection
  - Warn about data loss
  - Verify device size ≥ image size
- [ ] Write: `dd if=phase-boot.img of=/dev/sdX bs=4M status=progress`
- [ ] Sync and eject

**Dependencies**: Task 6.1
**Output**: USB writing script

---
