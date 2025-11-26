# Parallels Desktop Configuration for Phase Boot

## Optimal VM Settings

### Hardware Configuration

```bash
# Create VM with optimal settings
prlctl create "Phase Boot ARM64" --ostype linux --arch arm64

# CPU & Memory
prlctl set "Phase Boot ARM64" \
    --memsize 2048 \
    --cpus 2 \
    --cpu-hotplug off \
    --memquota auto

# Boot Configuration
prlctl set "Phase Boot ARM64" \
    --efi-boot on \
    --efi-secure-boot off \
    --select-boot-device off

# Storage
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --image ~/Software/phase/boot/build/phase-boot-arm64.img \
    --type plain \
    --position 0

# Network
prlctl set "Phase Boot ARM64" \
    --device-add net \
    --type shared \
    --adapter-type virtio

# Serial Port (for debugging)
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --output /tmp/phase-boot-serial.log

# Disable unnecessary devices
prlctl set "Phase Boot ARM64" \
    --device-del sound0 \
    --device-del usb
```

### Display Configuration

For headless operation (serial console only):

```bash
# Remove video device for faster boot
prlctl set "Phase Boot ARM64" --device-del video0

# Or minimize video resources
prlctl set "Phase Boot ARM64" \
    --videosize 16 \
    --3d-accelerate off
```

For graphical debugging:

```bash
# Enable basic video
prlctl set "Phase Boot ARM64" \
    --device-add video \
    --videosize 64 \
    --3d-accelerate off
```

---

## Network Modes

### Shared Network (NAT) - Default

```bash
prlctl set "Phase Boot ARM64" \
    --device-set net0 \
    --type shared

# VM gets IP via DHCP from Parallels NAT
# Internet access: Yes
# Host access: Yes (via gateway)
# LAN access: No
# Good for: Internet mode testing
```

### Bridged Network

```bash
prlctl set "Phase Boot ARM64" \
    --device-set net0 \
    --type bridged \
    --iface en0

# VM on same network as Mac
# Internet access: Yes
# Host access: Yes
# LAN access: Yes
# Good for: Local mode testing, peer discovery
```

### Host-Only Network

```bash
# Create host-only network
prlsrvctl net add HostOnly \
    --type host-only \
    --ip 10.37.129.1 \
    --dhcp yes

# Attach VM
prlctl set "Phase Boot ARM64" \
    --device-set net0 \
    --type host-only \
    --iface vnic0

# VM isolated with host
# Internet access: No
# Host access: Yes
# LAN access: No
# Good for: Private mode testing
```

---

## Disk Image Formats

### Raw Image (Current)

```bash
# Direct raw image attachment
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --image phase-boot-arm64.img \
    --type plain
```

**Pros**: Simple, no conversion needed
**Cons**: No snapshots, no compression

### Parallels HDD Format

```bash
# Convert raw to Parallels format
qemu-img convert -f raw -O parallels \
    phase-boot-arm64.img \
    phase-boot-arm64.hdd

# Attach converted image
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --image phase-boot-arm64.hdd
```

**Pros**: Snapshots, better performance
**Cons**: Conversion step required

### Expandable Disk

```bash
# Create expandable disk
prlctl set "Phase Boot ARM64" \
    --device-add hdd \
    --size 10240 \
    --type expand

# Then install Phase Boot to it
# (Requires more setup)
```

---

## Shared Folders for Fast Iteration

### Setup

```bash
# Enable shared folders
prlctl set "Phase Boot ARM64" \
    --shf-host on

# Share specific directory
prlctl set "Phase Boot ARM64" \
    --shf-host-defined ~/Software/phase/boot/build/esp

# Or share entire boot directory
prlctl set "Phase Boot ARM64" \
    --shf-host-defined ~/Software/phase/boot
```

### Access in VM

