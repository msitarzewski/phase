# Phase Boot Quickstart - ARM64

This guide covers building and deploying Phase Boot on ARM64 hardware, including Raspberry Pi 4, ARM development boards, and ARM VMs.

## Prerequisites

### Build Host Requirements

**Native ARM64 Host**:
```bash
# Ubuntu/Debian ARM64
sudo apt-get update
sudo apt-get install -y build-essential git wget curl \
  dosfstools mtools grub-efi-arm64-bin qemu-system-aarch64
```

**x86_64 Cross-Compilation Host**:
```bash
# Ubuntu/Debian x86_64
sudo apt-get update
sudo apt-get install -y build-essential git wget curl \
  crossbuild-essential-arm64 dosfstools mtools \
  grub-efi-arm64-bin qemu-system-aarch64 qemu-efi-aarch64
```

### Target Hardware Requirements

#### Raspberry Pi 4/5
- Raspberry Pi 4 Model B (2GB+ RAM recommended)
- Raspberry Pi 5 (all models)
- MicroSD card (4GB+, Class 10+)
- USB-C power supply (5V 3A minimum)
- Network connectivity (Ethernet or WiFi)

#### Other ARM64 Hardware
- ARM64 CPU (ARMv8-A or newer)
- UEFI firmware (e.g., U-Boot with UEFI support)
- 512MB RAM minimum, 1GB+ recommended
- SD card or USB storage
- Serial console recommended for debugging

## Building Phase Boot

### Native ARM64 Build

```bash
# Navigate to Phase repository
cd /home/user/phase

# Build for ARM64
make -C boot all ARCH=arm64

# Expected output:
# - boot/esp/EFI/BOOT/BOOTAA64.EFI (ARM64 UEFI bootloader)
# - boot/esp/vmlinuz-phase (ARM64 kernel)
# - boot/initramfs.cpio.gz (initramfs)
```

### Cross-Compilation from x86_64

```bash
# Set cross-compilation environment
export CROSS_COMPILE=aarch64-linux-gnu-
export ARCH=arm64

# Build
make -C boot all ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-

# Verify ARM64 binaries
file boot/esp/vmlinuz-phase
# Expected: Linux kernel ARM64 boot executable Image

file boot/esp/EFI/BOOT/BOOTAA64.EFI
# Expected: PE32+ executable (EFI application) ARM aarch64
```

### Build Artifacts

```bash
# Check build output
ls -lh boot/esp/EFI/BOOT/BOOTAA64.EFI
ls -lh boot/esp/vmlinuz-phase
ls -lh boot/initramfs.cpio.gz

# ARM64-specific files
ls -lh boot/esp/dtb/  # Device tree blobs (if applicable)
```

## Raspberry Pi Deployment

### 1. Prepare SD Card

```bash
# Identify SD card device
lsblk

# CAUTION: Verify device carefully!
# Replace /dev/sdX with your SD card device
export SDCARD=/dev/sdX

# Partition the SD card
sudo parted $SDCARD mklabel gpt
sudo parted $SDCARD mkpart ESP fat32 1MiB 512MiB
sudo parted $SDCARD set 1 esp on

# Format
sudo mkfs.vfat -F32 ${SDCARD}1
```

### 2. Install Phase Boot

```bash
# Mount SD card
sudo mkdir -p /mnt/phase-boot
sudo mount ${SDCARD}1 /mnt/phase-boot

# Copy ESP contents
sudo cp -r boot/esp/* /mnt/phase-boot/

# Raspberry Pi specific: firmware files
# (If needed - modern RPi 4/5 have UEFI firmware in SPI)
# Download from https://github.com/pftf/RPi4
# sudo cp -r rpi4-uefi/* /mnt/phase-boot/

# Unmount
sudo umount /mnt/phase-boot
```

### 3. Raspberry Pi UEFI Firmware (RPi 4)

Raspberry Pi 4 needs UEFI firmware installed:

