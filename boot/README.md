# Phase Boot

Bootable USB/VM image for Phase network discovery and WASM execution.

**Status**: Milestone 1 (Boot Stub & Media Layout) - In Development

## Overview

Phase Boot creates a minimal bootable environment that:

1. **Boots** from USB or VM (UEFI)
2. **Discovers** Phase network peers via mDNS and libp2p DHT
3. **Verifies** and fetches signed kernel/initramfs/rootfs
4. **Executes** kexec into the verified target system
5. **Runs** WASM jobs via the Plasm daemon

### Boot Modes

| Mode | Description |
|------|-------------|
| **Internet** | Full network, DHT discovery, downloads from web |
| **Local** | LAN-only, mDNS discovery, uses cache |
| **Private** | No writes, optional Tor, ephemeral identity |

## Quick Start

### Prerequisites

```bash
# Check required dependencies
./scripts/deps-check.sh

# Install missing (Ubuntu/Debian)
sudo apt-get install -y \
    qemu-system-x86 squashfs-tools dosfstools gdisk \
    cpio busybox-static grub-efi-amd64-bin ovmf \
    mtools xorriso curl
```

### Build

```bash
# Build everything for x86_64
make

# Or build specific components
make esp          # ESP partition contents
make initramfs    # Initramfs image
make rootfs       # SquashFS seed rootfs
make image        # Full USB image

# Build for ARM64
make ARCH=arm64
```

### Test in QEMU

```bash
# Test x86_64 build
make test-qemu-x86

# Or manually
./scripts/test-qemu-x86.sh --image build/phase-boot-x86_64.img
```

### Write to USB

```bash
# Write to USB device (DESTRUCTIVE!)
sudo ./scripts/write-usb.sh --image build/phase-boot-x86_64.img --device /dev/sdX
```

## Directory Structure

```
boot/
├── Makefile              # Build orchestration
├── README.md             # This file
├── esp/                  # EFI System Partition contents
│   ├── EFI/BOOT/         # Bootloader binaries
│   │   ├── BOOTX64.EFI   # systemd-boot x86_64
│   │   ├── BOOTAA64.EFI  # systemd-boot ARM64
│   │   └── grub.cfg      # GRUB fallback
│   ├── loader/           # systemd-boot config
│   │   ├── loader.conf   # Main loader config
│   │   └── entries/      # Boot menu entries
│   └── dtbs/             # Device Tree Blobs (ARM64)
├── kernel/               # Kernel configs
│   ├── config-x86_64     # x86_64 kernel config
│   └── config-arm64      # ARM64 kernel config
├── initramfs/            # Initramfs contents
│   ├── init              # PID 1 init script
│   ├── bin/              # Essential binaries (busybox)
│   ├── sbin/             # System binaries
│   └── etc/              # Configuration
├── rootfs/               # Seed rootfs (SquashFS source)
│   ├── bin/, sbin/       # Binaries
│   ├── etc/              # Configuration
│   └── usr/              # Shared libraries
├── scripts/              # Build & utility scripts
│   ├── deps-check.sh     # Dependency checker
│   ├── build-initramfs.sh
│   ├── build-rootfs.sh
│   ├── partition-layout.sh
│   ├── test-qemu-x86.sh
│   └── write-usb.sh
├── configs/              # Mode configurations
└── docs/                 # Documentation
```

## Boot Process

```
UEFI Firmware
    ↓
systemd-boot (or GRUB)
    ↓ selects boot entry
Kernel + Initramfs
    ↓ kernel cmdline: phase.mode=internet
/init script (PID 1)
    ↓ mounts /proc, /sys, /dev
    ↓ parses phase.mode
    ↓ brings up network
    ↓ [M2+] discovers peers
    ↓ [M3+] verifies & fetches
    ↓ [M4+] kexec into target
Shell / Plasm daemon
```

## Milestone Roadmap

### M1: Boot Stub & Media Layout (Current)
- [x] Project structure and Makefile
- [x] ESP partition with bootloader configs
- [x] Init script with mode parsing
- [x] Build scripts
- [ ] Kernel acquisition
- [ ] Full image build & test

### M2: Network Bring-up & Discovery
- [ ] Network initialization in init
- [ ] mDNS discovery (avahi)
- [ ] libp2p DHT integration
- [ ] Manifest schema

### M3: Verification & Fetch Pipeline
- [ ] Ed25519 signature verification
- [ ] Content-addressable storage
- [ ] Artifact fetcher (HTTP/IPFS)
- [ ] TUF-like metadata

### M4: kexec Handoff & Modes
- [ ] kexec orchestration
- [ ] OverlayFS setup
- [ ] Mode-specific behavior
- [ ] Kernel cmdline generation

### M5: Packaging & VM Images
- [ ] USB image builder
- [ ] QCOW2 for QEMU/KVM
- [ ] Parallels Desktop image
- [ ] Reproducible builds

### M6: Phase/Plasma Hello Job
- [ ] Plasm daemon integration
- [ ] Post-boot WASM execution
- [ ] systemd service
- [ ] Hello world demo

### M7: Docs & Secure Boot
- [ ] Quickstart guides
- [ ] Architecture documentation
- [ ] Threat model
- [ ] Secure Boot investigation

## Configuration

### Kernel Command Line Parameters

| Parameter | Values | Description |
|-----------|--------|-------------|
| `phase.mode` | internet, local, private | Boot mode |
| `phase.channel` | stable, testing | Update channel |
| `phase.cache` | enabled, disabled | Use local cache |
| `phase.nowrite` | true, false | Prevent disk writes |

### Environment Variables

```bash
# Build configuration
ARCH=x86_64          # Target architecture (x86_64, arm64)
IMAGE_SIZE=4G        # Output image size
```

## Testing

### QEMU Testing

```bash
# x86_64 with OVMF
qemu-system-x86_64 \
    -machine q35 \
    -enable-kvm \
    -m 2G \
    -bios /usr/share/ovmf/OVMF.fd \
    -drive file=build/phase-boot-x86_64.img,format=raw \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0

# ARM64 with EFI
qemu-system-aarch64 \
    -machine virt \
    -cpu cortex-a57 \
    -m 2G \
    -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
    -drive file=build/phase-boot-arm64.img,format=raw \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0
```

### Hardware Testing

Hardware testing requires:
1. USB drive (4GB+)
2. Target machine with UEFI
3. Disable Secure Boot (for now)

See `docs/tested-hardware.md` for compatibility list.

## Development

### Adding New Boot Entry

1. Create `esp/loader/entries/mymode.conf`
2. Add kernel options: `phase.mode=mymode`
3. Update init script to handle new mode
4. Rebuild: `make esp`

### Modifying Initramfs

1. Add files to `initramfs/`
2. Update `init` script if needed
3. Rebuild: `make initramfs`

### Building Custom Kernel

1. Edit `kernel/config-x86_64` or `kernel/config-arm64`
2. Run: `make kernel-x86_64` or `make kernel-arm64`
3. Rebuild image: `make image`

## License

Apache 2.0 - See [LICENSE](../LICENSE)

## Related

- [Phase Plasm Daemon](../daemon/) - WASM execution engine
- [Phase PHP SDK](../php-sdk/) - PHP client library
- [Memory Bank](../memory-bank/releases/boot/) - Planning docs
