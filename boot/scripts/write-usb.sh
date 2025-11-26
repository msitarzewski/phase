#!/bin/bash
#
# Phase Boot - USB Writer
# Writes Phase Boot image to physical USB device
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

IMAGE=""
DEVICE=""
FORCE=false

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - USB Writer

Usage: $(basename "$0") [OPTIONS]

Options:
    -i, --image PATH      Source image file [required]
    -d, --device PATH     Target USB device (e.g., /dev/sdX) [required]
    -f, --force           Skip confirmation prompt
    -h, --help            Show this help message

WARNING: This will DESTROY ALL DATA on the target device!

Example:
    $(basename "$0") --image phase-boot.img --device /dev/sdb
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -i|--image)   IMAGE="$2"; shift 2 ;;
        -d|--device)  DEVICE="$2"; shift 2 ;;
        -f|--force)   FORCE=true; shift ;;
        -h|--help)    usage ;;
        *)            fail "Unknown option: $1" ;;
    esac
done

# Validate required arguments
[[ -z "$IMAGE" ]] && fail "Image path required. Use --image PATH"
[[ -z "$DEVICE" ]] && fail "Device path required. Use --device PATH"

#------------------------------------------------------------------------------
# Safety Checks
#------------------------------------------------------------------------------

safety_checks() {
    info "Performing safety checks..."

    # Must be root
    if [[ $EUID -ne 0 ]]; then
        fail "This script must be run as root"
    fi

    # Check image exists
    if [[ ! -f "$IMAGE" ]]; then
        fail "Image not found: $IMAGE"
    fi
    ok "Image found: $IMAGE"

    # Check device exists
    if [[ ! -b "$DEVICE" ]]; then
        fail "Device not found or not a block device: $DEVICE"
    fi
    ok "Device exists: $DEVICE"

    # Prevent writing to mounted devices
    if mount | grep -q "^$DEVICE"; then
        fail "Device $DEVICE is mounted! Unmount it first."
    fi

    # Check for partitions
    for part in "${DEVICE}"*[0-9]; do
        if mount | grep -q "^$part"; then
            fail "Partition $part is mounted! Unmount all partitions first."
        fi
    done
    ok "Device is not mounted"

    # Prevent writing to system disks
    local root_dev
    root_dev=$(df / | tail -1 | awk '{print $1}' | sed 's/[0-9]*$//')
    if [[ "$DEVICE" == "$root_dev" ]]; then
        fail "DANGER: $DEVICE appears to be your system disk!"
    fi
    ok "Not a system disk"

    # Check device size
    local image_size device_size
    image_size=$(stat -c%s "$IMAGE")
    device_size=$(blockdev --getsize64 "$DEVICE")

    if [[ $device_size -lt $image_size ]]; then
        fail "Device too small: $(numfmt --to=iec $device_size) < $(numfmt --to=iec $image_size)"
    fi
    ok "Device size OK: $(numfmt --to=iec $device_size)"

    # Show device info
    echo ""
    info "Device information:"
    lsblk -o NAME,SIZE,TYPE,MOUNTPOINT "$DEVICE" 2>/dev/null || true
    echo ""
}

#------------------------------------------------------------------------------
# Confirmation
#------------------------------------------------------------------------------

confirm_write() {
    if [[ "$FORCE" == "true" ]]; then
        return 0
    fi

    echo ""
    warn "========================================"
    warn "  WARNING: ALL DATA WILL BE DESTROYED  "
    warn "========================================"
    echo ""
    echo "  Image:  $IMAGE"
    echo "  Device: $DEVICE"
    echo ""

    read -p "Type 'yes' to confirm: " confirm

    if [[ "$confirm" != "yes" ]]; then
        fail "Aborted by user"
    fi
}

#------------------------------------------------------------------------------
# Write Image
#------------------------------------------------------------------------------

write_image() {
    local image_size image_size_human
    image_size=$(stat -c%s "$IMAGE")
    image_size_human=$(numfmt --to=iec-i --suffix=B "$image_size" 2>/dev/null || echo "$((image_size / 1048576)) MiB")

    info "Writing image to $DEVICE..."
    info "Image size: $image_size_human"
    info "Note: Sync will flush buffers after write (can be slow on USB drives)"
    echo ""

    # Use pv for progress (required dependency), fallback to dd status
    if command -v pv &>/dev/null; then
        pv -s "$image_size" -tpreb "$IMAGE" | dd of="$DEVICE" bs=4M conv=fsync 2>/dev/null
    else
        dd if="$IMAGE" of="$DEVICE" bs=4M status=progress conv=fsync
    fi

    echo ""

    # Sync with progress indicator
    info "Syncing buffers to disk (this can take a while on slow drives)..."

    # Run sync in background and show spinner
    sync &
    local sync_pid=$!
    local spin=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
    local i=0
    while kill -0 $sync_pid 2>/dev/null; do
        printf "\r  %s Flushing write cache... " "${spin[$i]}"
        i=$(( (i + 1) % ${#spin[@]} ))
        sleep 0.1
    done
    printf "\r  ✓ Sync complete!            \n"

    ok "Image written successfully!"
}

#------------------------------------------------------------------------------
# Verify
#------------------------------------------------------------------------------

verify_write() {
    info "Verifying write..."

    local image_size image_size_human
    image_size=$(stat -c%s "$IMAGE")
    image_size_human=$(numfmt --to=iec-i --suffix=B "$image_size" 2>/dev/null || echo "$((image_size / 1048576)) MiB")

    # Drop filesystem caches to ensure we read from disk, not memory
    info "Dropping caches to ensure fresh read from device..."
    echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true

    # Re-read partition table
    blockdev --rereadpt "$DEVICE" 2>/dev/null || true

    # Calculate checksum of image
    info "Hashing source image ($image_size_human)..."
    local image_hash
    image_hash=$(pv -s "$image_size" -tpreb "$IMAGE" | sha256sum | cut -d' ' -f1)

    # Calculate checksum of written data
    info "Hashing device ($image_size_human)..."
    local device_hash
    device_hash=$(dd if="$DEVICE" bs=4M count=$((image_size / 4194304 + 1)) status=none | head -c "$image_size" | pv -s "$image_size" -tpreb | sha256sum | cut -d' ' -f1)

    echo ""
    info "Image hash:  $image_hash"
    info "Device hash: $device_hash"

    if [[ "$image_hash" == "$device_hash" ]]; then
        ok "Verification passed!"
    else
        warn "Checksums do not match!"
        warn "This may indicate a write error or bad USB drive."
        warn "Try writing again, or use a different USB drive."
        fail "Verification FAILED!"
    fi
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - USB Writer"
echo "========================"
echo ""

safety_checks
confirm_write
write_image
verify_write

echo ""
ok "Done! You can now boot from $DEVICE"
echo ""
info "To boot:"
echo "  1. Insert USB into target machine"
echo "  2. Enter BIOS/UEFI setup"
echo "  3. Disable Secure Boot (temporarily)"
echo "  4. Select USB as boot device"
