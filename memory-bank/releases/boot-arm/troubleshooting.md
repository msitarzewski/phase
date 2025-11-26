# Phase Boot ARM64 Troubleshooting

## Build Issues

### "No kernel found" for ARM64

```
→ Preparing ARM64 kernel...
  No kernel found at: build/kernel/vmlinuz-arm64

  Please run the kernel download script first:
    ./scripts/download-kernel.sh --arch arm64
```

**Solution**:
```bash
./scripts/download-kernel.sh --arch arm64
```

### GRUB ARM64 Build Fails

```
Building GRUB EFI bootloader (ARM64)...
grub-mkstandalone: error: unable to install for arm64-efi
```

**Causes**:
1. Missing grub-efi-arm64-bin package
2. Cross-compilation not set up

**Solution on Ubuntu**:
```bash
sudo apt-get install grub-efi-arm64-bin
```

**Solution on macOS** (use Linux VM):
```bash
# GRUB can't be built natively on macOS
# Build in a Linux VM or use Docker
docker run -v $(pwd):/work ubuntu:22.04 bash -c "
    apt-get update && \
    apt-get install -y grub-efi-arm64-bin make && \
    cd /work && make esp ARCH=arm64"
```

### Cross-compilation Errors

```
aarch64-linux-gnu-gcc: command not found
```

**Solution**:
```bash
# Ubuntu/Debian
sudo apt-get install gcc-aarch64-linux-gnu

# macOS
brew install aarch64-elf-gcc
```

---

## Parallels Issues

### "Invalid architecture"

```
prlctl: error: Invalid architecture specified
```

**Cause**: Parallels version doesn't support ARM64

**Solution**:
- Ensure Parallels Desktop 17+ (for Apple Silicon)
- Check: `prlctl --version`

### "Cannot find UEFI firmware"

```
VM failed to start: UEFI firmware not found
```

**Solution**:
```bash
# Verify EFI is enabled
prlctl set "Phase Boot ARM64" --efi-boot on

# Check Parallels installation
ls /Applications/Parallels\ Desktop.app/Contents/Resources/Tools/
```

### VM Starts But Black Screen

**Causes**:
1. No video device
2. Kernel panic before display init
3. Wrong console parameter

**Solutions**:
```bash
# Add video device
prlctl set "Phase Boot ARM64" --device-add video --videosize 64

# Check serial output
tail -f /tmp/phase-boot-serial.log

# Verify kernel cmdline includes console
# In grub.cfg: console=ttyAMA0,115200
```

### VM Hangs at UEFI

**Cause**: Disk not recognized as bootable

**Solutions**:
```bash
# Verify disk has GPT partition table
fdisk -l phase-boot-arm64.img

# Verify ESP partition exists and is marked bootable
gdisk -l phase-boot-arm64.img
# Should show: EF00 (EFI System)

# Rebuild image
make clean
make all ARCH=arm64
```

### Network Not Working

**Symptoms**: No IP address, ping fails

**Check VM network config**:
```bash
prlctl list "Phase Boot ARM64" --info | grep -A10 "net"
```

**Check in-VM**:
```bash
# If you can get a shell
ip addr
ip route
cat /etc/resolv.conf
```

**Common fixes**:
```bash
# Reset network adapter
prlctl set "Phase Boot ARM64" --device-del net0
prlctl set "Phase Boot ARM64" --device-add net --type shared --adapter-type virtio

# Or use bridged mode
prlctl set "Phase Boot ARM64" --device-set net0 --type bridged --iface en0
```

---

## Boot Issues

### "VFS: Cannot open root device"

```
VFS: Cannot open root device "(null)" or unknown-block(0,0)
Kernel panic - not syncing: VFS: Unable to mount root fs
```

**Cause**: Kernel can't find initramfs or root device

**Solutions**:
1. Verify initramfs is in ESP:
   ```bash
   ls build/esp/initramfs-arm64.img
   ```

