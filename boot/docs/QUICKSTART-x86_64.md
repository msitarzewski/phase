# Phase Boot Quickstart - x86_64

This guide walks you through building, testing, and deploying Phase Boot on x86_64 hardware.

## Prerequisites

### Build Host Requirements

- Linux host (Ubuntu 22.04+ or similar)
- At least 2GB free disk space
- Build tools:
  ```bash
  sudo apt-get update
  sudo apt-get install -y build-essential git wget curl \
    dosfstools mtools xorriso syslinux-common grub-efi-amd64-bin \
    qemu-system-x86 ovmf
  ```

### Target Hardware Requirements

- x86_64 CPU (Intel/AMD)
- UEFI firmware (recommended) or legacy BIOS
- 512MB RAM minimum, 1GB+ recommended
- USB drive (1GB+) or hard disk
- Network connectivity for internet boot mode

## Building Phase Boot

### 1. Clone and Build

```bash
# Navigate to Phase repository
cd /home/user/phase

# Build everything for x86_64
make -C boot all

# Expected output:
# - boot/esp/EFI/BOOT/BOOTX64.EFI (UEFI bootloader)
# - boot/esp/vmlinuz-phase (kernel)
# - boot/initramfs.cpio.gz (initramfs with Phase discovery tools)
```

Build artifacts:
- **ESP image**: `boot/esp/` - UEFI system partition contents
- **Initramfs**: `boot/initramfs.cpio.gz` - Initial RAM filesystem
- **Kernel**: `boot/esp/vmlinuz-phase` - Linux kernel

### 2. Verify Build

```bash
# Check all required files exist
ls -lh boot/esp/EFI/BOOT/BOOTX64.EFI
ls -lh boot/esp/vmlinuz-phase
ls -lh boot/initramfs.cpio.gz

# Check sizes (approximate)
# BOOTX64.EFI: ~100KB
# vmlinuz-phase: ~8-15MB
# initramfs.cpio.gz: ~20-50MB
```

## Testing in QEMU

### Quick Test

```bash
# Run QEMU test (UEFI mode)
make -C boot test-qemu

# This launches QEMU with:
# - 2GB RAM
# - OVMF UEFI firmware
# - Network in user mode
# - VGA console
```

### Manual QEMU Launch

```bash
# Using the helper script
boot/scripts/test-qemu-x86.sh

# Or manually with more control:
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic
```

### Expected QEMU Output

1. UEFI firmware initializes
2. GRUB bootloader loads
3. Linux kernel boots
4. Initramfs executes Phase discovery:
   ```
   Phase Boot v0.1
   Mode: internet (default)
   Discovering Phase peers...
   ```

### Interactive Testing

Press `e` in GRUB to edit boot parameters:
```
linux /vmlinuz-phase phase.mode=local phase.loglevel=debug
initrd /initramfs.cpio.gz
```

## Writing to USB Drive

### CAUTION
**Writing to the wrong device will destroy data. Verify your device carefully.**

### 1. Identify USB Device

```bash
# Before inserting USB
lsblk

# Insert USB drive, then
lsblk

# Look for new device (e.g., /dev/sdb, /dev/sdc)
# Verify by size and vendor
sudo dmesg | tail -20
```

### 2. Write to USB

```bash
# Replace /dev/sdX with your actual device
sudo boot/scripts/write-usb.sh /dev/sdX

# The script:
# 1. Creates GPT partition table
# 2. Creates EFI system partition
# 3. Formats as FAT32
# 4. Copies ESP contents
# 5. Installs GRUB bootloader
```

### 3. Manual Method (Alternative)

```bash
# Partition the drive
sudo parted /dev/sdX mklabel gpt
sudo parted /dev/sdX mkpart ESP fat32 1MiB 512MiB
sudo parted /dev/sdX set 1 esp on

# Format
sudo mkfs.vfat -F32 /dev/sdX1

# Mount and copy
sudo mkdir -p /mnt/phase-boot
sudo mount /dev/sdX1 /mnt/phase-boot
sudo cp -r boot/esp/* /mnt/phase-boot/
sudo umount /mnt/phase-boot

# Install GRUB
sudo grub-install --target=x86_64-efi \
  --efi-directory=/mnt/phase-boot \
  --boot-directory=/mnt/phase-boot \
  --removable /dev/sdX
```

### 4. Verify USB Contents

```bash
sudo mount /dev/sdX1 /mnt/phase-boot
ls -R /mnt/phase-boot
sudo umount /mnt/phase-boot

# Expected structure:
# /EFI/BOOT/BOOTX64.EFI
# /vmlinuz-phase
# /initramfs.cpio.gz
# /grub/grub.cfg
```

## Boot Modes

Phase Boot supports three discovery modes via kernel command line:

### Internet Mode (Default)

```bash
phase.mode=internet
```

- Discovers Phase peers via public internet
- Uses DNS, HTTP, or distributed discovery
- Fetches manifests from discovered peers
- Verifies signatures against trusted keys
- Downloads and kexec's into verified system

**Use case**: Public Phase network participation

### Local Mode

```bash
phase.mode=local
```

- Discovers peers on local network only
- Uses mDNS/Avahi for discovery
- Ideal for development and testing
- No internet connectivity required

**Use case**: Private lab, development environment

### Private Mode

```bash
phase.mode=private
```

- Manual peer configuration
- No automatic discovery
- Requires pre-configured peer list
- Maximum security isolation

**Use case**: Air-gapped deployments, maximum security

