# Phase Boot ARM64 - Development Environment

**Status**: QEMU direct boot works, EFI/ISO boot BLOCKED
**Target**: Fast iteration development on Apple Silicon
**Last Updated**: 2025-11-26

## What Works

### QEMU Direct Boot (bypasses EFI entirely)
```bash
# This works - kernel+initrd passed directly to QEMU
qemu-system-aarch64 \
    -M virt -cpu host -accel hvf -m 1024 \
    -kernel build/kernel/vmlinuz-arm64 \
    -initrd build/initramfs/initramfs-arm64.img \
    -append "console=ttyAMA0 phase.mode=internet" \
    -nographic
```
- Full Phase Boot init runs, drops to shell
- ~5 second boot time with HVF acceleration

## What Doesn't Work (Exhaustive List)

### 1. GRUB `linux` command on ARM64
**Error**: `invalid magic number`
**Tried**:
- Alpine kernel (PE32+ EFI stub) - invalid magic
- Ubuntu kernel (gzip compressed) - invalid magic
- Adding gzio module to GRUB - still fails
- Using `search --file` to find kernel partition - finds it, still invalid magic

**Root cause**: GRUB's ARM64 `linux` module appears broken for these kernels

### 2. GRUB `chainloader` for EFI stub kernel
**Error**: `EFI stub: ERROR: Failed to handle fs_proto`
**Tried**: Chainloading kernel as EFI app with `initrd=` parameter
**Root cause**: Chainloader doesn't set up LOAD_FILE2 protocol for initrd

### 3. Direct EFISTUB (kernel as BOOTAA64.EFI)
**Error**: Kernel decompresses, then hangs at "Exiting boot services..."
**Root cause**: Initrd not being loaded (no bootloader to load it)

### 4. Unified Kernel Image (UKI) with objcopy
**Error**: `EFI decompressor: uncompression error`
**Tried**: Adding .initrd and .cmdline sections via objcopy
**Root cause**: objcopy corrupts the kernel's internal compression

### 5. Parallels ISO EFI boot
**Error**: "No operating system installed" or boots to BIOS
**Tried**:
- xorriso with -e efiboot.img
- Various FAT12/16/32 formats for efiboot.img
- Different ISO structures
**Root cause**: Parallels UEFI doesn't recognize our ISO EFI boot structure

### 6. Parallels raw disk image
**Error**: "Invalid hard disk image file"
**Tried**:
- Raw disk with GPT + ESP partition
- qemu-img convert to parallels format
**Root cause**: Parallels requires specific HDD format we can't create

### 7. Parallels keyboard input
**Error**: No keyboard response in GRUB menu
**Tried**: Adding USB device to VM
**Root cause**: Unknown - possibly Parallels UEFI issue

## Next Approaches to Try

1. **Use an existing bootable ARM64 Linux ISO** (Alpine, Ubuntu) and study how they structure their EFI boot
2. **systemd-boot** instead of GRUB (if available for ARM64)
3. **iPXE** for network boot
4. **UTM** instead of Parallels (uses QEMU backend, might work better)
5. **Copy working ISO structure** from a known-good ARM64 distro

## Files Modified
- `boot/initramfs/init` - removed `set -e` to handle errors gracefully
- `boot/esp/EFI/BOOT/grub-arm64.cfg` - added search command
- `boot/Makefile` - use grub-arm64.cfg for ARM64 builds

---

## Executive Summary

This release focuses on creating a **rapid development workflow** for Phase Boot using:
- ARM64 (aarch64) architecture
- Parallels Desktop on Apple Silicon (M1/M2/M3/M4)
- Direct disk image testing (no USB writes)
- Sub-minute rebuild-to-boot cycles

### Why ARM64 + Parallels?

| Approach | Iteration Time | Setup Complexity |
|----------|---------------|------------------|
| x86_64 USB | ~15 min | Low |
| x86_64 QEMU (emulated) | ~5 min | Medium |
| ARM64 Parallels (native) | **~30 sec** | Medium |
| ARM64 UTM (native) | ~1 min | Medium |

Parallels uses Apple's Hypervisor.framework for native ARM64 virtualization - no emulation penalty.

---

## Milestones