2. Check grub.cfg paths:
   ```bash
   grep initrd build/esp/EFI/BOOT/grub.cfg
   # Should be: initrd /initramfs-arm64.img
   ```

3. Rebuild initramfs:
   ```bash
   make initramfs ARCH=arm64
   make esp ARCH=arm64
   ```

### Init Script Fails

```
/init: line X: <command>: not found
```

**Cause**: Missing busybox or symlinks

**Solution**:
```bash
# Verify busybox is in initramfs
cd build/initramfs/work
ls -la bin/busybox
ls -la bin/sh  # Should be symlink to busybox

# Rebuild
make initramfs ARCH=arm64
```

### Mount Failures

```
mount: mounting tmpfs on /run failed: No such file or directory
```

**Cause**: Mount point directories don't exist

**Solution**: Already fixed in latest init - it creates directories before mounting.
```bash
git pull
make clean
make all ARCH=arm64
```

### "kernel too old"

```
FATAL: kernel too old
```

**Cause**: Kernel doesn't meet glibc requirements (usually with Alpine musl this doesn't happen, but with other distros it might)

**Solution**: Use newer kernel
```bash
./scripts/download-kernel.sh --arch arm64 --version edge
```

---

## Serial Console Issues

### No Output on Serial

**Check kernel cmdline**:
```bash
grep console build/esp/EFI/BOOT/grub.cfg
# ARM64 should have: console=ttyAMA0,115200
```

**Verify serial device in Parallels**:
```bash
prlctl list "Phase Boot ARM64" --info | grep serial
```

**Correct serial config**:
```bash
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --output /tmp/phase-boot-serial.log
```

### Garbled Serial Output

**Cause**: Baud rate mismatch

**Fix in grub.cfg**:
```
linux /vmlinuz-arm64 console=ttyAMA0,115200 ...
```

**Fix in Parallels** (if using socket):
- Ensure terminal uses 115200 baud

### Serial File Empty

**Cause**: VM didn't boot far enough

**Debug**:
```bash
# Check VM status
prlctl status "Phase Boot ARM64"

# Check if VM is running
prlctl list -a

# Try graphical mode to see where it's stuck
prlctl set "Phase Boot ARM64" --device-add video
prlctl start "Phase Boot ARM64"
```

---

## Performance Issues

### VM Boot Very Slow

**Possible causes**:
1. Using emulation instead of virtualization
2. Insufficient resources

**Solutions**:
```bash
# Ensure using ARM64 (virtualized)
prlctl list "Phase Boot ARM64" --info | grep arch
# Should show: arm64

# Increase resources
prlctl set "Phase Boot ARM64" --memsize 2048 --cpus 2

# Disable unnecessary features
prlctl set "Phase Boot ARM64" \
    --faster-vm on \
    --3d-accelerate off
```

### Disk I/O Slow

**Solutions**:
```bash
# Convert to Parallels format
qemu-img convert -f raw -O parallels \
    phase-boot-arm64.img phase-boot.hdd

# Re-attach
prlctl set "Phase Boot ARM64" --device-del hdd0
prlctl set "Phase Boot ARM64" --device-add hdd --image phase-boot.hdd
```

---

## Quick Diagnostic Commands

```bash
# Full VM info
prlctl list "Phase Boot ARM64" --info

# Check Parallels service
prlsrvctl info

# VM problem report
prlctl problem-report "Phase Boot ARM64" --dump > vm-diag.txt

# Serial log
cat /tmp/phase-boot-serial.log

# Disk image info
file phase-boot-arm64.img
fdisk -l phase-boot-arm64.img

# ESP contents
ls -la build/esp/
ls -la build/esp/EFI/BOOT/
```

---

## Getting Help

If issues persist:

1. Capture serial output: `cat /tmp/phase-boot-serial.log`
2. Get VM diagnostics: `prlctl problem-report "Phase Boot ARM64" --dump`
3. Check build output: `make all ARCH=arm64 2>&1 | tee build.log`
4. Open issue with logs attached
