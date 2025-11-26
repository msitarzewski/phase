#!/bin/bash
#
# Phase Boot - Initramfs Builder
# Creates compressed CPIO initramfs archives
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
INITRAMFS_SRC="${BOOT_DIR}/initramfs"

ARCH="x86_64"
OUTPUT=""
INSTALL_BUSYBOX=true

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - Initramfs Builder

Usage: $(basename "$0") [OPTIONS]

Options:
    -a, --arch ARCH       Target architecture (x86_64, arm64) [default: x86_64]
    -o, --output PATH     Output file path [required]
    -s, --source DIR      Source initramfs directory [default: ../initramfs]
    --no-busybox          Skip busybox symlink installation
    -h, --help            Show this help message

Example:
    $(basename "$0") --arch x86_64 --output build/initramfs-x86_64.img
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -a|--arch)     ARCH="$2"; shift 2 ;;
        -o|--output)   OUTPUT="$2"; shift 2 ;;
        -s|--source)   INITRAMFS_SRC="$2"; shift 2 ;;
        --no-busybox)  INSTALL_BUSYBOX=false; shift ;;
        -h|--help)     usage ;;
        *)             fail "Unknown option: $1" ;;
    esac
done

# Validate required arguments
[[ -z "$OUTPUT" ]] && fail "Output path required. Use --output PATH"
[[ ! -d "$INITRAMFS_SRC" ]] && fail "Initramfs source not found: $INITRAMFS_SRC"

#------------------------------------------------------------------------------
# Install BusyBox Symlinks
#------------------------------------------------------------------------------

install_busybox_symlinks() {
    local busybox_path="$1"
    local bin_dir="$2"

    info "Installing BusyBox symlinks..."

    # Common busybox applets needed for boot
    local applets=(
        # Shell
        sh ash
        # Core utils
        ls cat echo cp mv rm mkdir rmdir ln pwd basename dirname
        # File operations
        head tail wc grep sed awk cut sort uniq tr
        # System
        mount umount losetup sync
        # Process
        ps kill sleep
        # Network
        ip ping route ifconfig hostname
        # Compression
        gzip gunzip zcat
        # Archive
        tar cpio
        # Misc
        true false test [ env clear reset
        # DHCP
        udhcpc
    )

    local count=0
    for applet in "${applets[@]}"; do
        if [[ ! -e "${bin_dir}/${applet}" ]]; then
            ln -sf busybox "${bin_dir}/${applet}" 2>/dev/null && ((count++)) || true
        fi
    done

    ok "Created $count busybox symlinks"
}

#------------------------------------------------------------------------------
# Build Initramfs
#------------------------------------------------------------------------------

build_initramfs() {
    info "Building initramfs for ${ARCH}..."

    # Create temporary working directory
    local work_dir
    work_dir=$(mktemp -d)
    trap "rm -rf '$work_dir'" EXIT

    info "Work directory: $work_dir"

    # Copy source to work directory
    cp -a "$INITRAMFS_SRC"/* "$work_dir/" 2>/dev/null || true

    # Ensure init is executable
    if [[ -f "$work_dir/init" ]]; then
        chmod +x "$work_dir/init"
        ok "Init script found and made executable"
    else
        warn "No init script found in $INITRAMFS_SRC"
    fi

    # Create essential directories
    mkdir -p "$work_dir"/{bin,sbin,etc,dev,proc,sys,run,tmp,usr/{bin,sbin,lib},lib,lib64}

    # Install busybox symlinks if busybox exists
    if [[ "$INSTALL_BUSYBOX" == "true" ]]; then
        # Check for busybox in work dir or try to copy from host
        if [[ -f "$work_dir/bin/busybox" ]]; then
            install_busybox_symlinks "$work_dir/bin/busybox" "$work_dir/bin"
        elif command -v busybox &>/dev/null; then
            local host_busybox
            host_busybox=$(command -v busybox)
            if file "$host_busybox" | grep -q "statically linked"; then
                cp "$host_busybox" "$work_dir/bin/busybox"
                chmod +x "$work_dir/bin/busybox"
                install_busybox_symlinks "$work_dir/bin/busybox" "$work_dir/bin"
            else
                warn "Host busybox is not static, skipping copy"
            fi
        else
            warn "No busybox found, initramfs may not boot properly"
        fi
    fi

    # Create device nodes (these will be overwritten by devtmpfs but needed for init)
    if [[ $EUID -eq 0 ]]; then
        mknod -m 622 "$work_dir/dev/console" c 5 1 2>/dev/null || true
        mknod -m 666 "$work_dir/dev/null"    c 1 3 2>/dev/null || true
        mknod -m 666 "$work_dir/dev/zero"    c 1 5 2>/dev/null || true
        mknod -m 666 "$work_dir/dev/tty"     c 5 0 2>/dev/null || true
    else
        warn "Not root, skipping device node creation"
    fi

    # Create CPIO archive
    info "Creating CPIO archive..."
    local cpio_file="${work_dir}/initramfs.cpio"

    (cd "$work_dir" && find . -print0 | cpio --null -o -H newc --quiet > "$cpio_file")

    # Compress with gzip
    info "Compressing with gzip..."
    gzip -9 -c "$cpio_file" > "$OUTPUT"

    # Report results
    local size
    size=$(du -h "$OUTPUT" | cut -f1)
    local checksum
    checksum=$(sha256sum "$OUTPUT" | cut -d' ' -f1)

    echo ""
    ok "Initramfs created successfully!"
    echo ""
    info "Output:   $OUTPUT"
    info "Size:     $size"
    info "Arch:     $ARCH"
    info "SHA256:   ${checksum:0:16}..."
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - Initramfs Builder"
echo "==============================="
echo ""

# Ensure output directory exists
mkdir -p "$(dirname "$OUTPUT")"

# Build
build_initramfs

echo ""
ok "Done!"