### M1: ARM64 Kernel & Toolchain
- [ ] Download/build ARM64 kernel using existing script
- [ ] Verify ARM64 GRUB/systemd-boot builds
- [ ] Test initramfs creation for ARM64
- [ ] Document macOS cross-compile requirements

### M2: Parallels VM Image Creation
- [ ] Create raw ARM64 disk image with `make ARCH=arm64`
- [ ] Convert to Parallels-compatible format
- [ ] Create Parallels VM configuration
- [ ] Document one-click rebuild workflow

### M3: Fast Iteration Scripts
- [ ] `make parallels` - Build + launch Parallels VM
- [ ] `make parallels-update` - Update ESP without full rebuild
- [ ] Hot-reload ESP contents (no reboot needed)
- [ ] Serial console integration

### M4: Debug & Development Tools
- [ ] GDB remote debugging over serial
- [ ] Kernel panic capture
- [ ] Boot timing analysis
- [ ] Integration with Claude Code workflow

---

## Technical Details

### ARM64 Kernel Acquisition

The existing `download-kernel.sh` already supports ARM64:

```bash
# Download ARM64 kernel from Alpine
./scripts/download-kernel.sh --arch arm64

# Output:
# build/kernel/vmlinuz-arm64
# build/kernel/modules/  (ARM64 modules)
```

### ARM64 Image Build

```bash
# Full ARM64 build
make clean
make download-kernel ARCH=arm64
make all ARCH=arm64

# Output:
# build/phase-boot-arm64.img (raw disk image)
```

### Parallels Integration Options

#### Option A: Raw Disk Image (Recommended)

Parallels can boot raw disk images directly:

```bash
# Create Parallels VM pointing to raw image
prlctl create "Phase Boot ARM64" \
    --ostype linux \
    --arch arm64

# Attach raw image as hard disk
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --image build/phase-boot-arm64.img \
    --type plain

# Enable EFI boot
prlctl set "Phase Boot ARM64" \
    --efi-boot on
```

#### Option B: HDD Conversion

Convert to Parallels HDD format for better performance:

```bash
# Convert raw to Parallels HDD
qemu-img convert -f raw -O parallels \
    build/phase-boot-arm64.img \
    build/phase-boot-arm64.hdd
```

#### Option C: Shared Folder Mount

Mount ESP as shared folder for instant updates:

```bash
# Enable shared folders in Parallels
prlctl set "Phase Boot ARM64" \
    --shf-host on \
    --shf-host-defined /path/to/boot/build/esp

# In VM, ESP contents available at:
# /media/psf/esp/
```

### Fast Iteration Workflow

```
┌─────────────────────────────────────────────────────────────┐
│  Developer Workflow (Target: <30 seconds)                   │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  1. Edit init script or binary                              │
│     └─> vim boot/initramfs/init                             │
│                                                             │
│  2. Rebuild initramfs only (5 seconds)                      │
│     └─> make initramfs ARCH=arm64                           │
│                                                             │
│  3. Update ESP (instant via shared folder)                  │
│     └─> (automatic if using shared folder)                  │
│         OR: make esp-update ARCH=arm64                      │
│                                                             │
│  4. Reboot VM (10 seconds)                                  │
│     └─> prlctl restart "Phase Boot ARM64"                   │
│         OR: In VM: reboot                                   │
│                                                             │
│  5. See changes in serial console                           │
│     └─> Parallels serial output window                      │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Makefile Targets (To Implement)

```makefile
# Build ARM64 for Parallels
parallels: ARCH=arm64
parallels: all
	@echo "Creating Parallels VM..."
	@$(SCRIPTS_DIR)/parallels-create.sh

# Update ESP without full rebuild
esp-update: ARCH=arm64
esp-update:
	@echo "Updating ESP..."
	@cp $(INITRAMFS_BUILD)/initramfs-$(ARCH).img $(ESP_BUILD)/
	@echo "ESP updated. Reboot VM to apply."

# Launch Parallels VM
parallels-start:
	prlctl start "Phase Boot ARM64"

# Restart Parallels VM
parallels-restart:
	prlctl restart "Phase Boot ARM64"

# View serial console
parallels-console:
	prlctl enter "Phase Boot ARM64" --serial 0