```bash
# In the booted VM, mount shared folder
mkdir -p /mnt/host
mount -t 9p -o trans=virtio host0 /mnt/host

# Access ESP contents
ls /mnt/host/esp/

# Or for auto-mount, add to /etc/fstab:
# host0 /mnt/host 9p trans=virtio,version=9p2000.L 0 0
```

### Workflow

```bash
# On Mac: Edit and rebuild
vim boot/initramfs/init
make initramfs ARCH=arm64

# In VM: Remount to see changes
mount -o remount /mnt/host

# Or just reboot the VM
# Changes are immediately visible
```

---

## Serial Console Options

### File Output (Simple)

```bash
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --output /tmp/phase-boot-serial.log

# Monitor
tail -f /tmp/phase-boot-serial.log
```

### Socket (Interactive)

```bash
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --socket /tmp/phase-boot.sock

# Connect interactively
socat - UNIX-CONNECT:/tmp/phase-boot.sock

# Or with screen
screen /tmp/phase-boot.sock
```

### Pipe to Terminal

```bash
prlctl set "Phase Boot ARM64" \
    --device-add serial \
    --pipe /tmp/phase-boot.pipe

# Create and connect
mkfifo /tmp/phase-boot.pipe
cat /tmp/phase-boot.pipe
```

---

## Snapshots

### Create Snapshot

```bash
# After successful boot, create baseline
prlctl snapshot "Phase Boot ARM64" \
    --name "working-boot" \
    --description "Phase Boot successfully booting"
```

### Restore Snapshot

```bash
# Revert to known-good state
prlctl snapshot-switch "Phase Boot ARM64" \
    --name "working-boot"
```

### List Snapshots

```bash
prlctl snapshot-list "Phase Boot ARM64"
```

---

## Performance Tuning

### Optimize for Boot Speed

```bash
prlctl set "Phase Boot ARM64" \
    --cpus 2 \
    --memsize 2048 \
    --cpu-hotplug off \
    --faster-vm on \
    --startup-view headless \
    --on-shutdown close \
    --autostart off
```

### Minimize Resource Usage

```bash
prlctl set "Phase Boot ARM64" \
    --cpus 1 \
    --memsize 1024 \
    --device-del sound0 \
    --device-del usb \
    --3d-accelerate off
```

---

## Troubleshooting

### VM Won't Start

```bash
# Check VM status
prlctl status "Phase Boot ARM64"

# Check for errors
prlctl problem-report "Phase Boot ARM64" --dump

# Reset VM
prlctl reset "Phase Boot ARM64"
```

### No Serial Output

```bash
# Verify serial device
prlctl list "Phase Boot ARM64" --info | grep -A5 serial

# Check kernel has serial enabled
# In grub.cfg, ensure: console=ttyAMA0,115200
```

### Network Not Working

```bash
# Check network device
prlctl list "Phase Boot ARM64" --info | grep -A5 net

# In VM, check interfaces
ip addr
ip route

# Check Parallels network service
prlsrvctl net list
```

### Disk Not Detected

```bash
# Verify disk attachment
prlctl list "Phase Boot ARM64" --info | grep -A5 hdd

# Check disk format
file phase-boot-arm64.img
# Should show: DOS/MBR boot sector or similar
```

---

## Command Reference

```bash
# VM Lifecycle
prlctl create "Name" --ostype linux --arch arm64
prlctl start "Name"
prlctl stop "Name"
prlctl restart "Name"
prlctl suspend "Name"
prlctl resume "Name"
prlctl reset "Name"
prlctl delete "Name"

# Configuration
prlctl set "Name" --option value
prlctl list "Name" --info

# Snapshots
prlctl snapshot "Name" --name "snap1"
prlctl snapshot-list "Name"
prlctl snapshot-switch "Name" --name "snap1"
prlctl snapshot-delete "Name" --name "snap1"

# Network
prlsrvctl net list
prlsrvctl net add NetName --type shared
prlsrvctl net set NetName --option value
prlsrvctl net del NetName

# Service
prlsrvctl info
prlsrvctl restart
```
