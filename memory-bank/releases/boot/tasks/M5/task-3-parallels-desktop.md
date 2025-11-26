# Task 3 — Parallels Desktop Bundle


**Agent**: Release Agent
**Estimated**: 4 days

#### 3.1 Parallels VM configuration
- [ ] Research: Parallels `.pvm` bundle structure
- [ ] Components:
  - `.hdd` — Virtual disk (Parallels format, similar to QCOW2)
  - `config.pvs` — XML configuration (RAM, CPU, network, UEFI)
  - NVRAM files (UEFI variables)
- [ ] Tools: `prl_disk_tool` (Parallels CLI, macOS only)

**Dependencies**: None
**Output**: Parallels bundle structure documentation

#### 3.2 Convert QCOW2 to Parallels HDD
- [ ] Method 1: Use `prl_disk_tool convert` (macOS only)
  - Command: `prl_disk_tool convert --hdd phase-boot-arm64.hdd phase-boot-arm64.qcow2`
- [ ] Method 2: Create `.pvm` bundle manually
  - Create `.hdd` with `qemu-img convert -O parallels`
  - Generate `config.pvs` from template
- [ ] Recommendation: Document both, prefer Method 1 for official release

**Dependencies**: Task 2.2 (ARM64 QCOW2)
**Output**: Parallels HDD conversion method

#### 3.3 Build Parallels bundle
- [ ] Script: `boot/scripts/build-parallels.sh --input phase-boot-arm64.qcow2 --output phase-boot.pvm`
- [ ] Steps (macOS required):
  - Convert QCOW2 to `.hdd`: `prl_disk_tool convert --hdd phase-boot.hdd phase-boot-arm64.qcow2`
  - Create `.pvm` directory: `mkdir phase-boot.pvm`
  - Move HDD: `mv phase-boot.hdd phase-boot.pvm/`
  - Generate `config.pvs`: XML with RAM=2GB, CPU=2, Network=shared, UEFI=enabled
  - Package: `tar czf phase-boot-parallels.tar.gz phase-boot.pvm/`
- [ ] Target size: <700MB compressed

**Dependencies**: Tasks 3.1, 3.2
**Output**: Parallels bundle script, `.pvm` bundle

#### 3.4 Test Parallels bundle
- [ ] Platform: macOS with Parallels Desktop
- [ ] Steps:
  - Extract: `tar xzf phase-boot-parallels.tar.gz`
  - Open in Parallels: Double-click `phase-boot.pvm`
  - Start VM
- [ ] Validation: Full M1-M4 flow, network functional
- [ ] Document: `boot/docs/vm-setup-parallels.md`

**Dependencies**: Task 3.3
**Output**: Parallels test results, setup guide

---
