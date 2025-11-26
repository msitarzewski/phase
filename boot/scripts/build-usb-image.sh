#!/bin/bash
#
# Phase Boot - USB Image Builder
# Creates complete bootable USB image with GPT partitioning
#

set -e

#------------------------------------------------------------------------------
# Color Output
#------------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

ok()    { echo -e "${GREEN}[OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
fail()  { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }
info()  { echo -e "${BLUE}[INFO]${NC} $1"; }

#------------------------------------------------------------------------------
# Configuration
#------------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BOOT_DIR="$(dirname "$SCRIPT_DIR")"

ARCH=""
OUTPUT=""
SIZE="4G"
ESP_SIZE="256M"
SEED_SIZE="1G"

# Build artifact paths (will be set based on ARCH)
ESP_SOURCE=""
ROOTFS_SQFS=""
KERNEL=""
INITRAMFS=""

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - USB Image Builder

Creates complete bootable USB image with:
  - GPT partition table
  - ESP partition (bootloader, kernel, initramfs)
  - Seed partition (rootfs SquashFS)
  - Cache partition (ext4)

Usage: $(basename "$0") [OPTIONS]

Options:
    -a, --arch ARCH       Target architecture (x86_64, arm64) [required]
    -o, --output PATH     Output image file [default: build/phase-boot-ARCH.img]
    -s, --size SIZE       Total image size (e.g., 4G, 8G) [default: 4G]
    --esp-size SIZE       ESP partition size [default: 256M]
    --seed-size SIZE      Seed rootfs partition size [default: 1G]
    -h, --help            Show this help message

Build Requirements:
    Must run after successful build of:
      - bootloader (systemd-boot for x86_64, U-Boot for arm64)
      - kernel
      - initramfs
      - rootfs SquashFS

Example:
    $(basename "$0") --arch x86_64 --output phase-boot.img --size 8G
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -a|--arch)      ARCH="$2"; shift 2 ;;
        -o|--output)    OUTPUT="$2"; shift 2 ;;
        -s|--size)      SIZE="$2"; shift 2 ;;
        --esp-size)     ESP_SIZE="$2"; shift 2 ;;
        --seed-size)    SEED_SIZE="$2"; shift 2 ;;
        -h|--help)      usage ;;
        *)              fail "Unknown option: $1" ;;
    esac
done

# Validate
[[ -z "$ARCH" ]] && fail "Architecture required. Use --arch x86_64 or --arch arm64"
[[ "$ARCH" != "x86_64" && "$ARCH" != "arm64" ]] && fail "Invalid architecture: $ARCH (must be x86_64 or arm64)"

# Set default output if not specified
[[ -z "$OUTPUT" ]] && OUTPUT="${BOOT_DIR}/build/phase-boot-${ARCH}.img"

# Set build artifact paths
ESP_SOURCE="${BOOT_DIR}/build/esp-${ARCH}"
ROOTFS_SQFS="${BOOT_DIR}/build/rootfs.sqfs"

# Check for required tools
command -v sgdisk &>/dev/null || fail "sgdisk not found. Install gdisk"
command -v mkfs.vfat &>/dev/null || fail "mkfs.vfat not found. Install dosfstools"
command -v mkfs.ext4 &>/dev/null || fail "mkfs.ext4 not found. Install e2fsprogs"
command -v truncate &>/dev/null || fail "truncate not found. Install coreutils"

#------------------------------------------------------------------------------
# Helper Functions
#------------------------------------------------------------------------------

# Convert size string to bytes
size_to_bytes() {
    local size=$1
    local num=${size%[GMKgmk]*}
    local unit=${size##*[0-9]}

    case $unit in
        G|g) echo $((num * 1024 * 1024 * 1024)) ;;
        M|m) echo $((num * 1024 * 1024)) ;;
        K|k) echo $((num * 1024)) ;;
        *)   echo "$num" ;;
    esac
}

# Convert size string to MB
size_to_mb() {
    local bytes
    bytes=$(size_to_bytes "$1")
    echo $((bytes / 1024 / 1024))
}

# Check if running as root
require_root() {
    if [[ $EUID -ne 0 ]]; then
        info "Requesting sudo for disk operations..."
        exec sudo -E "$0" "$@"
    fi
}

#------------------------------------------------------------------------------
# Validate Build Artifacts
#------------------------------------------------------------------------------

