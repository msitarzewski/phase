# Phase Boot Quickstart - Virtual Machines

This guide covers running Phase Boot in virtual machines across different platforms: QEMU, VMware, VirtualBox, and UTM (Apple Silicon).

## QEMU

QEMU is the recommended VM platform for Phase Boot development and testing.

### QEMU x86_64

#### Quick Start

```bash
# Build Phase Boot
make -C boot all

# Test in QEMU
make -C boot test-qemu

# Or use the helper script
boot/scripts/test-qemu-x86.sh
```

#### Manual QEMU Launch

```bash
# Basic UEFI boot
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic

# With KVM acceleration (Linux host)
qemu-system-x86_64 \
  -machine q35,accel=kvm \
  -cpu host \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic

# With graphical output
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -vga std \
  -display gtk
```

#### QEMU with Disk Image

```bash
# Create disk image
qemu-img create -f qcow2 phase-boot.qcow2 10G

# Partition and format
sudo modprobe nbd
sudo qemu-nbd -c /dev/nbd0 phase-boot.qcow2
sudo parted /dev/nbd0 mklabel gpt
sudo parted /dev/nbd0 mkpart ESP fat32 1MiB 512MiB
sudo parted /dev/nbd0 set 1 esp on
sudo mkfs.vfat -F32 /dev/nbd0p1

# Install Phase Boot
sudo mount /dev/nbd0p1 /mnt/phase-boot
sudo cp -r boot/esp/* /mnt/phase-boot/
sudo grub-install --target=x86_64-efi \
  --efi-directory=/mnt/phase-boot \
  --boot-directory=/mnt/phase-boot \
  --removable /dev/nbd0
sudo umount /mnt/phase-boot
sudo qemu-nbd -d /dev/nbd0

# Boot from disk image
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=qcow2,file=phase-boot.qcow2 \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic
```

### QEMU ARM64

#### Prerequisites

```bash
# Install ARM64 QEMU
sudo apt-get install qemu-system-aarch64 qemu-efi-aarch64

# Download UEFI firmware
sudo mkdir -p /usr/share/qemu-efi-aarch64
sudo wget -O /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  https://releases.linaro.org/components/kernel/uefi-linaro/latest/release/qemu64/QEMU_EFI.fd
```

#### Launch ARM64 VM

```bash
# Build ARM64 Phase Boot
make -C boot all ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-

# Test in QEMU ARM64
qemu-system-aarch64 \
  -machine virt \
  -cpu cortex-a57 \
  -m 2048 \
  -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic

# With KVM (ARM64 host only)
qemu-system-aarch64 \
  -machine virt,gic-version=3 \
  -cpu host \
  -enable-kvm \
  -m 2048 \
  -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic
```

### QEMU Network Configuration

#### User Mode (Default)

```bash
# Outbound only, no inbound connections
-netdev user,id=net0 \
-device virtio-net-pci,netdev=net0

# With port forwarding (SSH example)
-netdev user,id=net0,hostfwd=tcp::2222-:22 \
-device virtio-net-pci,netdev=net0
```

#### Bridged Network

```bash
# Create bridge (one-time setup)
sudo ip link add br0 type bridge
sudo ip addr add 192.168.100.1/24 dev br0
sudo ip link set br0 up

# Allow QEMU to use bridge
sudo mkdir -p /etc/qemu
echo "allow br0" | sudo tee /etc/qemu/bridge.conf
sudo chmod u+s /usr/lib/qemu/qemu-bridge-helper

# Launch with bridged network
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev bridge,id=net0,br=br0 \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic
```

#### TAP Network

```bash
# Create TAP interface
sudo ip tuntap add dev tap0 mode tap
sudo ip link set tap0 up
sudo ip addr add 192.168.200.1/24 dev tap0

# Launch with TAP
qemu-system-x86_64 \
  -machine q35 \
  -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev tap,id=net0,ifname=tap0,script=no,downscript=no \
  -device virtio-net-pci,netdev=net0 \
  -serial mon:stdio \
  -nographic
```

### QEMU Serial Console

```bash
# Monitor on stdio
-serial mon:stdio -nographic

# Serial on separate terminal
-serial pty
# QEMU will print: char device redirected to /dev/pts/X
# Connect with: screen /dev/pts/X

# Serial to file
-serial file:serial.log

# Serial to TCP port
-serial tcp:localhost:4444,server,nowait
# Connect with: telnet localhost 4444
```

## VMware Workstation / Fusion

### Create VM