```bash
# Download RPi4 UEFI firmware
cd /tmp
wget https://github.com/pftf/RPi4/releases/latest/download/RPi4_UEFI_Firmware_v1.38.zip
unzip RPi4_UEFI_Firmware_v1.38.zip

# Mount SD card
sudo mount ${SDCARD}1 /mnt/phase-boot

# Install UEFI firmware
sudo cp -r RPI_EFI.fd /mnt/phase-boot/
sudo cp -r bcm2711-rpi-*.dtb /mnt/phase-boot/
sudo cp -r config.txt /mnt/phase-boot/
sudo cp -r overlays/ /mnt/phase-boot/

# Create config.txt if not exists
cat << 'EOF' | sudo tee /mnt/phase-boot/config.txt
arm_64bit=1
enable_uart=1
enable_gic=1
armstub=RPI_EFI.fd
disable_commandline_tags=1
device_tree_address=0x1f0000
device_tree_end=0x200000
EOF

# Unmount
sudo umount /mnt/phase-boot
```

### 4. Boot Raspberry Pi

1. Insert SD card into Raspberry Pi
2. Connect Ethernet cable
3. (Optional) Connect serial console for debugging
4. Power on
5. Press ESC during boot to enter UEFI menu
6. Select "Boot Manager" → "UEFI SD/MMC"

### Raspberry Pi Serial Console

```bash
# Connect USB-to-TTL adapter:
# - TX (adapter) → GPIO14 (RPi pin 8)
# - RX (adapter) → GPIO15 (RPi pin 10)
# - GND (adapter) → GND (RPi pin 6)

# Open serial console on build host
sudo screen /dev/ttyUSB0 115200

# Or with minicom
sudo minicom -D /dev/ttyUSB0 -b 115200
```

## Generic ARM64 Development Boards

### Khadas VIM3/4, ODROID-N2+, etc.

```bash
# Write to SD/eMMC
sudo boot/scripts/write-usb.sh /dev/sdX

# Or manual method:
sudo parted /dev/sdX mklabel gpt
sudo parted /dev/sdX mkpart ESP fat32 1MiB 512MiB
sudo parted /dev/sdX set 1 esp on
sudo mkfs.vfat -F32 /dev/sdX1

sudo mount /dev/sdX1 /mnt/phase-boot
sudo cp -r boot/esp/* /mnt/phase-boot/
sudo grub-install --target=arm64-efi \
  --efi-directory=/mnt/phase-boot \
  --boot-directory=/mnt/phase-boot \
  --removable /dev/sdX
sudo umount /mnt/phase-boot
```

### U-Boot Integration

If your board uses U-Boot:

```bash
# U-Boot environment
setenv boot_efi 'load mmc 0:1 ${kernel_addr_r} /EFI/BOOT/BOOTAA64.EFI; bootefi ${kernel_addr_r}'
setenv bootcmd 'run boot_efi'
saveenv

# Or boot manually from U-Boot prompt
load mmc 0:1 ${kernel_addr_r} /EFI/BOOT/BOOTAA64.EFI
bootefi ${kernel_addr_r}
```

## Testing in QEMU

### ARM64 QEMU on x86_64 Host

```bash
# Install ARM64 QEMU
sudo apt-get install qemu-system-aarch64 qemu-efi-aarch64

# Download ARM64 UEFI firmware (if not installed)
sudo mkdir -p /usr/share/qemu-efi-aarch64
sudo wget -O /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  https://releases.linaro.org/components/kernel/uefi-linaro/latest/release/qemu64/QEMU_EFI.fd

# Test in QEMU
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
```

### ARM64 QEMU on ARM64 Host

