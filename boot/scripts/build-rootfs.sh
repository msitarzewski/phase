#!/bin/bash
#
# Phase Boot - Seed Rootfs Builder
# Creates SquashFS image from rootfs directory
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
ROOTFS_SRC="${BOOT_DIR}/rootfs"

OUTPUT=""
COMPRESSION="xz"
BLOCK_SIZE="1M"

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - Seed Rootfs Builder

Usage: $(basename "$0") [OPTIONS]

Options:
    -o, --output PATH     Output file path [required]
    -s, --source DIR      Source rootfs directory [default: ../rootfs]
    -c, --compression ALG Compression algorithm (xz, gzip, lzo) [default: xz]
    -b, --block-size SIZE Block size for compression [default: 1M]
    -h, --help            Show this help message

Example:
    $(basename "$0") --output build/rootfs.sqfs
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -o|--output)       OUTPUT="$2"; shift 2 ;;
        -s|--source)       ROOTFS_SRC="$2"; shift 2 ;;
        -c|--compression)  COMPRESSION="$2"; shift 2 ;;
        -b|--block-size)   BLOCK_SIZE="$2"; shift 2 ;;
        -h|--help)         usage ;;
        *)                 fail "Unknown option: $1" ;;
    esac
done

# Validate
[[ -z "$OUTPUT" ]] && fail "Output path required. Use --output PATH"
[[ ! -d "$ROOTFS_SRC" ]] && fail "Rootfs source not found: $ROOTFS_SRC"

# Check for mksquashfs
command -v mksquashfs &>/dev/null || fail "mksquashfs not found. Install squashfs-tools"

#------------------------------------------------------------------------------
# Build SquashFS
#------------------------------------------------------------------------------

build_rootfs() {
    info "Building seed rootfs SquashFS..."
    info "Source: $ROOTFS_SRC"
    info "Compression: $COMPRESSION"

    # Ensure output directory exists
    mkdir -p "$(dirname "$OUTPUT")"

    # Remove existing output
    rm -f "$OUTPUT"

    # Create SquashFS
    info "Creating SquashFS image..."
    mksquashfs "$ROOTFS_SRC" "$OUTPUT" \
        -comp "$COMPRESSION" \
        -b "$BLOCK_SIZE" \
        -no-xattrs \
        -noappend \
        -quiet

    # Report results
    local size
    size=$(du -h "$OUTPUT" | cut -f1)
    local checksum
    checksum=$(sha256sum "$OUTPUT" | cut -d' ' -f1)

    echo ""
    ok "SquashFS created successfully!"
    echo ""
    info "Output:      $OUTPUT"
    info "Size:        $size"
    info "Compression: $COMPRESSION"
    info "SHA256:      ${checksum:0:16}..."

    # Verify mount (if root)
    if [[ $EUID -eq 0 ]]; then
        local mount_point
        mount_point=$(mktemp -d)
        if mount -t squashfs -o loop,ro "$OUTPUT" "$mount_point" 2>/dev/null; then
            local file_count
            file_count=$(find "$mount_point" -type f | wc -l)
            umount "$mount_point"
            rmdir "$mount_point"
            ok "Verified: $file_count files in image"
        else
            warn "Could not verify mount (might need root)"
            rmdir "$mount_point" 2>/dev/null || true
        fi
    fi
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - Seed Rootfs Builder"
echo "================================="
echo ""

build_rootfs

echo ""
ok "Done!"
