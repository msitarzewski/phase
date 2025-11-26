#!/bin/bash
#
# Phase Boot - Dependency Checker
# Checks for required build tools and provides install commands
#

set -e

#------------------------------------------------------------------------------
# Color Output
#------------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

ok()    { echo -e "${GREEN}[OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
fail()  { echo -e "${RED}[MISSING]${NC} $1"; }
info()  { echo -e "${BLUE}[INFO]${NC} $1"; }

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - Dependency Checker

Usage: $(basename "$0") [OPTIONS]

Options:
    -h, --help      Show this help message
    -i, --install   Show install commands for missing deps
    -q, --quiet     Only show missing dependencies

Checks for all required tools to build Phase Boot images.
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

SHOW_INSTALL=false
QUIET=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -h|--help)    usage ;;
        -i|--install) SHOW_INSTALL=true; shift ;;
        -q|--quiet)   QUIET=true; shift ;;
        *)            echo "Unknown option: $1"; usage ;;
    esac
done

#------------------------------------------------------------------------------
# Dependency Definitions
#------------------------------------------------------------------------------

# Format: "command|package|description"
DEPS=(
    "cpio|cpio|Archive creation for initramfs"
    "gzip|gzip|Compression for initramfs"
    "mksquashfs|squashfs-tools|SquashFS creation for rootfs"
    "sgdisk|gdisk|GPT partition table manipulation"
    "mkfs.vfat|dosfstools|FAT32 filesystem creation"
    "mkfs.ext4|e2fsprogs|ext4 filesystem creation"
    "dd|coreutils|Raw disk writing"
    "losetup|mount|Loop device management"
    "qemu-system-x86_64|qemu-system-x86|x86_64 emulation"
    "qemu-system-aarch64|qemu-system-arm|ARM64 emulation"
    "grub-mkimage|grub-efi-amd64-bin|GRUB EFI image creation"
    "busybox|busybox-static|Minimal userspace tools"
    "xz|xz-utils|XZ compression"
    "sha256sum|coreutils|Checksum verification"
    "curl|curl|Downloading kernel/tools"
    "tar|tar|Archive extraction"
)

# Optional dependencies
OPTIONAL_DEPS=(
    "kvm|qemu-kvm|Hardware virtualization (faster QEMU)"
    "pv|pv|Progress bar for dd operations"
    "mtools|mtools|FAT filesystem manipulation"
    "xorriso|xorriso|ISO image creation"
)

# UEFI firmware files
FIRMWARE_PATHS=(
    "/usr/share/ovmf/OVMF.fd|ovmf|UEFI firmware for x86_64 QEMU"
    "/usr/share/qemu-efi-aarch64/QEMU_EFI.fd|qemu-efi-aarch64|UEFI firmware for ARM64 QEMU"
    "/usr/lib/systemd/boot/efi/systemd-bootx64.efi|systemd-boot|systemd-boot x86_64"
)

#------------------------------------------------------------------------------
# Check Functions
#------------------------------------------------------------------------------

MISSING_REQUIRED=()
MISSING_OPTIONAL=()
MISSING_FIRMWARE=()

check_command() {
    local cmd=$1
    local pkg=$2
    local desc=$3
    local required=${4:-true}

    if command -v "$cmd" &>/dev/null; then
        [[ "$QUIET" == "false" ]] && ok "$cmd - $desc"
        return 0
    else
        if [[ "$required" == "true" ]]; then
            fail "$cmd - $desc"
            MISSING_REQUIRED+=("$pkg")
        else
            [[ "$QUIET" == "false" ]] && warn "$cmd - $desc (optional)"
            MISSING_OPTIONAL+=("$pkg")
        fi
        return 1
    fi
}

check_file() {
    local path=$1
    local pkg=$2
    local desc=$3

    if [[ -f "$path" ]]; then
        [[ "$QUIET" == "false" ]] && ok "$path"
        return 0
    else
        warn "$path - $desc"
        MISSING_FIRMWARE+=("$pkg")
        return 1
    fi
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - Dependency Check"
echo "=============================="
echo ""

# Check required dependencies
if [[ "$QUIET" == "false" ]]; then
    info "Checking required dependencies..."
    echo ""
fi

for dep in "${DEPS[@]}"; do
    IFS='|' read -r cmd pkg desc <<< "$dep"
    check_command "$cmd" "$pkg" "$desc" true
done

echo ""

# Check optional dependencies
if [[ "$QUIET" == "false" ]]; then
    info "Checking optional dependencies..."
    echo ""
fi

for dep in "${OPTIONAL_DEPS[@]}"; do
    IFS='|' read -r cmd pkg desc <<< "$dep"
    check_command "$cmd" "$pkg" "$desc" false
done

echo ""

# Check UEFI firmware
if [[ "$QUIET" == "false" ]]; then
    info "Checking UEFI firmware..."
    echo ""
fi

for fw in "${FIRMWARE_PATHS[@]}"; do
    IFS='|' read -r path pkg desc <<< "$fw"
    check_file "$path" "$pkg" "$desc"
done

#------------------------------------------------------------------------------
# Summary
#------------------------------------------------------------------------------

echo ""
echo "=============================="

if [[ ${#MISSING_REQUIRED[@]} -eq 0 ]]; then
    ok "All required dependencies installed!"
else
    fail "${#MISSING_REQUIRED[@]} required dependencies missing"
fi

if [[ ${#MISSING_OPTIONAL[@]} -gt 0 ]]; then
    warn "${#MISSING_OPTIONAL[@]} optional dependencies missing"
fi

#------------------------------------------------------------------------------
# Install Commands
#------------------------------------------------------------------------------

if [[ "$SHOW_INSTALL" == "true" || ${#MISSING_REQUIRED[@]} -gt 0 ]]; then
    echo ""
    info "Install commands (Ubuntu/Debian):"
    echo ""

    if [[ ${#MISSING_REQUIRED[@]} -gt 0 ]]; then
        # Deduplicate packages
        UNIQUE_PKGS=($(echo "${MISSING_REQUIRED[@]}" | tr ' ' '\n' | sort -u | tr '\n' ' '))
        echo "  sudo apt-get install -y ${UNIQUE_PKGS[*]}"
    fi

    if [[ ${#MISSING_OPTIONAL[@]} -gt 0 ]]; then
        UNIQUE_OPT=($(echo "${MISSING_OPTIONAL[@]}" | tr ' ' '\n' | sort -u | tr '\n' ' '))
        echo ""
        echo "  # Optional:"
        echo "  sudo apt-get install -y ${UNIQUE_OPT[*]}"
    fi

    if [[ ${#MISSING_FIRMWARE[@]} -gt 0 ]]; then
        UNIQUE_FW=($(echo "${MISSING_FIRMWARE[@]}" | tr ' ' '\n' | sort -u | tr '\n' ' '))
        echo ""
        echo "  # UEFI firmware:"
        echo "  sudo apt-get install -y ${UNIQUE_FW[*]}"
    fi
fi

#------------------------------------------------------------------------------
# Exit Code
#------------------------------------------------------------------------------

if [[ ${#MISSING_REQUIRED[@]} -gt 0 ]]; then
    exit 1
fi

exit 0