```bash
# Test script (if available)
boot/scripts/test-qemu-arm64.sh

# Or manually
qemu-system-aarch64 \
  -machine virt \
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

### Expected QEMU Output

```
QEMU EFI firmware initializing...
BdsDxe: loading Boot0001 "UEFI QEMU DVD-ROM" from PciRoot(0x0)/Pci(0x1,0x1)/Ata(Secondary,Master,0x0)
EFI stub: Booting Linux Kernel...
Phase Boot v0.1
Mode: internet (default)
Network: eth0 UP 10.0.2.15
Discovering Phase peers...
```

## Boot Modes

Phase Boot ARM64 supports the same modes as x86_64:

### Internet Mode (Default)

```bash
phase.mode=internet
```

- Public network peer discovery
- Full Phase network participation

### Local Mode

```bash
phase.mode=local
```

- Local network discovery only
- Development and testing
- No internet required

### Private Mode

```bash
phase.mode=private
```

- Manual peer configuration
- Air-gapped deployments
- Maximum security

### Setting Boot Mode on ARM64

**In GRUB**:
1. Press `e` to edit boot entry
2. Modify kernel line:
   ```
   linux /vmlinuz-phase phase.mode=local console=ttyAMA0,115200
   ```
3. Press `Ctrl-X` to boot

**Persistent in grub.cfg**:
```bash
menuentry "Phase Boot - Local Mode" {
    linux /vmlinuz-phase phase.mode=local console=ttyAMA0,115200
    initrd /initramfs.cpio.gz
}
```

## Cross-Compilation Notes

### Kernel Configuration

```bash
# ARM64 kernel defconfig
make -C boot/kernel ARCH=arm64 defconfig

# Customize for specific hardware
make -C boot/kernel ARCH=arm64 menuconfig
# Enable: Device Drivers → ARM Platforms → [Your board]
# Enable: Device Drivers → Network device support → Ethernet drivers
```

### Device Tree Blobs (DTB)

```bash
# Build device trees
make -C boot/kernel ARCH=arm64 dtbs

# Copy to ESP
cp boot/kernel/arch/arm64/boot/dts/broadcom/bcm2711-rpi-4-b.dtb \
   boot/esp/dtb/

# Reference in GRUB
# grub.cfg:
# devicetree /dtb/bcm2711-rpi-4-b.dtb
```

### Initramfs for ARM64

```bash
# Build ARM64 binaries for initramfs
make -C boot/initramfs ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-

# Statically linked binaries required
# busybox, phase-discover, phase-verify tools must be ARM64
```

### Toolchain Setup

```bash
# Install cross-compilation toolchain
sudo apt-get install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu

# Verify toolchain
aarch64-linux-gnu-gcc --version
# Should show: aarch64-linux-gnu-gcc (Ubuntu ...) ...

# Test compile
echo 'int main() { return 0; }' | aarch64-linux-gnu-gcc -x c -o /tmp/test -
file /tmp/test
# Should show: ELF 64-bit LSB executable, ARM aarch64
```

## Common ARM64 Issues

### Raspberry Pi Doesn't Boot

**Problem**: Black screen, no output

**Solution**:
1. Check UEFI firmware installed correctly
2. Verify config.txt exists with correct settings
3. Connect serial console to see boot messages
4. Try HDMI output (some UEFI firmwares prefer HDMI)

**Problem**: "Start4.elf not found"

**Solution**:
```bash
# RPi firmware files needed on SD card
sudo mount ${SDCARD}1 /mnt/phase-boot
sudo cp /boot/firmware/start4.elf /mnt/phase-boot/
sudo cp /boot/firmware/fixup4.dat /mnt/phase-boot/
sudo umount /mnt/phase-boot
```

### Cross-Compilation Fails

**Problem**: "Invalid instruction" or "illegal instruction"

**Solution**:
```bash
# Verify ARCH and CROSS_COMPILE set correctly
export ARCH=arm64
export CROSS_COMPILE=aarch64-linux-gnu-

# Clean and rebuild
make -C boot clean
make -C boot all
```

**Problem**: Kernel panic on boot

**Solution**:
1. Check kernel built for correct ARM version (ARMv8-A)
2. Verify device tree matches hardware
3. Enable early console output:
   ```bash
   # Add to kernel command line
   earlycon console=ttyAMA0,115200
   ```

### Network Issues

**Problem**: No network interface found

**Solution**:
1. Verify kernel has drivers for your NIC:
   ```bash
   # Check kernel config
   grep CONFIG_BROADCOM_PHY boot/kernel/.config
   grep CONFIG_BCMGENET boot/kernel/.config
   ```
2. Load kernel modules in initramfs
3. Check device tree has network device enabled

**Problem**: WiFi not working

**Solution**:
```bash
# WiFi typically requires firmware blobs
# Copy RPi WiFi firmware to initramfs
sudo mkdir -p boot/initramfs/lib/firmware/brcm
sudo cp /lib/firmware/brcm/brcmfmac43455-sdio.* \
   boot/initramfs/lib/firmware/brcm/

