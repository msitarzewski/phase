# Phase Boot ARM64 - Parallels Quickstart

**Goal**: Boot Phase Boot in Parallels on Apple Silicon in under 15 minutes.

---

## Step 1: Prerequisites

### On your Mac (Apple Silicon)

```bash
# 1. Install Parallels Desktop
# Download from: https://www.parallels.com/products/desktop/trial/

# 2. Install Homebrew if not present
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# 3. Install required tools
brew install qemu coreutils wget
```

### Verify Parallels Installation

```bash
# Check prlctl is available
prlctl --version
# Expected: prlctl version X.X.X

# Check virtualization support
prlsrvctl info | grep "Hardware virtualization"
# Expected: Hardware virtualization supported
```

---

## Step 2: Build ARM64 Image

### Option A: Build on Linux (Recommended)

If you have an Ubuntu machine or VM:

```bash
cd ~/Software/phase/boot

# Install dependencies
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    squashfs-tools \
    dosfstools \
    gdisk \
    cpio \
    gzip \
    wget \
    busybox-static \
    grub-efi-arm64-bin

# Download ARM64 kernel
./scripts/download-kernel.sh --arch arm64

# Build everything for ARM64
make clean
make all ARCH=arm64

# Output: build/phase-boot-arm64.img
```

### Option B: Build on macOS (Cross-compile)

```bash
cd ~/Software/phase/boot

# This requires a Linux VM or Docker for some steps
# The Makefile will guide you through requirements

./scripts/download-kernel.sh --arch arm64
make all ARCH=arm64
```

---

## Step 3: Create Parallels VM

### Method 1: GUI (Easiest)

1. **Open Parallels Desktop**

2. **File → New**

3. **Install Windows, Linux, or macOS from an image**
   - Click "Continue"

4. **Select Image**
   - Click "Choose Manually"
   - Navigate to `~/Software/phase/boot/build/`
   - Select `phase-boot-arm64.img`
   - (Change file filter to "All Files" if needed)

5. **Configure VM**
   - Name: `Phase Boot ARM64`
   - Location: Default
   - Click "Create"

6. **Before Starting - Configure Settings**
   - Right-click VM → "Configure..."
   - **Hardware → Boot Order**: Hard Disk first
   - **Hardware → Boot Flags**: Enable "EFI Secure Boot" → OFF
   - **Hardware → Serial Port**: Add serial port → Output to file
   - **Options → Sharing**: Enable "Share Mac folders"
   - Click "OK"

7. **Start VM**
   - Click "Start"
   - Watch serial console for boot messages

### Method 2: Command Line

```bash
# Create VM
prlctl create "Phase Boot ARM64" \
    --ostype linux \
    --arch arm64

# Configure VM
prlctl set "Phase Boot ARM64" \
    --memsize 2048 \
    --cpus 2 \
    --efi-boot on \
    --efi-secure-boot off

# Add disk image
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --image ~/Software/phase/boot/build/phase-boot-arm64.img \
    --type plain \
    --position 0

# Add serial port for debugging
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --output /tmp/phase-boot-serial.log

# Add network
prlctl set "Phase Boot ARM64" \
    --device-add net \
    --type shared

# Start VM
prlctl start "Phase Boot ARM64"

# View serial output
tail -f /tmp/phase-boot-serial.log
```

---

## Step 4: Fast Iteration Workflow

Once the VM is set up, here's your rapid development cycle:

### Edit → Build → Test (Target: 30 seconds)

```bash
# Terminal 1: Edit files
vim boot/initramfs/init

# Terminal 2: Rebuild and restart
make initramfs ARCH=arm64 && \
cp build/initramfs/initramfs-arm64.img build/esp/initramfs-arm64.img && \
prlctl restart "Phase Boot ARM64"

# Terminal 3: Watch serial output
tail -f /tmp/phase-boot-serial.log
```

### Even Faster: Shared Folder Method

```bash
# 1. Configure Parallels to share the ESP directory
prlctl set "Phase Boot ARM64" \
    --shf-host on \
    --shf-host-defined ~/Software/phase/boot/build/esp

# 2. In VM (once booted), mount shared folder
mount -t 9p -o trans=virtio,version=9p2000.L host0 /mnt/esp

# 3. Now edits on Mac are instantly visible in VM
# Just reboot the VM to apply changes
```

---

## Step 5: Debugging

### Serial Console

```bash
# Real-time serial output
tail -f /tmp/phase-boot-serial.log

# Interactive serial console (if using socket)
prlctl set "Phase Boot ARM64" \
    --device-del serial0
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --socket /tmp/phase-boot.sock

# Connect
socat - UNIX-CONNECT:/tmp/phase-boot.sock
```

### Boot Failure Analysis

```bash
# Check VM status
prlctl status "Phase Boot ARM64"

# View execution stats
prlctl exec "Phase Boot ARM64" cat /proc/cmdline

# Dump VM log
prlctl problem-report "Phase Boot ARM64" --dump
```

### Common Issues

| Symptom | Cause | Fix |
|---------|-------|-----|
| VM won't start | Wrong architecture | Ensure `--arch arm64` |
| "No bootable device" | EFI not enabled | Enable EFI boot in settings |
| Black screen | Serial only | Add `--device-add video` |
| Network not working | Missing driver | Check virtio drivers in kernel |

---

## Step 6: Cleanup

```bash
# Stop VM
prlctl stop "Phase Boot ARM64"

# Delete VM (preserves disk image)
prlctl delete "Phase Boot ARM64"

# Full cleanup (deletes everything)
prlctl delete "Phase Boot ARM64" --keep-memfile
rm -rf ~/Parallels/Phase\ Boot\ ARM64.pvm
```

---

## Quick Reference

```bash
# Create and configure VM
prlctl create "Phase Boot ARM64" --ostype linux --arch arm64
prlctl set "Phase Boot ARM64" --memsize 2048 --cpus 2 --efi-boot on

# Manage VM
prlctl start "Phase Boot ARM64"
prlctl stop "Phase Boot ARM64"
prlctl restart "Phase Boot ARM64"
prlctl status "Phase Boot ARM64"

# Serial console
tail -f /tmp/phase-boot-serial.log

# Build commands
make download-kernel ARCH=arm64
make all ARCH=arm64
make initramfs ARCH=arm64
make esp ARCH=arm64

# Full rebuild cycle
make clean && make download-kernel ARCH=arm64 && make all ARCH=arm64
```

---

## Next Steps

After successfully booting:

1. **Test network discovery**: `phase-discover --channel stable`
2. **Test verification**: `phase-verify --manifest test.json`
3. **Test full boot flow**: Watch init script execute
4. **Profile boot time**: Measure each phase
5. **Fix issues**: Edit, rebuild, reboot, repeat

---

**Estimated Setup Time**: 15-30 minutes (first time)
**Iteration Time**: 30-60 seconds (after setup)
