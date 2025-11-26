# Phase Boot ARM64 - Development Environment

**Status**: QEMU + vmnet-shared WORKING - VM can reach host services
**Target**: Fast iteration development on Apple Silicon
**Last Updated**: 2025-11-26

## Quick Start (Working Solution)

### Prerequisites
```bash
# macOS with Apple Silicon
brew install qemu

# Docker for building initramfs
# (Docker Desktop or colima)
```

### Step 1: Download Kernel (one time)
```bash
cd boot
make download-kernel ARCH=arm64
```

### Step 2: Build Initramfs (after init script changes)
```bash
cd boot
docker run --rm -v "$(pwd):/work" -w /work ubuntu:22.04 bash -c '
  apt-get update -qq && apt-get install -y -qq busybox-static cpio gzip >/dev/null 2>&1
  rm -rf build/initramfs-work && mkdir -p build/initramfs-work/{bin,sbin,dev,proc,sys,run,tmp,etc,lib/modules}
  cp initramfs/init build/initramfs-work/init && chmod +x build/initramfs-work/init
  cp /bin/busybox build/initramfs-work/bin/busybox && chmod +x build/initramfs-work/bin/busybox
  cd build/initramfs-work/bin && for cmd in $(/bin/busybox --list); do ln -sf busybox $cmd 2>/dev/null; done && cd /work
  cd build/initramfs-work/sbin && ln -sf ../bin/busybox modprobe && ln -sf ../bin/busybox insmod && cd /work
  KVER=$(ls build/kernel/modules/)
  for mod in af_packet virtio virtio_ring virtio_pci_modern_dev virtio_pci_legacy_dev virtio_pci failover net_failover virtio_net; do
    src=$(find build/kernel/modules -name "${mod}.ko.gz" 2>/dev/null | head -1)
    [ -n "$src" ] && gunzip -c "$src" > build/initramfs-work/lib/modules/$KVER/${mod}.ko
  done
  cd build/initramfs-work && find . -print0 | cpio --null -o --format=newc 2>/dev/null | gzip -9 > ../initramfs/initramfs-arm64.img'
```

### Step 3a: Boot with User Networking (isolated, no sudo)
```bash
qemu-system-aarch64 \
  -M virt -cpu host -accel hvf -m 1024 \
  -kernel build/kernel/vmlinuz-arm64 \
  -initrd build/initramfs/initramfs-arm64.img \
  -append "console=ttyAMA0 phase.mode=internet" \
  -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
  -nographic
```

### Step 3b: Boot with vmnet-shared (VM on host network, requires sudo)
```bash
sudo qemu-system-aarch64 \
  -M virt -cpu host -accel hvf -m 512 \
  -kernel build/kernel/vmlinuz-arm64 \
  -initrd build/initramfs/initramfs-arm64.img \
  -append "console=ttyAMA0 phase.mode=internet" \
  -netdev vmnet-shared,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -nographic
```

This puts the VM on the same network as your Mac (192.168.2.x via bridge102).
The VM can reach services running on your Mac.

Press `Ctrl+A X` to exit QEMU.

## What Works

### QEMU Direct Boot with Networking
- **Boot time**: ~2-3 seconds with HVF acceleration
- **User networking**: DHCP via virtio-net (IP: 10.0.2.15) - isolated
- **vmnet-shared**: VM gets real LAN IP (192.168.2.x) - can reach host
- **Console**: Full serial console via ttyAMA0
- **Init flow**: Complete Phase Boot init runs
  - Mounts filesystems
  - Loads kernel modules (af_packet, virtio stack)
  - Attempts phase-discover (placeholder for M2)
  - Drops to interactive shell

### POC: VM to Host Communication (Tested 2025-11-26)
With vmnet-shared networking:
1. VM boots and gets IP on 192.168.2.x subnet
2. Mac has 192.168.2.1 on bridge102 interface
3. VM can wget/curl services running on Mac
4. **Proven**: VM successfully fetched from Python HTTP server on host