validate_artifacts() {
    info "Validating build artifacts..."

    # Check ESP directory
    if [[ ! -d "$ESP_SOURCE" ]]; then
        fail "ESP directory not found: $ESP_SOURCE"
    fi
    ok "ESP directory: $ESP_SOURCE"

    # Check for bootloader
    if [[ "$ARCH" == "x86_64" ]]; then
        if [[ ! -f "$ESP_SOURCE/EFI/BOOT/BOOTX64.EFI" ]]; then
            fail "Bootloader not found: $ESP_SOURCE/EFI/BOOT/BOOTX64.EFI"
        fi
    elif [[ "$ARCH" == "arm64" ]]; then
        if [[ ! -f "$ESP_SOURCE/EFI/BOOT/BOOTAA64.EFI" ]]; then
            fail "Bootloader not found: $ESP_SOURCE/EFI/BOOT/BOOTAA64.EFI"
        fi
    fi
    ok "Bootloader present"

    # Check for kernel
    local kernel_found=false
    for kpath in "$ESP_SOURCE/vmlinuz" "$ESP_SOURCE/vmlinuz-"* "$ESP_SOURCE/Image" "$ESP_SOURCE/Image-"*; do
        if [[ -f "$kpath" ]]; then
            kernel_found=true
            ok "Kernel: $(basename "$kpath")"
            break
        fi
    done
    [[ "$kernel_found" == "false" ]] && fail "Kernel not found in $ESP_SOURCE"

    # Check for initramfs
    local initramfs_found=false
    for ipath in "$ESP_SOURCE/initramfs.cpio.gz" "$ESP_SOURCE/initramfs-"*.cpio.gz "$ESP_SOURCE/initrd"*; do
        if [[ -f "$ipath" ]]; then
            initramfs_found=true
            ok "Initramfs: $(basename "$ipath")"
            break
        fi
    done
    [[ "$initramfs_found" == "false" ]] && fail "Initramfs not found in $ESP_SOURCE"

    # Check rootfs SquashFS
    if [[ ! -f "$ROOTFS_SQFS" ]]; then
        fail "Rootfs SquashFS not found: $ROOTFS_SQFS"
    fi
    ok "Rootfs: $ROOTFS_SQFS ($(du -h "$ROOTFS_SQFS" | cut -f1))"
}

#------------------------------------------------------------------------------
# Create Partitioned Image
#------------------------------------------------------------------------------

create_image() {
    info "Creating disk image..."
    info "Output: $OUTPUT"
    info "Size: $SIZE"

    # Ensure output directory exists
    mkdir -p "$(dirname "$OUTPUT")"

    # Create sparse image file
    rm -f "$OUTPUT"
    truncate -s "$SIZE" "$OUTPUT"
    ok "Created sparse image: $SIZE"

    # Calculate partition sizes in MB
    local esp_mb seed_mb
    esp_mb=$(size_to_mb "$ESP_SIZE")
    seed_mb=$(size_to_mb "$SEED_SIZE")

    info "Partition layout: ESP=${esp_mb}MB, Seed=${seed_mb}MB, Cache=remaining"

    # Create GPT partition table
    info "Creating GPT partition table..."
    sgdisk --clear "$OUTPUT" &>/dev/null

    # Partition 1: ESP (EFI System Partition)
    local esp_start=2048  # 1MB in 512-byte sectors
    local esp_end=$((esp_start + (esp_mb * 2048) - 1))
    sgdisk --new=1:${esp_start}:${esp_end} \
           --typecode=1:EF00 \
           --change-name=1:"EFI System" \
           "$OUTPUT" &>/dev/null
    ok "Partition 1: ESP (${esp_mb}MB, FAT32)"

    # Partition 2: Seed rootfs
    local seed_start=$((esp_end + 1))
    local seed_end=$((seed_start + (seed_mb * 2048) - 1))
    sgdisk --new=2:${seed_start}:${seed_end} \
           --typecode=2:8300 \
           --change-name=2:"Phase Seed" \
           "$OUTPUT" &>/dev/null
    ok "Partition 2: Seed (${seed_mb}MB, SquashFS)"

    # Partition 3: Cache (remaining space)
    local cache_start=$((seed_end + 1))
    sgdisk --new=3:${cache_start}:0 \
           --typecode=3:8300 \
           --change-name=3:"Phase Cache" \
           "$OUTPUT" &>/dev/null

    # Calculate cache size
    local total_mb cache_mb
    total_mb=$(size_to_mb "$SIZE")
    cache_mb=$((total_mb - esp_mb - seed_mb - 2))  # -2MB for alignment
    ok "Partition 3: Cache (${cache_mb}MB, ext4)"
}

