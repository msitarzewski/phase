#!/bin/bash
#
# Phase Boot - Release Builder
# Orchestrates complete release build for all architectures
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

VERSION=""
RELEASE_DIR="${BOOT_DIR}/build/release"
ARCHES=("x86_64" "arm64")
BUILD_USB=true
BUILD_QCOW2=true
SKIP_EXISTING=false
IMAGE_SIZE="4G"

# Build counts
TOTAL_BUILDS=0
SUCCESSFUL_BUILDS=0
FAILED_BUILDS=0

#------------------------------------------------------------------------------
# Usage
#------------------------------------------------------------------------------

usage() {
    cat <<EOF
Phase Boot - Release Builder

Creates complete release build for all architectures:
  - USB images (raw disk images)
  - QCOW2 images (for virtualization)
  - Checksums (SHA256)
  - Release notes template

Usage: $(basename "$0") [OPTIONS]

Options:
    -v, --version TAG     Version tag (e.g., m5, v0.1.0) [required]
    -a, --arch ARCH       Build only specific architecture (x86_64, arm64)
    -d, --dir PATH        Release output directory [default: build/release]
    -s, --size SIZE       Image size [default: 4G]
    --no-usb              Skip USB image build
    --no-qcow2            Skip QCOW2 image build
    --skip-existing       Skip builds if output files exist
    -h, --help            Show this help message

Release Structure:
    build/release/
    ├── phase-boot-VERSION-x86_64.img
    ├── phase-boot-VERSION-x86_64.img.sha256
    ├── phase-boot-VERSION-x86_64.qcow2
    ├── phase-boot-VERSION-x86_64.qcow2.sha256
    ├── phase-boot-VERSION-arm64.img
    ├── phase-boot-VERSION-arm64.img.sha256
    ├── phase-boot-VERSION-arm64.qcow2
    ├── phase-boot-VERSION-arm64.qcow2.sha256
    ├── SHA256SUMS
    └── RELEASE_NOTES.md

Example:
    $(basename "$0") --version m5
    $(basename "$0") --version v0.1.0 --arch x86_64
EOF
    exit 0
}

#------------------------------------------------------------------------------
# Parse Arguments
#------------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--version)       VERSION="$2"; shift 2 ;;
        -a|--arch)          ARCHES=("$2"); shift 2 ;;
        -d|--dir)           RELEASE_DIR="$2"; shift 2 ;;
        -s|--size)          IMAGE_SIZE="$2"; shift 2 ;;
        --no-usb)           BUILD_USB=false; shift ;;
        --no-qcow2)         BUILD_QCOW2=false; shift ;;
        --skip-existing)    SKIP_EXISTING=true; shift ;;
        -h|--help)          usage ;;
        *)                  fail "Unknown option: $1" ;;
    esac
done

# Validate
[[ -z "$VERSION" ]] && fail "Version tag required. Use --version TAG"
[[ "$BUILD_USB" == "false" && "$BUILD_QCOW2" == "false" ]] && fail "At least one build type required (USB or QCOW2)"

# Validate architectures
for arch in "${ARCHES[@]}"; do
    [[ "$arch" != "x86_64" && "$arch" != "arm64" ]] && fail "Invalid architecture: $arch"
done

# Check for required scripts
[[ ! -x "$SCRIPT_DIR/build-usb-image.sh" ]] && fail "build-usb-image.sh not found or not executable"
if [[ "$BUILD_QCOW2" == "true" ]]; then
    [[ ! -x "$SCRIPT_DIR/build-qcow2.sh" ]] && fail "build-qcow2.sh not found or not executable"
fi

#------------------------------------------------------------------------------
# Helper Functions
#------------------------------------------------------------------------------

# Print section header
section() {
    echo ""
    echo "=============================================================================="
    echo "  $1"
    echo "=============================================================================="
    echo ""
}

# Check if file exists (for skip logic)
check_skip() {
    local file=$1
    if [[ "$SKIP_EXISTING" == "true" && -f "$file" ]]; then
        warn "Skipping: $(basename "$file") already exists"
        return 0
    fi
    return 1
}

# Build USB image for architecture
build_usb() {
    local arch=$1
    local output="${RELEASE_DIR}/phase-boot-${VERSION}-${arch}.img"

    info "Building USB image for $arch..."
    TOTAL_BUILDS=$((TOTAL_BUILDS + 1))

    # Check if should skip
    if check_skip "$output"; then
        SUCCESSFUL_BUILDS=$((SUCCESSFUL_BUILDS + 1))
        return 0
    fi

    # Run build
    if "$SCRIPT_DIR/build-usb-image.sh" \
        --arch "$arch" \
        --output "$output" \
        --size "$IMAGE_SIZE"; then
        ok "USB image built: $(basename "$output")"
        SUCCESSFUL_BUILDS=$((SUCCESSFUL_BUILDS + 1))
        return 0
    else
        fail "USB image build failed for $arch"
        FAILED_BUILDS=$((FAILED_BUILDS + 1))
        return 1
    fi
}

