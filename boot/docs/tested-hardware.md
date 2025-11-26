# Phase Boot Hardware Compatibility

## Overview

Phase Boot targets UEFI-capable x86_64 and ARM64 systems.

**Note**: Secure Boot is not currently supported. Disable it in BIOS/UEFI settings.

## Tested Hardware

### Virtual Machines

| Platform | Architecture | Status | Notes |
|----------|--------------|--------|-------|
| QEMU/KVM | x86_64 | Pending | Primary dev target |
| QEMU/KVM | aarch64 | Pending | Needs QEMU_EFI.fd |
| VirtualBox | x86_64 | Untested | Enable EFI in settings |
| VMware Fusion | x86_64 | Untested | |
| Parallels Desktop | x86_64 | Untested | Apple Silicon VM |
| UTM | aarch64 | Untested | Apple Silicon native |

### x86_64 Physical Hardware

| Device | Status | Notes |
|--------|--------|-------|
| Intel NUC (various) | Untested | Good reference platform |
| Dell OptiPlex | Untested | |
| HP ProDesk | Untested | |
| Lenovo ThinkCentre | Untested | |
| Generic PC (UEFI) | Untested | |

### ARM64 Physical Hardware

| Device | Status | Notes |
|--------|--------|-------|
| Raspberry Pi 4 | Untested | Needs Pi UEFI firmware |
| Raspberry Pi 5 | Untested | Needs Pi UEFI firmware |
| Apple Silicon Mac | N/A | VM only (Parallels/UTM) |
| Ampere Altra | Untested | Server-class ARM64 |

## Requirements

### Minimum Requirements

- **Architecture**: x86_64 or ARM64
- **Firmware**: UEFI 2.0+
- **RAM**: 512MB minimum, 2GB recommended
- **Storage**: USB 2.0+ (4GB minimum)
- **Network**: Ethernet (Wi-Fi support limited in M1)

### UEFI Requirements

- UEFI boot mode (not Legacy/CSM)
- FAT32 ESP partition support
- Secure Boot must be disabled (for now)

## Testing a New Device

1. **Prepare USB**:
   ```bash
   sudo ./scripts/write-usb.sh --image build/phase-boot-x86_64.img --device /dev/sdX
   ```

2. **Configure BIOS/UEFI**:
   - Enter setup (usually F2, F12, Del, or Esc at boot)
   - Disable Secure Boot
   - Enable UEFI boot mode
   - Set USB as first boot device

3. **Boot and Test**:
   - Verify boot menu appears
   - Test each boot mode
   - Check network connectivity
   - Verify kexec availability

4. **Report Results**:
   - Update this document with device info
   - Note any quirks or issues
   - Include BIOS/UEFI version if relevant

## Known Issues

### General

*None documented yet*

### Device-Specific

*None documented yet*

## Raspberry Pi UEFI Setup

Raspberry Pi requires third-party UEFI firmware:

1. Download from: https://github.com/pftf/RPi4
2. Extract to SD card FAT32 partition
3. Boot Pi with SD card
4. Insert Phase Boot USB
5. Select USB from boot menu

## Apple Silicon Notes

Apple Silicon Macs cannot boot Linux natively from USB. Options:

1. **Parallels Desktop**: Create VM, attach raw disk image
2. **UTM**: Create VM with ARM64 Linux
3. **Asahi Linux**: Different project, not Phase Boot

For development, use QEMU ARM64 emulation on any host.