1. **New Virtual Machine**
   - Typical configuration
   - Installer disc image: Skip
   - Guest OS: Linux → Other Linux 5.x or later 64-bit
   - Name: Phase Boot
   - Disk: 10GB, single file
   - RAM: 2GB minimum

2. **Customize Hardware**
   - Remove floppy
   - CD/DVD: Use ISO (if you created one) or point to ESP directory
   - Network: NAT or Bridged
   - USB Controller: USB 3.1
   - **Firmware**: UEFI (not BIOS)

### Install Phase Boot

#### Option 1: ISO Image

```bash
# Create bootable ISO
sudo apt-get install xorriso grub-efi-amd64-bin

# Build ISO
grub-mkrescue -o phase-boot.iso boot/esp

# Attach ISO to VM
# VM Settings → CD/DVD → Use ISO image → phase-boot.iso
```

#### Option 2: Virtual Disk

```bash
# Create VMDK
qemu-img create -f vmdk phase-boot.vmdk 10G

# Mount and install (Linux host)
sudo modprobe nbd
sudo qemu-nbd -c /dev/nbd0 phase-boot.vmdk
sudo parted /dev/nbd0 mklabel gpt
sudo parted /dev/nbd0 mkpart ESP fat32 1MiB 512MiB
sudo parted /dev/nbd0 set 1 esp on
sudo mkfs.vfat -F32 /dev/nbd0p1

sudo mount /dev/nbd0p1 /mnt/phase-boot
sudo cp -r boot/esp/* /mnt/phase-boot/
sudo grub-install --target=x86_64-efi \
  --efi-directory=/mnt/phase-boot \
  --boot-directory=/mnt/phase-boot \
  --removable /dev/nbd0
sudo umount /mnt/phase-boot
sudo qemu-nbd -d /dev/nbd0

# Attach VMDK to VM
# VM Settings → Hard Disk → Add → Existing disk → phase-boot.vmdk
```

### VMware Network for Phase Discovery

**NAT Mode** (default):
- Outbound internet access
- Local discovery limited to VMware NAT subnet
- Good for: Internet mode testing

**Bridged Mode**:
- VM on same network as host
- Full local discovery
- Good for: Local mode testing with physical devices

**Host-Only Mode**:
- Isolated network with host
- Good for: Private mode testing

### VMware Serial Console

```bash
# Add serial port to VM
# VM Settings → Add → Serial Port → Output to file
# File: /tmp/phase-boot-serial.log

# Monitor serial output
tail -f /tmp/phase-boot-serial.log
```

## VirtualBox

### Create VM

1. **New Virtual Machine**
   - Name: Phase Boot
   - Type: Linux
   - Version: Other Linux (64-bit)
   - RAM: 2048 MB
   - Hard disk: Create virtual hard disk
   - Type: VDI
   - Storage: Dynamically allocated
   - Size: 10 GB

2. **Settings → System**
   - **Enable EFI**: Check this box
   - Boot order: Hard Disk first
   - Processor: 2 CPUs

3. **Settings → Network**
   - Adapter 1: Enabled
   - Attached to: NAT or Bridged

### Install Phase Boot

#### Option 1: ISO

```bash
# Create ISO
grub-mkrescue -o phase-boot.iso boot/esp

# Attach to VM
# Settings → Storage → Controller: IDE → Add optical drive → phase-boot.iso
```

#### Option 2: VDI Disk

```bash
# Convert disk image
qemu-img convert -f qcow2 -O vdi phase-boot.qcow2 phase-boot.vdi

# Or create and install directly
VBoxManage createmedium disk \
  --filename phase-boot.vdi \
  --size 10240 \
  --format VDI

# Install Phase Boot (manual steps using VirtualBox's disk access)
```

#### Option 3: USB Boot

```bash
# Write Phase Boot to USB drive
sudo boot/scripts/write-usb.sh /dev/sdX

# Enable USB in VirtualBox
# Settings → USB → Enable USB Controller → USB 3.0

# Start VM, select Devices → USB → [Your USB device]
```

### VirtualBox EFI Configuration

```bash
# Access EFI shell
# Boot VM, press F12 for boot menu, select "EFI Internal Shell"

# From EFI shell, boot manually:
FS0:
cd EFI\BOOT
BOOTX64.EFI
```

### VirtualBox Serial Console

```bash
# Enable serial port
VBoxManage modifyvm "Phase Boot" \
  --uart1 0x3F8 4 \
  --uartmode1 file /tmp/phase-boot-serial.log

# Or via GUI:
# Settings → Serial Ports → Port 1
# Enable Serial Port: checked
# Port Mode: Raw File
# Path/Address: /tmp/phase-boot-serial.log

# Monitor output
tail -f /tmp/phase-boot-serial.log
```