# Build QCOW2 image from USB image
build_qcow2() {
    local arch=$1
    local input="${RELEASE_DIR}/phase-boot-${VERSION}-${arch}.img"
    local output="${RELEASE_DIR}/phase-boot-${VERSION}-${arch}.qcow2"

    info "Building QCOW2 image for $arch..."
    TOTAL_BUILDS=$((TOTAL_BUILDS + 1))

    # Check if input exists
    if [[ ! -f "$input" ]]; then
        warn "Skipping QCOW2: USB image not found: $(basename "$input")"
        FAILED_BUILDS=$((FAILED_BUILDS + 1))
        return 1
    fi

    # Check if should skip
    if check_skip "$output"; then
        SUCCESSFUL_BUILDS=$((SUCCESSFUL_BUILDS + 1))
        return 0
    fi

    # Run build
    if "$SCRIPT_DIR/build-qcow2.sh" \
        --input "$input" \
        --output "$output"; then
        ok "QCOW2 image built: $(basename "$output")"
        SUCCESSFUL_BUILDS=$((SUCCESSFUL_BUILDS + 1))
        return 0
    else
        fail "QCOW2 build failed for $arch"
        FAILED_BUILDS=$((FAILED_BUILDS + 1))
        return 1
    fi
}

#------------------------------------------------------------------------------
# Generate Combined Checksums
#------------------------------------------------------------------------------

