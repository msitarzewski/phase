# M5 — Packaging & VM Images

**Objective**  
Ship disk images: USB (dual-arch) + ready-to-run VM images (QCOW2/Parallels). Document Apple Silicon path via VM.

**Deliverables**  
- `phase-boot-usb.img` with 3 partitions.  
- `phase-boot-amd64.qcow2`, `phase-boot-arm64.qcow2`.  
- Parallels bundle or recipe with UEFI config.  
- Checksums + sigs for all images.

**Acceptance Criteria**  
- Boots cleanly in Parallels and UTM/QEMU (network bridged).  
- Integrity check doc: users verify sums+sigs before use.

**Tasks**  
- [ ] Image packaging scripts + reproducible build notes.  
- [ ] Parallels/UTM quickstart.  
- [ ] Publish checksums/signatures.

**Risks**  
- VM USB passthrough inconsistencies → prefer disk images for VMs.