#------------------------------------------------------------------------------
# Format and Populate Partitions
#------------------------------------------------------------------------------

populate_image() {
    info "Setting up loop device..."

    local loop_dev
    loop_dev=$(losetup --find --show --partscan "$OUTPUT")

    # Cleanup on exit
    cleanup() {
        sync
        losetup -d "$loop_dev" 2>/dev/null || true
    }
    trap cleanup EXIT

    # Wait for partition devices
    sleep 1

    # Verify partition devices
    if [[ ! -e "${loop_dev}p1" ]]; then
        fail "Partition devices not created. Try: sudo partprobe $loop_dev"
    fi

    # Format ESP as FAT32
    info "Formatting ESP partition (FAT32)..."
    mkfs.vfat -F 32 -n "PHASEEFI" "${loop_dev}p1" &>/dev/null
    ok "ESP formatted"

    # Copy ESP contents
    info "Copying ESP contents..."
    local mount_point
    mount_point=$(mktemp -d)
    mount "${loop_dev}p1" "$mount_point"

    cp -r "$ESP_SOURCE"/* "$mount_point"/
    local file_count
    file_count=$(find "$mount_point" -type f | wc -l)

    sync
    umount "$mount_point"
    rmdir "$mount_point"
    ok "Copied $file_count files to ESP"

    # Write SquashFS to seed partition
    info "Writing rootfs SquashFS to seed partition..."
    dd if="$ROOTFS_SQFS" of="${loop_dev}p2" bs=4M status=none
    ok "SquashFS written to seed partition"

    # Format Cache as ext4
    info "Formatting cache partition (ext4)..."
    mkfs.ext4 -L "PHASECACHE" -q "${loop_dev}p3" &>/dev/null
    ok "Cache partition formatted"

    # Cleanup
    losetup -d "$loop_dev"
    trap - EXIT
}

#------------------------------------------------------------------------------
# Generate Checksums
#------------------------------------------------------------------------------

generate_checksums() {
    info "Generating checksums..."

    local checksum_file="${OUTPUT}.sha256"
    local checksum
    checksum=$(sha256sum "$OUTPUT" | cut -d' ' -f1)

    # Write checksum file
    echo "$checksum  $(basename "$OUTPUT")" > "$checksum_file"
    ok "Checksum: ${checksum:0:16}..."

    # Generate detailed manifest
    local manifest="${OUTPUT}.manifest"
    cat > "$manifest" <<EOF
Phase Boot USB Image Manifest
==============================

Image:        $(basename "$OUTPUT")
Architecture: $ARCH
Build Date:   $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Image Size:   $(du -h "$OUTPUT" | cut -f1)
SHA256:       $checksum

Partitions:
  1. ESP (FAT32)    - Bootloader, kernel, initramfs
  2. Seed (raw)     - SquashFS rootfs
  3. Cache (ext4)   - Persistent storage

Build Artifacts:
  ESP Source:   $ESP_SOURCE
  Rootfs:       $ROOTFS_SQFS
EOF

    ok "Manifest: $(basename "$manifest")"
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - USB Image Builder"
echo "==============================="
echo ""
echo "Architecture: $ARCH"
echo "Output:       $OUTPUT"
echo ""

validate_artifacts

echo ""
require_root "$@"

echo ""
create_image

echo ""
populate_image

echo ""
generate_checksums

echo ""
ok "USB image build complete!"
echo ""
info "Image:    $OUTPUT"
info "Size:     $(du -h "$OUTPUT" | cut -f1)"
info "Checksum: ${OUTPUT}.sha256"
info "Manifest: ${OUTPUT}.manifest"
echo ""
info "Next steps:"
echo "  - Write to USB: sudo dd if=$OUTPUT of=/dev/sdX bs=4M status=progress"
echo "  - Test in QEMU: ./scripts/test-qemu-x86.sh --image $OUTPUT"
echo "  - Convert to QCOW2: ./scripts/build-qcow2.sh --input $OUTPUT"