# Rebuild initramfs
make -C boot initramfs
```

### QEMU ARM64 Issues

**Problem**: QEMU crashes or fails to boot

**Solution**:
```bash
# Use compatible machine type
qemu-system-aarch64 -machine virt,gic-version=3

# Or try different CPU
qemu-system-aarch64 -cpu cortex-a72

# Enable KVM on ARM64 host
qemu-system-aarch64 -enable-kvm -cpu host
```

**Problem**: UEFI firmware not found

**Solution**:
```bash
# Install QEMU EFI firmware
sudo apt-get install qemu-efi-aarch64

# Or download manually
wget https://releases.linaro.org/components/kernel/uefi-linaro/latest/release/qemu64/QEMU_EFI.fd
sudo mkdir -p /usr/share/qemu-efi-aarch64
sudo mv QEMU_EFI.fd /usr/share/qemu-efi-aarch64/
```

## Hardware-Specific Notes

### Raspberry Pi 4
- Requires UEFI firmware from https://github.com/pftf/RPi4
- 3GB RAM limit in 32-bit UEFI mode (use 64-bit)
- USB boot possible with firmware update
- Serial console on GPIO14/15

### Raspberry Pi 5
- Native UEFI support in bootloader
- No additional firmware needed
- Faster boot times
- Better PCIe support

### NVIDIA Jetson Nano/Xavier
- Use L4T (Linux for Tegra) bootloader
- Flash ESP to eMMC or SD
- May require device tree modifications

### ODROID-N2+
- Petitboot bootloader
- UEFI support via third-party firmware
- eMMC recommended over SD for reliability

## Performance Tips

### ARM64-Specific Optimizations

```bash
# Build kernel with ARM64 optimizations
make -C boot/kernel ARCH=arm64 \
  KCFLAGS="-mcpu=cortex-a72 -O2"

# For Raspberry Pi 4
make -C boot/kernel ARCH=arm64 \
  KCFLAGS="-mcpu=cortex-a72"

# For newer ARMv8.2+
make -C boot/kernel ARCH=arm64 \
  KCFLAGS="-march=armv8.2-a -O2"
```

### Fast Boot

```bash
# Reduce kernel output
phase.loglevel=warn quiet

# Skip initramfs delays
phase.timeout=30

# Use faster discovery
phase.mode=local
```

## Next Steps

- **x86_64 Guide**: See `QUICKSTART-x86_64.md` for x86_64 deployment
- **VM Guide**: See `QUICKSTART-VM.md` for virtual machines
- **Configuration**: See `boot/docs/CONFIGURATION.md` for advanced options
- **Hardware Testing**: See `boot/docs/tested-hardware.md` for verified boards
- **Troubleshooting**: See `boot/docs/TROUBLESHOOTING.md` for detailed diagnostics

## Reference

### Key Files for ARM64

- `boot/Makefile` - Set `ARCH=arm64`
- `boot/scripts/write-usb.sh` - Works with SD cards
- `boot/esp/EFI/BOOT/BOOTAA64.EFI` - ARM64 UEFI bootloader
- `boot/esp/grub/grub.cfg` - GRUB configuration
- `boot/esp/dtb/` - Device tree blobs

### Boot Parameters for ARM64

| Parameter | Description |
|-----------|-------------|
| `console=ttyAMA0,115200` | ARM64 serial console (PL011) |
| `console=ttyS0,115200` | Generic serial console |
| `earlycon` | Early console output |
| `phase.mode=local` | Local discovery mode |

### Useful Commands

```bash
# Build for ARM64 from x86_64
make -C boot clean
make -C boot all ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-

# Rebuild only initramfs
make -C boot initramfs ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-

# Update SD card
sudo mount /dev/sdX1 /mnt/phase-boot
sudo cp boot/initramfs.cpio.gz /mnt/phase-boot/
sudo umount /mnt/phase-boot

# Test in QEMU
qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 2048 \
  -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
  -drive format=raw,file=fat:rw:boot/esp -nographic
```
