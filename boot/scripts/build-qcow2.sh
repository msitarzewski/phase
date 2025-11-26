#!/bin/bash
#
# Phase Boot - QCOW2 Converter
# Converts raw disk images to QCOW2 format for QEMU/KVM
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

INPUT=""
OUTPUT=""
COMPRESSION=true
COMPAT="1.1"  # QCOW2 version (1.1 is widely compatible)

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - QCOW2 Converter

Converts raw disk images to QCOW2 format for use with QEMU/KVM.
QCOW2 provides:
  - Compression (smaller file size)
  - Snapshots support
  - Better performance in virtualization

Usage: $(basename "$0") [OPTIONS]

Options:
    -i, --input PATH      Input raw disk image [required]
    -o, --output PATH     Output QCOW2 file [default: INPUT.qcow2]
    --no-compression      Disable compression (faster, larger)
    --compat VERSION      QCOW2 version (0.10, 1.1) [default: 1.1]
    -h, --help            Show this help message

QCOW2 Versions:
    0.10 - Maximum compatibility (QEMU 0.10+)
    1.1  - Modern features (QEMU 1.1+, recommended)

Example:
    $(basename "$0") --input phase-boot-x86_64.img
    $(basename "$0") --input phase-boot.img --output phase-boot-vm.qcow2
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -i|--input)         INPUT="$2"; shift 2 ;;
        -o|--output)        OUTPUT="$2"; shift 2 ;;
        --no-compression)   COMPRESSION=false; shift ;;
        --compat)           COMPAT="$2"; shift 2 ;;
        -h|--help)          usage ;;
        *)                  fail "Unknown option: $1" ;;
    esac
done

# Validate
[[ -z "$INPUT" ]] && fail "Input image required. Use --input PATH"
[[ ! -f "$INPUT" ]] && fail "Input image not found: $INPUT"

# Set default output if not specified
if [[ -z "$OUTPUT" ]]; then
    # Remove .img extension if present, add .qcow2
    local base="${INPUT%.img}"
    OUTPUT="${base}.qcow2"
fi

# Validate compat version
[[ "$COMPAT" != "0.10" && "$COMPAT" != "1.1" ]] && fail "Invalid compat version: $COMPAT (must be 0.10 or 1.1)"

# Check for qemu-img
command -v qemu-img &>/dev/null || fail "qemu-img not found. Install qemu-utils"

#------------------------------------------------------------------------------
# Analyze Input Image
#------------------------------------------------------------------------------

analyze_input() {
    info "Analyzing input image..."

    local input_size
    input_size=$(du -h "$INPUT" | cut -f1)
    ok "Input: $INPUT ($input_size)"

    # Check if input is already QCOW2
    local input_format
    input_format=$(qemu-img info "$INPUT" | grep "file format:" | awk '{print $3}')

    if [[ "$input_format" == "qcow2" ]]; then
        warn "Input is already QCOW2 format"
        echo ""
        read -p "Continue conversion anyway? [y/N] " -n 1 -r
        echo ""
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            info "Conversion cancelled"
            exit 0
        fi
    else
        ok "Format: $input_format"
    fi

    # Show detailed info
    info "Image details:"
    qemu-img info "$INPUT" | grep -E "virtual size|disk size|format" | sed 's/^/  /'
}

#------------------------------------------------------------------------------
# Convert to QCOW2
#------------------------------------------------------------------------------

convert_image() {
    info "Converting to QCOW2..."
    info "Output: $OUTPUT"
    info "Compression: $COMPRESSION"
    info "Compat: $COMPAT"

    # Ensure output directory exists
    mkdir -p "$(dirname "$OUTPUT")"

    # Remove existing output
    if [[ -f "$OUTPUT" ]]; then
        warn "Removing existing output: $OUTPUT"
        rm -f "$OUTPUT"
    fi

    # Build qemu-img command
    local cmd="qemu-img convert"
    cmd+=" -f raw"                # Input format
    cmd+=" -O qcow2"              # Output format
    cmd+=" -o compat=$COMPAT"     # QCOW2 version

    # Add compression if enabled
    if [[ "$COMPRESSION" == "true" ]]; then
        cmd+=" -c"                # Compress
        cmd+=" -o preallocation=off"
    fi

    # Add progress display
    cmd+=" -p"

    # Input and output files
    cmd+=" \"$INPUT\""
    cmd+=" \"$OUTPUT\""

    # Execute conversion
    echo ""
    eval $cmd
    echo ""

    ok "Conversion complete"
}

#------------------------------------------------------------------------------
# Verify Output
#------------------------------------------------------------------------------

verify_output() {
    info "Verifying output..."

    if [[ ! -f "$OUTPUT" ]]; then
        fail "Output file not created: $OUTPUT"
    fi

    # Get file sizes
    local input_size output_size
    input_size=$(stat -f%z "$INPUT" 2>/dev/null || stat -c%s "$INPUT")
    output_size=$(stat -f%z "$OUTPUT" 2>/dev/null || stat -c%s "$OUTPUT")

    # Calculate compression ratio
    local ratio
    ratio=$(awk "BEGIN {printf \"%.1f\", ($input_size / $output_size)}")

    local input_h output_h
    input_h=$(du -h "$INPUT" | cut -f1)
    output_h=$(du -h "$OUTPUT" | cut -f1)

    ok "Output verified"
    echo ""
    info "Size comparison:"
    echo "  Input (raw):   $input_h"
    echo "  Output (qcow2): $output_h"
    echo "  Compression:   ${ratio}x"

    # Show QCOW2 info
    echo ""
    info "QCOW2 details:"
    qemu-img info "$OUTPUT" | sed 's/^/  /'
}

#------------------------------------------------------------------------------
# Generate Checksum
#------------------------------------------------------------------------------

generate_checksum() {
    info "Generating checksum..."

    local checksum_file="${OUTPUT}.sha256"
    local checksum
    checksum=$(sha256sum "$OUTPUT" | cut -d' ' -f1)

    echo "$checksum  $(basename "$OUTPUT")" > "$checksum_file"
    ok "Checksum: ${checksum:0:16}..."
    ok "Saved to: $(basename "$checksum_file")"
}

#------------------------------------------------------------------------------
# Main
#------------------------------------------------------------------------------

echo "Phase Boot - QCOW2 Converter"
echo "============================="
echo ""

analyze_input

echo ""
convert_image

echo ""
verify_output

echo ""
generate_checksum

echo ""
ok "QCOW2 conversion complete!"
echo ""
info "Output:   $OUTPUT"
info "Checksum: ${OUTPUT}.sha256"
echo ""
info "Test in QEMU:"
echo "  qemu-system-x86_64 -enable-kvm -m 2G -bios /usr/share/ovmf/OVMF.fd -drive file=$OUTPUT,format=qcow2,if=virtio"