### Setting Boot Mode

**In GRUB** (interactive):
1. Press `e` to edit boot entry
2. Add `phase.mode=local` to kernel line
3. Press `Ctrl-X` to boot

**In grub.cfg** (persistent):
```bash
# Edit boot/esp/grub/grub.cfg
menuentry "Phase Boot - Local Mode" {
    linux /vmlinuz-phase phase.mode=local
    initrd /initramfs.cpio.gz
}
```

## First Boot Experience

### 1. Insert USB and Power On

- Set UEFI boot order to USB first
- Or press F12/F11/ESC for boot menu

### 2. GRUB Menu

```
Phase Boot v0.1
================
1. Phase Boot (Internet Mode)
2. Phase Boot (Local Mode)
3. Phase Boot (Private Mode)
4. UEFI Shell
```

### 3. Discovery Phase

```
Phase Boot v0.1
Mode: internet
Network: eth0 UP 192.168.1.100
Discovering Phase peers...
Found 5 peers:
  - peer-a.phase.network (verified)
  - peer-b.phase.network (verified)
  - peer-c.phase.network (verified)
Fetching manifest from peer-a.phase.network...
```

### 4. Verification Phase

```
Manifest: system-image-v1.2.3
Signature: VALID (trusted key: 0xABCD...)
Artifacts:
  - kernel.img (15.2 MB, sha256: abc123...)
  - rootfs.img (500.8 MB, sha256: def456...)
Downloading artifacts...
```

### 5. Kexec into System

```
Verifying checksums... OK
Preparing kexec...
Executing new kernel...

[System boots into verified Phase OS]
```

## Common Troubleshooting

### Build Issues

**Problem**: `make -C boot all` fails with missing tools

**Solution**:
```bash
# Install all dependencies
sudo apt-get install -y build-essential git wget curl \
  dosfstools mtools xorriso syslinux-common grub-efi-amd64-bin
```

**Problem**: Permission denied errors

**Solution**:
```bash
# Ensure you own the phase directory
sudo chown -R $USER:$USER /home/user/phase
```

### QEMU Issues

**Problem**: QEMU shows black screen

**Solution**:
```bash
# Remove -nographic flag or try
qemu-system-x86_64 ... -serial stdio -display gtk
```

**Problem**: OVMF firmware not found

**Solution**:
```bash
# Install OVMF
sudo apt-get install ovmf

# Find firmware path
find /usr/share -name "OVMF*.fd"

# Update QEMU command with correct path
```

### USB Boot Issues

**Problem**: USB doesn't appear in UEFI boot menu

**Solution**:
1. Verify USB is formatted with GPT and ESP flag
2. Check UEFI settings - enable USB boot
3. Try different USB port (USB 2.0 vs 3.0)
4. Re-run `write-usb.sh` script

**Problem**: "No bootable device" error

**Solution**:
```bash
# Reinstall GRUB with --removable flag
sudo mount /dev/sdX1 /mnt/phase-boot
sudo grub-install --target=x86_64-efi \
  --efi-directory=/mnt/phase-boot \
  --removable /dev/sdX
```

### Network Discovery Issues

**Problem**: "No peers found"

**Solution**:
1. Check network connectivity:
   ```bash
   # In Phase Boot shell (press Ctrl-C during boot)
   ip addr show
   ping 8.8.8.8
   ```
2. Try local mode if on isolated network:
   ```bash
   phase.mode=local
   ```
3. Check firewall rules on network

**Problem**: "Signature verification failed"

**Solution**:
1. Check system clock is correct
2. Verify trusted keys in `/etc/phase/trusted-keys/`
3. Enable debug logging:
   ```bash
   phase.mode=internet phase.loglevel=debug
   ```

### Debug Mode

Enable verbose logging:
```bash
# Add to kernel command line
phase.loglevel=debug console=ttyS0,115200
```

Access emergency shell:
```bash
# Add to kernel command line
phase.shell=emergency

# Or press Ctrl-C during boot to drop to shell
```

## Next Steps

- **Configuration**: See `boot/docs/CONFIGURATION.md` for advanced options
- **Development**: See `boot/docs/DEVELOPMENT.md` for building custom initramfs
- **Security**: See `boot/docs/THREAT-MODEL.md` for security architecture
- **ARM64**: See `QUICKSTART-ARM64.md` for ARM deployment
- **VMs**: See `QUICKSTART-VM.md` for virtual machine setup

## Reference

### Key Files

- `boot/Makefile` - Build system
- `boot/scripts/write-usb.sh` - USB writing script
- `boot/scripts/test-qemu-x86.sh` - QEMU test script
- `boot/esp/grub/grub.cfg` - GRUB configuration
- `boot/initramfs/init` - Initramfs init script

### Boot Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `phase.mode` | internet | Discovery mode: internet/local/private |
| `phase.loglevel` | info | Log level: debug/info/warn/error |
| `phase.shell` | - | Drop to shell: emergency/always |
| `phase.peers` | - | Manual peer list (private mode) |
| `phase.timeout` | 300 | Discovery timeout (seconds) |

### Useful Commands

```bash
# Rebuild only initramfs
make -C boot initramfs

# Rebuild only kernel
make -C boot kernel

# Clean build artifacts
make -C boot clean

# Full rebuild
make -C boot clean all

# Update USB without reformatting
sudo mount /dev/sdX1 /mnt/phase-boot
sudo cp boot/initramfs.cpio.gz /mnt/phase-boot/
sudo umount /mnt/phase-boot
```