generate_checksums() {
    section "Generating Combined Checksums"

    local checksum_file="${RELEASE_DIR}/SHA256SUMS"

    info "Generating SHA256SUMS..."

    # Remove old checksums file
    rm -f "$checksum_file"

    # Generate checksums for all files
    cd "$RELEASE_DIR"

    # Find all image files (not individual .sha256 files)
    local files=()
    for arch in "${ARCHES[@]}"; do
        [[ -f "phase-boot-${VERSION}-${arch}.img" ]] && files+=("phase-boot-${VERSION}-${arch}.img")
        [[ -f "phase-boot-${VERSION}-${arch}.qcow2" ]] && files+=("phase-boot-${VERSION}-${arch}.qcow2")
    done

    if [[ ${#files[@]} -eq 0 ]]; then
        warn "No image files found for checksum generation"
        cd - >/dev/null
        return 1
    fi

    # Generate combined checksums
    sha256sum "${files[@]}" > "$checksum_file"

    cd - >/dev/null

    ok "SHA256SUMS generated"
    echo ""
    cat "$checksum_file" | sed 's/^/  /'
}

#------------------------------------------------------------------------------
# Generate Release Notes Template
#------------------------------------------------------------------------------

generate_release_notes() {
    section "Generating Release Notes"

    local notes_file="${RELEASE_DIR}/RELEASE_NOTES.md"

    info "Creating release notes template..."

    cat > "$notes_file" <<EOF
# Phase Boot ${VERSION} Release Notes

**Release Date:** $(date +"%Y-%m-%d")

## Overview

[Briefly describe this release - what milestone, major features, or changes]

## What's New

### Features
- [Feature 1]
- [Feature 2]
- [Feature 3]

### Improvements
- [Improvement 1]
- [Improvement 2]

### Bug Fixes
- [Fix 1]
- [Fix 2]

## Download

### x86_64 (Intel/AMD)
EOF

    # Add download links for x86_64 if built
    if [[ " ${ARCHES[@]} " =~ " x86_64 " ]]; then
        cat >> "$notes_file" <<EOF

**USB Image:** \`phase-boot-${VERSION}-x86_64.img\`
- Size: $(du -h "${RELEASE_DIR}/phase-boot-${VERSION}-x86_64.img" 2>/dev/null | cut -f1 || echo "N/A")
- SHA256: \`$(grep "phase-boot-${VERSION}-x86_64.img$" "${RELEASE_DIR}/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 || echo "N/A")\`

EOF
        if [[ "$BUILD_QCOW2" == "true" && -f "${RELEASE_DIR}/phase-boot-${VERSION}-x86_64.qcow2" ]]; then
            cat >> "$notes_file" <<EOF
**QCOW2 Image:** \`phase-boot-${VERSION}-x86_64.qcow2\`
- Size: $(du -h "${RELEASE_DIR}/phase-boot-${VERSION}-x86_64.qcow2" 2>/dev/null | cut -f1 || echo "N/A")
- SHA256: \`$(grep "phase-boot-${VERSION}-x86_64.qcow2$" "${RELEASE_DIR}/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 || echo "N/A")\`

EOF
        fi
    fi

    # Add download links for arm64 if built
    if [[ " ${ARCHES[@]} " =~ " arm64 " ]]; then
        cat >> "$notes_file" <<EOF
### arm64 (ARM 64-bit)

**USB Image:** \`phase-boot-${VERSION}-arm64.img\`
- Size: $(du -h "${RELEASE_DIR}/phase-boot-${VERSION}-arm64.img" 2>/dev/null | cut -f1 || echo "N/A")
- SHA256: \`$(grep "phase-boot-${VERSION}-arm64.img$" "${RELEASE_DIR}/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 || echo "N/A")\`

EOF
        if [[ "$BUILD_QCOW2" == "true" && -f "${RELEASE_DIR}/phase-boot-${VERSION}-arm64.qcow2" ]]; then
            cat >> "$notes_file" <<EOF
**QCOW2 Image:** \`phase-boot-${VERSION}-arm64.qcow2\`
- Size: $(du -h "${RELEASE_DIR}/phase-boot-${VERSION}-arm64.qcow2" 2>/dev/null | cut -f1 || echo "N/A")
- SHA256: \`$(grep "phase-boot-${VERSION}-arm64.qcow2$" "${RELEASE_DIR}/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 || echo "N/A")\`

EOF
        fi
    fi

    cat >> "$notes_file" <<EOF
## Installation

### Writing to USB Drive

\`\`\`bash
# Linux/macOS
sudo dd if=phase-boot-${VERSION}-ARCH.img of=/dev/sdX bs=4M status=progress
sync

# Windows (use Rufus or similar tool in DD mode)
\`\`\`

### Testing in QEMU

\`\`\`bash
# x86_64
qemu-system-x86_64 -enable-kvm -m 2G \\
  -bios /usr/share/ovmf/OVMF.fd \\
  -drive file=phase-boot-${VERSION}-x86_64.qcow2,format=qcow2,if=virtio

# arm64
qemu-system-aarch64 -M virt -cpu cortex-a57 -m 2G \\
  -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \\
  -drive file=phase-boot-${VERSION}-arm64.qcow2,format=qcow2,if=virtio
\`\`\`

## Known Issues

- [Issue 1]
- [Issue 2]

## Verification

Verify image integrity:

\`\`\`bash
sha256sum -c SHA256SUMS
\`\`\`

## Build Information

- **Build Date:** $(date -u +"%Y-%m-%d %H:%M:%S UTC")
- **Architectures:** ${ARCHES[*]}
- **Image Size:** ${IMAGE_SIZE}

## Previous Releases

- [Link to previous release notes]

---

**Full Changelog:** [link to detailed changelog]
EOF

    ok "Release notes template created: $(basename "$notes_file")"
}

#------------------------------------------------------------------------------
# Print Build Summary
#------------------------------------------------------------------------------

print_summary() {
    section "Build Summary"

    echo "Version:      $VERSION"
    echo "Architectures: ${ARCHES[*]}"
    echo "Output:       $RELEASE_DIR"
    echo ""
    echo "Build Results:"
    echo "  Total:      $TOTAL_BUILDS"
    echo "  Successful: $SUCCESSFUL_BUILDS"
    echo "  Failed:     $FAILED_BUILDS"
    echo ""

    if [[ $FAILED_BUILDS -eq 0 ]]; then
        ok "All builds completed successfully!"
    else
        warn "$FAILED_BUILDS build(s) failed"
    fi

    echo ""
    info "Release artifacts:"
    ls -lh "$RELEASE_DIR" | tail -n +2 | awk '{printf "  %s  %s\n", $5, $9}'
}

#------------------------------------------------------------------------------
# Main Build Process
#------------------------------------------------------------------------------

main() {
    section "Phase Boot Release Builder"

    echo "Version:       $VERSION"
    echo "Architectures: ${ARCHES[*]}"
    echo "Output:        $RELEASE_DIR"
    echo "USB Images:    $BUILD_USB"
    echo "QCOW2 Images:  $BUILD_QCOW2"
    echo "Image Size:    $IMAGE_SIZE"
    echo ""

    # Create release directory
    mkdir -p "$RELEASE_DIR"
    ok "Release directory: $RELEASE_DIR"

    # Build for each architecture
    for arch in "${ARCHES[@]}"; do
        section "Building for $arch"

        # Build USB image
        if [[ "$BUILD_USB" == "true" ]]; then
            build_usb "$arch" || true
            echo ""
        fi

        # Build QCOW2 image
        if [[ "$BUILD_QCOW2" == "true" ]]; then
            build_qcow2 "$arch" || true
            echo ""
        fi
    done

    # Generate combined checksums
    generate_checksums

    # Generate release notes template
    echo ""
    generate_release_notes

    # Print summary
    echo ""
    print_summary

    # Exit with error if any builds failed
    [[ $FAILED_BUILDS -gt 0 ]] && exit 1
    exit 0
}

#------------------------------------------------------------------------------
# Execute
#------------------------------------------------------------------------------

main