### Kernel Modules Loaded
The init script automatically loads these modules in order:
1. `af_packet` - Required for DHCP (raw sockets)
2. `virtio`, `virtio_ring` - Virtio base
3. `virtio_pci_modern_dev`, `virtio_pci_legacy_dev` - PCI helpers
4. `virtio_pci` - Virtio PCI transport
5. `failover`, `net_failover` - Network failover support
6. `virtio_net` - Virtio network driver

## Development Workflow

### Edit-Test Cycle (~30 seconds)
1. Edit `initramfs/init` or other files
2. Rebuild initramfs (Step 2 above) - ~10 seconds
3. Boot QEMU (Step 3 above) - ~3 seconds
4. Test changes
5. Press `Ctrl+A X` to exit

### Debugging Tips
```bash
# In QEMU shell:
ip addr              # Check network config
ip route             # Check routing
cat /proc/cmdline    # Kernel command line
mount                # Check mounted filesystems
lsmod                # Loaded modules (if supported)
dmesg | head -50     # Kernel messages
```

---

## Known Issues

### udhcpc DHCP Config ✅ FIXED
**Status**: Fixed (2025-11-26)
**Solution**: Added `/usr/share/udhcpc/default.script` to initramfs

**Files created**:
- `boot/initramfs/usr/share/udhcpc/default.script` - DHCP callback script
- Updated `boot/initramfs/init` to use `-s /usr/share/udhcpc/default.script`

**Result**: DHCP now auto-configures IP, gateway, and DNS:
```
udhcpc[eth0]: Configuring: 10.0.2.15/255.255.255.0
udhcpc[eth0]: Adding gateway: 10.0.2.2
udhcpc[eth0]: Setting DNS: 10.0.2.3
Network: DHCP on eth0 (10.0.2.15)
```

### Parallels EFI Boot (Deferred)
**Status**: Kernel boots but hangs after "Exiting boot services..."
**Symptoms**:
- GRUB loads and shows menu
- Kernel decompresses successfully
- Initrd loads via LINUX_EFI_INITRD_MEDIA_GUID
- Hangs at "EFI stub: Exiting boot services..."

**Likely cause**: Console/framebuffer driver issue in Parallels ARM UEFI

**Tried**:
- Various console parameters (console=tty0, console=ttyAMA0)
- Alpine's GRUB binary (works in QEMU EFI, not Parallels)
- Multiple ISO structures

### EFI Boot Issues (QEMU works, Parallels doesn't)
| Approach | QEMU | Parallels |
|----------|------|-----------|
| Direct boot (-kernel) | Works | N/A |
| ISO with GRUB | Works | Hangs |
| ISO with Alpine GRUB | Works | Hangs |

---

## Files Modified

### `boot/initramfs/init`
- Added `load_modules()` function for virtio module loading
- Improved network setup with verbose output
- DHCP timeout configuration

### `boot/esp/EFI/BOOT/grub-arm64.cfg`
- Added search command for kernel partition
- Simple boot entries without complex module loading

---

## Architecture Notes

### Why QEMU Direct Boot?
QEMU's `-kernel` and `-initrd` options bypass the entire EFI/bootloader stack:
- Kernel loaded directly into memory
- Initrd loaded directly
- Command line passed via `-append`
- No EFI, no GRUB, no ISO structure needed

This is **faster and simpler** for development iteration.

### When You Need EFI Boot
EFI/ISO boot is only needed for:
- Testing actual USB boot experience
- Deploying to real hardware
- Testing bootloader configuration

For development, QEMU direct boot is superior.

---

## Next Steps

1. **Fix udhcpc**: Add default.script to initramfs for automatic DHCP configuration
2. **Test with Plasm**: Run Plasm daemon on Mac, have VM discover and execute job
3. **Build phase-discover ARM64**: Cross-compile discovery binary for initramfs
4. **Later**: Investigate Parallels console issue if needed for USB testing

---

**Owner**: Michael S.
**Created**: 2025-11-26
**Working**: QEMU with vmnet-shared networking, VM-to-host communication proven
