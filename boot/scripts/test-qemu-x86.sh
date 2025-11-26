#!/bin/bash
#
# Phase Boot - QEMU x86_64 Test Script
# Boots Phase Boot image in QEMU with UEFI
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

IMAGE="${BOOT_DIR}/build/phase-boot-x86_64.img"
MEMORY="2G"
CPUS="2"
ENABLE_KVM=true
SERIAL_OUTPUT=false
GRAPHICS=true

# OVMF firmware paths (Ubuntu/Debian locations)
OVMF_PATHS=(
    "/usr/share/ovmf/OVMF.fd"
    "/usr/share/OVMF/OVMF_CODE.fd"
    "/usr/share/edk2/ovmf/OVMF_CODE.fd"
    "/usr/share/qemu/OVMF.fd"
)

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - QEMU x86_64 Test Script

Usage: $(basename "$0") [OPTIONS]

Options:
    -i, --image PATH      Disk image to boot [default: build/phase-boot-x86_64.img]
    -m, --memory SIZE     RAM size (e.g., 2G, 4G) [default: 2G]
    -c, --cpus NUM        Number of CPUs [default: 2]
    --no-kvm              Disable KVM acceleration
    --serial              Enable serial console output
    --no-graphics         Disable graphical output (serial only)
    -h, --help            Show this help message

Example:
    $(basename "$0") --image phase-boot.img --memory 4G
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -i|--image)       IMAGE="$2"; shift 2 ;;
        -m|--memory)      MEMORY="$2"; shift 2 ;;
        -c|--cpus)        CPUS="$2"; shift 2 ;;
        --no-kvm)         ENABLE_KVM=false; shift ;;
        --serial)         SERIAL_OUTPUT=true; shift ;;
        --no-graphics)    GRAPHICS=false; shift ;;
        -h|--help)        usage ;;
        *)                fail "Unknown option: $1" ;;
    esac
done

#------------------------------------------------------------------------------
# Find OVMF Firmware
#------------------------------------------------------------------------------

find_ovmf() {
    for path in "${OVMF_PATHS[@]}"; do
        if [[ -f "$path" ]]; then
            echo "$path"
            return 0
        fi
    done
    return 1
}

#------------------------------------------------------------------------------
# Check Requirements
#------------------------------------------------------------------------------

check_requirements() {
    info "Checking requirements..."

    # Check QEMU
    if ! command -v qemu-system-x86_64 &>/dev/null; then
        fail "qemu-system-x86_64 not found. Install qemu-system-x86"
    fi
    ok "qemu-system-x86_64 found"

    # Check OVMF
    local ovmf
    if ovmf=$(find_ovmf); then
        ok "OVMF firmware found: $ovmf"
        OVMF_FW="$ovmf"
    else
        fail "OVMF firmware not found. Install ovmf package"
    fi

    # Check image
    if [[ ! -f "$IMAGE" ]]; then
        fail "Disk image not found: $IMAGE"
    fi
    ok "Disk image found: $IMAGE"

    # Check KVM
    if [[ "$ENABLE_KVM" == "true" ]]; then
        if [[ -r /dev/kvm ]]; then
            ok "KVM available"
        else
            warn "KVM not available, falling back to software emulation"
            ENABLE_KVM=false
        fi
    fi
}

#------------------------------------------------------------------------------
# Build QEMU Command
#------------------------------------------------------------------------------

build_qemu_cmd() {
    local cmd="qemu-system-x86_64"

    # Machine type
    cmd+=" -machine q35"

    # CPU and memory
    cmd+=" -cpu qemu64"
    cmd+=" -smp $CPUS"
    cmd+=" -m $MEMORY"

    # KVM
    if [[ "$ENABLE_KVM" == "true" ]]; then
        cmd+=" -enable-kvm"
        cmd+=" -cpu host"
    fi

    # UEFI firmware
    cmd+=" -bios $OVMF_FW"

    # Disk
    cmd+=" -drive file=$IMAGE,format=raw,if=virtio"

    # Network (user mode with DHCP)
    cmd+=" -netdev user,id=net0"
    cmd+=" -device virtio-net-pci,netdev=net0"

    # Display
    if [[ "$GRAPHICS" == "true" ]]; then
        cmd+=" -display gtk"
    else
        cmd+=" -nographic"
    fi

    # Serial
    if [[ "$SERIAL_OUTPUT" == "true" ]] || [[ "$GRAPHICS" == "false" ]]; then
        cmd+=" -serial mon:stdio"
    fi

    # USB (for keyboard/mouse)
    cmd+=" -usb -device usb-kbd -device usb-mouse"

    echo "$cmd"
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - QEMU x86_64 Test"
echo "=============================="
echo ""

check_requirements

echo ""
info "Configuration:"
echo "  Image:   $IMAGE"
echo "  Memory:  $MEMORY"
echo "  CPUs:    $CPUS"
echo "  KVM:     $ENABLE_KVM"
echo "  OVMF:    $OVMF_FW"
echo ""

# Build and show command
QEMU_CMD=$(build_qemu_cmd)
info "QEMU command:"
echo "  $QEMU_CMD"
echo ""

# Run QEMU
info "Starting QEMU..."
echo ""
echo "============================================"
echo "  Press Ctrl+A, X to exit QEMU (serial mode)"
echo "  Close window to exit (graphical mode)"
echo "============================================"
echo ""

exec $QEMU_CMD