### VirtualBox Network Modes

**NAT**:
```bash
# Default, outbound only
VBoxManage modifyvm "Phase Boot" --nic1 nat
```

**Bridged**:
```bash
# VM on host network
VBoxManage modifyvm "Phase Boot" --nic1 bridged --bridgeadapter1 eth0
```

**Host-Only**:
```bash
# Create host-only network
VBoxManage hostonlyif create
VBoxManage hostonlyif ipconfig vboxnet0 --ip 192.168.56.1

# Attach VM
VBoxManage modifyvm "Phase Boot" --nic1 hostonly --hostonlyadapter1 vboxnet0
```

## UTM (Apple Silicon macOS)

UTM is recommended for running Phase Boot on Apple Silicon (M1/M2/M3) Macs.

### Install UTM

```bash
# Download from https://mac.getutm.app/
# Or via Homebrew
brew install --cask utm
```

### Create ARM64 VM

1. **Create New Virtual Machine**
   - Virtualize (for ARM64)
   - Linux
   - Skip ISO boot
   - RAM: 2048 MB minimum
   - Storage: 10 GB
   - Name: Phase Boot ARM64

2. **VM Settings**
   - System → Architecture: ARM64 (aarch64)
   - System → Boot: UEFI
   - Drives → Delete existing drives
   - Drives → New Drive → Import → Select phase-boot.qcow2
   - Network → Network Mode: Shared Network (NAT)

### Build for ARM64

```bash
# On macOS (requires Homebrew)
brew install aarch64-elf-gcc qemu

# Cross-compile Phase Boot
make -C boot all ARCH=arm64 CROSS_COMPILE=aarch64-elf-

# Create disk image for UTM
qemu-img create -f qcow2 phase-boot.qcow2 10G

# Install Phase Boot (requires QEMU NBD or Linux VM)
# Use QEMU method described in "QEMU with Disk Image" section
```

### Create x86_64 VM (Emulated)

1. **Create New Virtual Machine**
   - Emulate (for x86_64 on ARM)
   - Linux
   - Skip ISO boot
   - Architecture: x86_64
   - RAM: 2048 MB
   - Storage: 10 GB

**Note**: Emulation is much slower than virtualization. Expect 10-50x slowdown.

### UTM Network Configuration

**Shared Network** (default):
- NAT with outbound access
- Good for internet mode

**Bridged Network**:
- VM on host network
- Requires manual configuration in macOS

**Host-Only Network**:
- Create in UTM settings
- Isolated network with host

### UTM Serial Console

```bash
# UTM has built-in serial console
# Window → Show Serial Console (Cmd+Shift+S)

# Or enable in VM Settings:
# Serial → Enable Serial Console
```

## Multi-VM Testing

### Test Peer Discovery

Run multiple VMs to test Phase peer discovery:

```bash
# Terminal 1: VM1
qemu-system-x86_64 \
  -machine q35 -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev tap,id=net0,ifname=tap0,script=no \
  -device virtio-net-pci,netdev=net0,mac=52:54:00:12:34:01 \
  -nographic

# Terminal 2: VM2
qemu-system-x86_64 \
  -machine q35 -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev tap,id=net1,ifname=tap1,script=no \
  -device virtio-net-pci,netdev=net1,mac=52:54:00:12:34:02 \
  -nographic

# Terminal 3: VM3
qemu-system-x86_64 \
  -machine q35 -m 2048 \
  -bios /usr/share/ovmf/OVMF.fd \
  -drive format=raw,file=fat:rw:boot/esp \
  -netdev tap,id=net2,ifname=tap2,script=no \
  -device virtio-net-pci,netdev=net2,mac=52:54:00:12:34:03 \
  -nographic
```

### Bridge TAP Interfaces

```bash
# Create bridge
sudo ip link add br0 type bridge
sudo ip link set br0 up

# Create and bridge TAP interfaces
for i in 0 1 2; do
  sudo ip tuntap add dev tap$i mode tap
  sudo ip link set tap$i up
  sudo ip link set tap$i master br0
done

# Assign IP to bridge
sudo ip addr add 192.168.100.1/24 dev br0

# Now all VMs can discover each other on br0
```

## Common VM Issues

### UEFI Firmware Not Found

