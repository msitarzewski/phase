#!/bin/bash
#
# Phase Boot - Partition Layout Creator
# Creates GPT-partitioned disk image for Phase Boot
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

OUTPUT=""
SIZE="4G"
ESP_SIZE="256M"
SEED_SIZE="512M"
# Cache gets remaining space

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - Partition Layout Creator

Usage: $(basename "$0") [OPTIONS]

Options:
    -o, --output PATH     Output image file [required]
    -s, --size SIZE       Total image size (e.g., 4G, 8G) [default: 4G]
    --esp-size SIZE       ESP partition size [default: 256M]
    --seed-size SIZE      Seed rootfs partition size [default: 512M]
    -h, --help            Show this help message

Partition Layout:
    1. ESP (EFI System Partition) - FAT32, bootable
    2. Seed rootfs - for SquashFS image
    3. Cache - ext4, remaining space

Example:
    $(basename "$0") --output phase-boot.img --size 4G
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -o|--output)    OUTPUT="$2"; shift 2 ;;
        -s|--size)      SIZE="$2"; shift 2 ;;
        --esp-size)     ESP_SIZE="$2"; shift 2 ;;
        --seed-size)    SEED_SIZE="$2"; shift 2 ;;
        -h|--help)      usage ;;
        *)              fail "Unknown option: $1" ;;
    esac
done

# Validate
[[ -z "$OUTPUT" ]] && fail "Output path required. Use --output PATH"

# Check for required tools
command -v sgdisk &>/dev/null || fail "sgdisk not found. Install gdisk"
command -v mkfs.vfat &>/dev/null || fail "mkfs.vfat not found. Install dosfstools"
command -v mkfs.ext4 &>/dev/null || fail "mkfs.ext4 not found. Install e2fsprogs"

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

#------------------------------------------------------------------------------
# Create Image
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

    info "Partition sizes: ESP=${esp_mb}MB, Seed=${seed_mb}MB, Cache=remaining"

    # Create GPT partition table
    info "Creating GPT partition table..."
    sgdisk --clear "$OUTPUT"

    # Partition 1: ESP (EFI System Partition)
    # Start at 1MB (after GPT header), size as specified
    local esp_start=2048  # 1MB in 512-byte sectors
    local esp_end=$((esp_start + (esp_mb * 2048) - 1))
    sgdisk --new=1:${esp_start}:${esp_end} \
           --typecode=1:EF00 \
           --change-name=1:"EFI System" \
           "$OUTPUT"
    ok "Created ESP partition (${esp_mb}MB)"

    # Partition 2: Seed rootfs
    local seed_start=$((esp_end + 1))
    local seed_end=$((seed_start + (seed_mb * 2048) - 1))
    sgdisk --new=2:${seed_start}:${seed_end} \
           --typecode=2:8300 \
           --change-name=2:"Phase Seed" \
           "$OUTPUT"
    ok "Created Seed partition (${seed_mb}MB)"

    # Partition 3: Cache (remaining space)
    local cache_start=$((seed_end + 1))
    sgdisk --new=3:${cache_start}:0 \
           --typecode=3:8300 \
           --change-name=3:"Phase Cache" \
           "$OUTPUT"
    ok "Created Cache partition (remaining space)"

    # Print partition table
    echo ""
    info "Partition table:"
    sgdisk --print "$OUTPUT"
}

#------------------------------------------------------------------------------
# Format Partitions (requires root and loop device)
#------------------------------------------------------------------------------

format_partitions() {
    if [[ $EUID -ne 0 ]]; then
        warn "Not running as root, skipping filesystem creation"
        warn "Run 'sudo $0 $*' to format partitions"
        return 0
    fi

    info "Setting up loop device..."

    local loop_dev
    loop_dev=$(losetup --find --show --partscan "$OUTPUT")

    # Cleanup on exit
    cleanup() {
        losetup -d "$loop_dev" 2>/dev/null || true
    }
    trap cleanup EXIT

    # Wait for partition devices
    sleep 1

    # Format ESP as FAT32
    if [[ -e "${loop_dev}p1" ]]; then
        info "Formatting ESP as FAT32..."
        mkfs.vfat -F 32 -n "PHASEEFI" "${loop_dev}p1"
        ok "Formatted ESP"
    fi

    # Seed partition - leave unformatted (will contain SquashFS)
    info "Seed partition left unformatted (for SquashFS dd)"

    # Format Cache as ext4
    if [[ -e "${loop_dev}p3" ]]; then
        info "Formatting Cache as ext4..."
        mkfs.ext4 -L "PHASECACHE" -q "${loop_dev}p3"
        ok "Formatted Cache"
    fi

    # Cleanup
    losetup -d "$loop_dev"
    trap - EXIT

    ok "All partitions formatted"
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - Partition Layout Creator"
echo "======================================="
echo ""

create_image

echo ""
format_partitions

echo ""
ok "Disk image created: $OUTPUT"
echo ""
info "Next steps:"
echo "  1. Mount ESP and copy bootloader files"
echo "  2. Write SquashFS to seed partition (dd)"
echo "  3. Boot in QEMU or write to USB"