```

### Serial Console Setup

Parallels supports serial console for boot debugging:

```bash
# Add serial port to VM
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --output /tmp/phase-boot-serial.log

# Or use socket for interactive access
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --socket /tmp/phase-boot-serial.sock

# Connect to serial console
socat - UNIX-CONNECT:/tmp/phase-boot-serial.sock
```

---

## Directory Structure

```
memory-bank/releases/boot-arm/
├── README.md                 # This file
├── QUICKSTART.md             # Step-by-step setup guide
├── parallels-setup.md        # Parallels-specific configuration
├── troubleshooting.md        # Common issues and solutions
└── scripts/
    ├── parallels-create.sh   # Create Parallels VM
    ├── parallels-update.sh   # Fast update ESP
    └── parallels-debug.sh    # Debug utilities
```

---

## Prerequisites

### macOS Requirements

```bash
# Install Parallels Desktop (paid, or trial)
# Download from: https://www.parallels.com/

# Install Homebrew dependencies
brew install qemu coreutils

# Optional: For cross-compilation
brew install aarch64-elf-gcc
```

### Linux VM Requirements (for building)

If building on Linux (in a VM or remote machine):

```bash
# Ubuntu/Debian ARM64 or x86_64 with cross-compile
sudo apt-get install \
    gcc-aarch64-linux-gnu \
    qemu-user-static \
    binfmt-support
```

### Parallels Configuration

1. **Virtualization** (not emulation) - Required for performance
2. **EFI Boot** - Required for UEFI bootloader
3. **Serial Port** - Recommended for debug output
4. **Shared Folders** - Recommended for fast iteration

---

## Implementation Tasks

### Phase 1: Verification (Day 1)

- [ ] Verify `make all ARCH=arm64` produces working image
- [ ] Test ARM64 kernel download
- [ ] Verify GRUB builds for ARM64
- [ ] Create basic Parallels VM manually
- [ ] Boot and capture any errors

### Phase 2: Automation (Day 2)

- [ ] Create `scripts/parallels-create.sh`
- [ ] Add `make parallels` target to Makefile
- [ ] Implement fast ESP update workflow
- [ ] Document serial console setup

### Phase 3: Developer Experience (Day 3)

- [ ] Create QUICKSTART.md with screenshots
- [ ] Add troubleshooting guide
- [ ] Test full edit-rebuild-test cycle
- [ ] Measure and optimize iteration time

### Phase 4: Integration (Day 4)

- [ ] Integrate with existing boot test suite
- [ ] Add CI/CD for ARM64 builds (GitHub Actions)
- [ ] Update main boot documentation
- [ ] Create video walkthrough (optional)

---

## Success Criteria

1. **Iteration Time**: Edit → Rebuild → Boot < 60 seconds
2. **One Command**: `make parallels-restart` updates and reboots
3. **Debug Visibility**: Serial console shows full boot log
4. **Cross-Platform**: Works on M1/M2/M3/M4 Macs
5. **Documentation**: New developer can set up in < 15 minutes

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| ARM64 GRUB build fails | High | Fall back to systemd-boot or direct kernel boot |
| Parallels API changes | Medium | Document manual setup as fallback |
| Kernel incompatibility | Medium | Pin to known-good Alpine kernel version |
| Performance issues | Low | Use native virtualization, not emulation |

---

## References

- [Parallels Command Line Reference](https://download.parallels.com/desktop/v18/docs/en_US/Parallels%20Desktop%20Pro%20Edition%20Command-Line%20Reference.pdf)
- [Alpine Linux ARM64](https://alpinelinux.org/downloads/)
- [Apple Hypervisor Framework](https://developer.apple.com/documentation/hypervisor)
- [Existing QUICKSTART-ARM64.md](../../../boot/docs/QUICKSTART-ARM64.md)
- [Existing QUICKSTART-VM.md](../../../boot/docs/QUICKSTART-VM.md)

---

## Next Steps

1. Read this plan and provide feedback
2. Test ARM64 build on current codebase
3. Create Parallels VM manually to validate approach
4. Implement automation scripts
5. Document and iterate

---

**Owner**: Michael S.
**Created**: 2025-11-26
**Target Completion**: TBD
