# Milestone M5 — Packaging & VM Images

**Status**: 🔵 PLANNED
**Owner**: Release Agent (primary), Systems Agent (image builds)
**Dependencies**: M1-M4 complete (full boot-to-kexec flow functional)
**Estimated Effort**: 3-4 weeks

## Intent Summary
Package Phase Boot as distributable disk images: USB image (dual-arch), VM-ready images (QCOW2 for QEMU/UTM, Parallels bundle), with cryptographic checksums and signatures for integrity verification. Document installation and verification procedures for end users.

---

## Acceptance Highlights
1. **USB image functional**: Boots on x86_64 and ARM64 hardware, full M1-M4 flow
2. **QCOW2 images functional**: Boot in QEMU, UTM, KVM with network
3. **Parallels bundle functional**: Boot in Parallels Desktop (macOS) with network
4. **Integrity verification**: Checksums match, signatures verify with public key
5. **Documentation complete**: Installation guide, verification steps, VM setup
6. **Reproducible builds**: Two builds from same commit produce identical checksums

## Tasks
1. [USB Image Packaging](task-1-usb-image.md)
2. [QCOW2 VM Images](task-2-qcow2-vm.md)
3. [Parallels Desktop Bundle](task-3-parallels-desktop.md)
4. [Checksums & Signatures](task-4-checksums-and.md)
5. [Reproducible Builds](task-5-reproducible-builds.md)
6. [Release Workflow & Distribution](task-6-release-workflow.md)
