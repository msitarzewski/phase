# Phase Boot Testing Guide

## Overview

This document describes testing procedures for Phase Boot images.

## Test Environments

### QEMU (Primary - Automated)

QEMU testing is the primary method for automated testing:

```bash
# x86_64 test
make test-qemu-x86

# ARM64 test
make test-qemu-arm
```

**Requirements**:
- `qemu-system-x86_64` or `qemu-system-aarch64`
- OVMF firmware (`/usr/share/ovmf/OVMF.fd`)
- KVM for hardware acceleration (optional but recommended)

### Physical Hardware (Manual)

Hardware testing requires manual intervention:

1. **Write image to USB**:
   ```bash
   sudo ./scripts/write-usb.sh --image build/phase-boot-x86_64.img --device /dev/sdX
   ```

2. **Boot target machine**:
   - Enter BIOS/UEFI setup
   - Disable Secure Boot (temporarily)
   - Set USB as first boot device
   - Save and exit

3. **Validate boot**:
   - Boot menu should appear with 3 entries
   - Select a mode and verify boot

## Test Cases

### TC-001: Boot Menu Display

**Objective**: Verify boot menu appears with correct entries

**Steps**:
1. Boot image in QEMU or on hardware
2. Wait for systemd-boot menu

**Expected**:
- Menu appears within 5 seconds
- Shows 3 entries:
  - "Phase Boot — Internet Mode" (default)
  - "Phase Boot — Local Mode"
  - "Phase Boot — Private Mode"
- 10 second timeout countdown visible

**Status**: [ ] Untested

---

### TC-002: Internet Mode Boot

**Objective**: Verify Internet Mode boots to shell

**Steps**:
1. Select "Internet Mode" from boot menu
2. Wait for kernel to boot

**Expected**:
- Kernel loads without errors
- Init script runs
- Phase Boot banner displayed
- Mode shows: "internet"
- Network interfaces listed
- Shell prompt appears: `phase-boot#`

**Validation Commands**:
```bash
cat /proc/cmdline | grep phase.mode=internet
ip link show
which kexec
free -m  # Should show >200MB available
```

**Status**: [ ] Untested

---

### TC-003: Local Mode Boot

**Objective**: Verify Local Mode boots with cache enabled

**Steps**:
1. Select "Local Mode" from boot menu
2. Wait for boot

**Expected**:
- Mode shows: "local"
- Cache enabled message
- `/proc/cmdline` contains `phase.cache=enabled`

**Status**: [ ] Untested

---

### TC-004: Private Mode Boot

**Objective**: Verify Private Mode boots with write protection

**Steps**:
1. Select "Private Mode" from boot menu
2. Wait for boot

**Expected**:
- Mode shows: "private"
- No-write mode message
- `/proc/cmdline` contains `phase.nowrite=true`

**Status**: [ ] Untested

---

### TC-005: Network Initialization

**Objective**: Verify network comes up via DHCP

**Prerequisites**: Network available (QEMU user mode or physical network)

**Steps**:
1. Boot in Internet Mode
2. Check network status

**Validation Commands**:
```bash
ip addr show
ip route show
ping -c 3 8.8.8.8
```

**Expected**:
- At least one interface has IP address
- Default route exists
- Ping succeeds (if network available)

**Status**: [ ] Untested

---

### TC-006: kexec Binary Present

**Objective**: Verify kexec is available for later milestones

**Steps**:
1. Boot to shell
2. Check for kexec

**Validation Commands**:
```bash
which kexec
kexec --version
cat /proc/sys/kernel/kexec_load_disabled
```

**Expected**:
- `kexec` binary found in `/sbin/kexec`
- Version displayed
- kexec_load_disabled = 0

**Status**: [ ] Untested

---

### TC-007: Memory Availability

**Objective**: Verify sufficient RAM for fetch operations

**Steps**:
1. Boot to shell
2. Check memory

**Validation Commands**:
```bash
free -m
cat /proc/meminfo | grep MemAvailable
```

**Expected**:
- Available memory > 200MB

**Status**: [ ] Untested

---

### TC-008: Partition Layout

**Objective**: Verify disk partition structure

**Steps**:
1. Boot to shell
2. Examine partitions

**Validation Commands**:
```bash
lsblk
fdisk -l /dev/sda  # or appropriate device
```

**Expected**:
- GPT partition table
- Partition 1: ESP (~256MB, FAT32)
- Partition 2: Seed (~512MB)
- Partition 3: Cache (remaining)

**Status**: [ ] Untested

---

## Hardware Compatibility

### Tested Hardware

| Device | Arch | Status | Notes |
|--------|------|--------|-------|
| QEMU x86_64 | x86_64 | Pending | Primary test target |
| QEMU aarch64 | arm64 | Pending | Secondary test target |
| Intel NUC | x86_64 | Pending | Reference x86 hardware |
| Raspberry Pi 4 | arm64 | Pending | Requires UEFI firmware |

### Known Issues

*None documented yet*

## Automated Testing

### CI Integration

Future CI pipeline will:

1. Build images for both architectures
2. Boot in QEMU with timeout
3. Execute validation commands
4. Parse output for pass/fail
5. Generate test report

### Test Script (TODO)

```bash
#!/bin/bash
# Automated QEMU test harness
# TODO: Implement for M1 completion
```

## Troubleshooting

### Boot Menu Not Appearing

1. Verify OVMF firmware path
2. Check image was built correctly
3. Try GRUB fallback

### Kernel Panic

1. Check kernel config has required features
2. Verify initramfs is valid CPIO
3. Check init script is executable

### No Network

1. Verify QEMU netdev configured
2. Check kernel has network drivers
3. Verify DHCP client present

## Reporting Issues

When reporting test failures, include:

1. Test case ID (TC-XXX)
2. Environment (QEMU version, hardware model)
3. Full boot log
4. `/proc/cmdline` contents
5. Any error messages

File issues at: https://github.com/msitarzewski/phase/issues
