# Task 1 — Quickstart Guides


**Agent**: Docs Agent
**Estimated**: 5 days

#### 1.1 Quickstart: x86_64 bare metal
- [ ] Document: `boot/docs/quickstart-x86_64.md`
- [ ] Content:
  - **Prerequisites**: USB stick (4GB+), x86_64 PC with UEFI
  - **Download**: `phase-boot-usb.img.gz`, `SHA256SUMS`, `SHA256SUMS.sig`
  - **Verify**: Checksums and signatures (M5 Task 4.4)
  - **Write USB**: `gunzip -c phase-boot-usb.img.gz | sudo dd of=/dev/sdX bs=4M status=progress`
  - **Boot**: Insert USB, enter UEFI boot menu (F12, F2, ESC), select USB
  - **First boot**: Select "Internet Mode", observe boot → discover → verify → kexec → hello job
  - **Troubleshooting**: Link to troubleshooting guide
- [ ] Screenshots/diagrams: Boot menu, console output

**Dependencies**: M1-M6 complete
**Output**: x86_64 quickstart guide

#### 1.2 Quickstart: ARM64 bare metal
- [ ] Document: `boot/docs/quickstart-arm64.md`
- [ ] Content: Similar to 1.1, but:
  - Hardware: Raspberry Pi 4 with UEFI firmware, USB-C power, HDMI display
  - Prerequisites: UEFI firmware installed (link to RPi UEFI project)
  - Write: SD card or USB stick (USB stick preferred for portability)
  - Boot: Power on, enter UEFI menu (if needed), boot from USB/SD
  - Device Tree: Mention automatic DTB selection for RPi 4
- [ ] Hardware notes: Tested on Raspberry Pi 4, other ARM64 boards may vary

**Dependencies**: M1-M6 complete
**Output**: ARM64 quickstart guide

#### 1.3 Quickstart: Apple Silicon (VM)
- [ ] Document: `boot/docs/quickstart-apple-silicon-vm.md`
- [ ] Content:
  - **Platform**: macOS Apple Silicon (M1/M2/M3)
  - **Prerequisites**: UTM or Parallels Desktop
  - **Option A: UTM**:
    - Download: `phase-boot-arm64.qcow2.gz`
    - Import: UTM → New VM → Emulate → ARM64 → UEFI
    - Attach QCOW2 as disk, configure network (shared or bridged)
    - Start VM, boot to Phase Boot
  - **Option B: Parallels**:
    - Download: `phase-boot-parallels.tar.gz`
    - Extract and open `.pvm` bundle
    - Start VM
  - **First boot**: Same as 1.1 (Internet Mode, full flow)
  - **Troubleshooting**: Network bridging, UEFI boot order
- [ ] Link to M5 VM setup guides (detailed steps)

**Dependencies**: M1-M6 complete, M5 VM images
**Output**: Apple Silicon quickstart guide

#### 1.4 Quickstart index
- [ ] Document: `boot/docs/README.md` (overview)
- [ ] Content:
  - Phase Boot overview (2-3 sentences)
  - Links to platform-specific quickstarts (x86_64, ARM64, Apple Silicon)
  - Links to architecture documentation
  - Links to troubleshooting
  - Links to advanced topics (Secure Boot, custom jobs)

**Dependencies**: Tasks 1.1-1.3
**Output**: Documentation index

---
