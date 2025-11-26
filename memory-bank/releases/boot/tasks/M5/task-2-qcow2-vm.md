# Task 2 — QCOW2 VM Images


**Agent**: Release Agent, Systems Agent
**Estimated**: 6 days

#### 2.1 QCOW2 image creation (x86_64)
- [ ] Script: `boot/scripts/build-qcow2.sh --arch x86_64 --output phase-boot-amd64.qcow2`
- [ ] Steps:
  - Create QCOW2 disk: `qemu-img create -f qcow2 phase-boot-amd64.qcow2 4G`
  - Attach as NBD device: `modprobe nbd`, `qemu-nbd -c /dev/nbd0 phase-boot-amd64.qcow2`
  - Partition and format: Same as USB image (GPT, 3 partitions)
  - Populate: Copy ESP, seed rootfs, cache partition
  - Detach NBD: `qemu-nbd -d /dev/nbd0`
  - Compress QCOW2: `qemu-img convert -c -O qcow2 phase-boot-amd64.qcow2 phase-boot-amd64-compressed.qcow2`
- [ ] Target size: <600MB compressed

**Dependencies**: Task 1.1 (USB structure)
**Output**: QCOW2 build script, x86_64 QCOW2 image

#### 2.2 QCOW2 image creation (ARM64)
- [ ] Script: `boot/scripts/build-qcow2.sh --arch arm64 --output phase-boot-arm64.qcow2`
- [ ] Steps: Same as 2.1, but use ARM64 kernel, initramfs, DTBs
- [ ] Target size: <600MB compressed

**Dependencies**: Task 1.1
**Output**: ARM64 QCOW2 image

#### 2.3 Optimize QCOW2 for VM
- [ ] Enable virtio drivers in seed kernel:
  - `CONFIG_VIRTIO_BLK=y` (virtio block device)
  - `CONFIG_VIRTIO_NET=y` (virtio network)
  - `CONFIG_VIRTIO_PCI=y` (virtio PCI)
- [ ] Benefits: Faster I/O, better networking in VMs
- [ ] Rebuild kernels: M1 Task 3.3 with updated configs

**Dependencies**: M1 Task 3.1-3.2 (kernel configs)
**Output**: Virtio-enabled kernels

#### 2.4 Build targets: make release-qcow2
- [ ] Makefile targets:
  - `make release-qcow2-amd64` → `phase-boot-amd64.qcow2.gz`
  - `make release-qcow2-arm64` → `phase-boot-arm64.qcow2.gz`
  - `make release-qcow2` → Both architectures
- [ ] Generate checksums: `sha256sum *.qcow2.gz > qcow2.sha256`

**Dependencies**: Tasks 2.1, 2.2
**Output**: Makefile targets, QCOW2 artifacts

#### 2.5 Test QCOW2 in QEMU
- [ ] Test x86_64:
  - Command: `qemu-system-x86_64 -M q35 -m 2G -bios /usr/share/ovmf/OVMF.fd -drive file=phase-boot-amd64.qcow2,format=qcow2,if=virtio -netdev user,id=net0 -device virtio-net-pci,netdev=net0 -enable-kvm`
  - Validation: Full M1-M4 flow, network functional
- [ ] Test ARM64:
  - Command: `qemu-system-aarch64 -M virt -cpu cortex-a57 -m 2G -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd -drive file=phase-boot-arm64.qcow2,format=qcow2,if=virtio -netdev user,id=net0 -device virtio-net-pci,netdev=net0`
  - Validation: Same as x86_64

**Dependencies**: Task 2.4
**Output**: QEMU test results

#### 2.6 Test QCOW2 in UTM (macOS)
- [ ] Import QCOW2 into UTM (macOS Apple Silicon VM app)
- [ ] Configure:
  - Architecture: ARM64
  - RAM: 2GB
  - Network: Bridged or shared
  - Boot: UEFI
- [ ] Test: Boot ARM64 image, full M1-M4 flow
- [ ] Document: `boot/docs/vm-setup-utm.md`

**Dependencies**: Task 2.4
**Output**: UTM test results, setup guide

---