**QEMU**:
```bash
# Install OVMF
sudo apt-get install ovmf

# Find firmware location
find /usr/share -name "OVMF*.fd"

# Use correct path in -bios parameter
```

**VirtualBox**:
```bash
# Ensure EFI is enabled
VBoxManage modifyvm "Phase Boot" --firmware efi
```

**VMware**:
- VM Settings → Options → Advanced → Firmware Type: UEFI

### VM Doesn't Boot

**Check boot order**:
- Ensure hard disk or CD/DVD is first in boot order
- For UEFI, ensure "EFI Internal Shell" is available

**VMware**: Settings → Options → Boot Options → Force BIOS Setup
**VirtualBox**: Settings → System → Boot Order

### No Network in VM

**QEMU**:
```bash
# Verify network device
-device virtio-net-pci,netdev=net0

# Try different network backend
-netdev user,id=net0  # User mode
-netdev tap,id=net0,ifname=tap0  # TAP mode
```

**VirtualBox**:
```bash
# Check adapter is enabled
VBoxManage showvminfo "Phase Boot" | grep NIC

# Reset network
VBoxManage modifyvm "Phase Boot" --nic1 nat
```

**VMware**:
- Settings → Network Adapter → Connected
- Try different network mode (NAT/Bridged)

### Slow Performance

**Enable hardware acceleration**:

**QEMU**: Use KVM
```bash
qemu-system-x86_64 -enable-kvm -cpu host
```

**VirtualBox**:
```bash
VBoxManage modifyvm "Phase Boot" --nested-hw-virt on
VBoxManage modifyvm "Phase Boot" --paravirtprovider kvm
```

**VMware**:
- Settings → Processors → Virtualize Intel VT-x/AMD-V

**UTM (Apple Silicon)**:
- Use "Virtualize" not "Emulate" for ARM64
- For x86_64: Emulation is inherently slow

### Serial Console No Output

**QEMU**:
```bash
# Ensure kernel has serial console
# grub.cfg: console=ttyS0,115200

# Verify serial parameter
-serial mon:stdio -nographic
```

**VirtualBox/VMware**:
- Ensure serial port enabled in settings
- Check file permissions on output file
- Try different serial port modes

## Performance Benchmarks

Approximate boot time to Phase discovery screen:

| Platform | Architecture | Mode | Boot Time |
|----------|-------------|------|-----------|
| QEMU + KVM | x86_64 | Virtualized | 3-5s |
| QEMU | x86_64 | Emulated | 10-15s |
| QEMU + KVM | ARM64 | Virtualized | 4-6s |
| QEMU | ARM64 | Emulated | 30-60s |
| VMware Workstation | x86_64 | Virtualized | 4-6s |
| VirtualBox | x86_64 | Virtualized | 5-8s |
| UTM (Apple Silicon) | ARM64 | Virtualized | 4-6s |
| UTM (Apple Silicon) | x86_64 | Emulated | 60-120s |

## Next Steps

- **Physical Hardware**: See `QUICKSTART-x86_64.md` or `QUICKSTART-ARM64.md`
- **Configuration**: See `boot/docs/CONFIGURATION.md` for advanced boot options
- **Development**: Use VMs for rapid iteration during development
- **Testing**: See `boot/docs/testing.md` for automated VM testing

## Reference

### QEMU Useful Options

| Option | Description |
|--------|-------------|
| `-machine q35` | Modern chipset (x86_64) |
| `-machine virt` | Generic virtualized board (ARM64) |
| `-cpu host` | Pass through host CPU (with KVM) |
| `-enable-kvm` | Use KVM acceleration |
| `-m 2048` | RAM in MB |
| `-smp 2` | Number of CPUs |
| `-bios <file>` | UEFI firmware |
| `-nographic` | No graphical output |
| `-serial mon:stdio` | Serial console on stdio |
| `-netdev user` | User mode networking (NAT) |
| `-netdev tap` | TAP networking (bridged) |

### Quick Commands

```bash
# Build and test x86_64
make -C boot all && make -C boot test-qemu

# Build and test ARM64
make -C boot all ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-
qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 2048 \
  -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  -drive format=raw,file=fat:rw:boot/esp -nographic

# Create bootable ISO
grub-mkrescue -o phase-boot.iso boot/esp

# Create QEMU disk image
qemu-img create -f qcow2 phase-boot.qcow2 10G

# Convert between formats
qemu-img convert -f qcow2 -O vmdk phase-boot.qcow2 phase-boot.vmdk
qemu-img convert -f qcow2 -O vdi phase-boot.qcow2 phase-boot.vdi
```
