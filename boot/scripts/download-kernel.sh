#!/bin/bash
# download-kernel.sh - Download pre-built kernels from Alpine Linux
# Part of Phase Boot - minimal bootloader system

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default values
ARCH="x86_64"
OUTPUT_DIR="$(dirname "$0")/../build/kernel"
VERSION="edge"
ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --arch)
            ARCH="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Download pre-built kernels from Alpine Linux"
            echo ""
            echo "Options:"
            echo "  --arch ARCH       Architecture: x86_64 or arm64 (default: x86_64)"
            echo "  --output DIR      Output directory (default: ../build/kernel)"
            echo "  --version VER     Alpine version: edge, v3.19, etc (default: edge)"
            echo "  -h, --help        Show this help message"
            echo ""
            echo "Example:"
            echo "  $0 --arch x86_64 --output ./kernel"
            exit 0
            ;;
        *)
            echo -e "${RED}Error: Unknown option $1${NC}"
            exit 1
            ;;
    esac
done

# Validate architecture
case "$ARCH" in
    x86_64)
        ALPINE_ARCH="x86_64"
        KERNEL_NAME="vmlinuz-x86_64"
        ;;
    arm64)
        ALPINE_ARCH="aarch64"
        KERNEL_NAME="vmlinuz-arm64"
        ;;
    *)
        echo -e "${RED}Error: Unsupported architecture: $ARCH${NC}"
        echo "Supported: x86_64, arm64"
        exit 1
        ;;
esac

echo -e "${BLUE}Phase Boot Kernel Downloader${NC}"
echo -e "${BLUE}============================${NC}"
echo ""
echo "Architecture: $ARCH ($ALPINE_ARCH)"
echo "Version:      $VERSION"
echo "Output:       $OUTPUT_DIR"
echo ""

# Create output directory
mkdir -p "$OUTPUT_DIR"
cd "$OUTPUT_DIR"

# Determine the release URL
RELEASES_URL="${ALPINE_MIRROR}/${VERSION}/releases/${ALPINE_ARCH}"
echo -e "${BLUE}Fetching release information...${NC}"

# Get the latest release file list
RELEASE_INDEX=$(curl -sL "$RELEASES_URL/latest-releases.yaml" 2>/dev/null || true)

if [ -z "$RELEASE_INDEX" ]; then
    echo -e "${YELLOW}Warning: Could not fetch latest-releases.yaml, using fallback method${NC}"
    # Fallback: try to get directory listing
    RELEASE_LIST=$(curl -sL "$RELEASES_URL/" | grep -oP 'alpine-minirootfs-[0-9.]+-'"$ALPINE_ARCH"'\.tar\.gz' | head -1)
    if [ -z "$RELEASE_LIST" ]; then
        echo -e "${RED}Error: Could not determine latest release${NC}"
        exit 1
    fi
    MINIROOTFS_FILE="$RELEASE_LIST"
else
    # Parse YAML to get minirootfs file
    MINIROOTFS_FILE=$(echo "$RELEASE_INDEX" | grep -A5 "flavor: alpine-minirootfs" | grep "file:" | head -1 | awk '{print $2}')
    if [ -z "$MINIROOTFS_FILE" ]; then
        echo -e "${RED}Error: Could not parse release information${NC}"
        exit 1
    fi
fi

echo "Release file: $MINIROOTFS_FILE"

# Download minirootfs to extract kernel
MINIROOTFS_URL="${RELEASES_URL}/${MINIROOTFS_FILE}"
CHECKSUM_URL="${RELEASES_URL}/${MINIROOTFS_FILE}.sha256"

echo ""
echo -e "${BLUE}Downloading checksums...${NC}"
if ! curl -fL -o "${MINIROOTFS_FILE}.sha256" "$CHECKSUM_URL" 2>/dev/null; then
    echo -e "${YELLOW}Warning: Could not download checksum file${NC}"
    SKIP_CHECKSUM=1
else
    echo -e "${GREEN}✓ Checksums downloaded${NC}"
    SKIP_CHECKSUM=0
fi

# Download the minirootfs
echo ""
echo -e "${BLUE}Downloading Alpine minirootfs...${NC}"
if [ -f "$MINIROOTFS_FILE" ]; then
    echo -e "${YELLOW}File already exists, skipping download${NC}"
else
    if ! curl -fL --progress-bar -o "$MINIROOTFS_FILE" "$MINIROOTFS_URL"; then
        echo -e "${RED}Error: Failed to download $MINIROOTFS_FILE${NC}"
        exit 1
    fi
    echo -e "${GREEN}✓ Download complete${NC}"
fi

# Verify checksum
if [ $SKIP_CHECKSUM -eq 0 ]; then
    echo ""
    echo -e "${BLUE}Verifying checksum...${NC}"
    if sha256sum -c "${MINIROOTFS_FILE}.sha256" 2>/dev/null | grep -q OK; then
        echo -e "${GREEN}✓ Checksum verified${NC}"
    else
        echo -e "${RED}Error: Checksum verification failed${NC}"
        exit 1
    fi
fi

# Now we need to get the actual kernel
# Alpine minirootfs doesn't include kernel, we need to download kernel package
echo ""
echo -e "${BLUE}Downloading kernel packages...${NC}"

# Determine package repository
REPO_URL="${ALPINE_MIRROR}/${VERSION}/main/${ALPINE_ARCH}"

# Download APKINDEX to find kernel package
echo "Fetching package index..."
if ! curl -fL -o APKINDEX.tar.gz "${REPO_URL}/APKINDEX.tar.gz" 2>/dev/null; then
    echo -e "${RED}Error: Could not download package index${NC}"
    exit 1
fi

# Extract and find linux-lts package
tar -xzf APKINDEX.tar.gz APKINDEX 2>/dev/null || {
    echo -e "${RED}Error: Could not extract package index${NC}"
    exit 1
}

# Find the linux-lts package name
KERNEL_PKG=$(grep -A20 "^P:linux-lts$" APKINDEX | grep "^V:" | head -1 | cut -d: -f2)
KERNEL_REL=$(grep -A20 "^P:linux-lts$" APKINDEX | grep "^V:" | head -1 | cut -d- -f3)

if [ -z "$KERNEL_PKG" ]; then
    echo -e "${RED}Error: Could not find linux-lts package${NC}"
    exit 1
fi

KERNEL_APK="linux-lts-${KERNEL_PKG}.apk"
KERNEL_URL="${REPO_URL}/${KERNEL_APK}"

echo "Kernel package: $KERNEL_APK"
echo ""

# Download kernel package
echo -e "${BLUE}Downloading kernel package...${NC}"
if ! curl -fL --progress-bar -o "$KERNEL_APK" "$KERNEL_URL"; then
    echo -e "${RED}Error: Failed to download kernel package${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Kernel package downloaded${NC}"

# Extract kernel from package
echo ""
echo -e "${BLUE}Extracting kernel...${NC}"

# APK files are tar.gz archives
mkdir -p kernel-extract
cd kernel-extract

if ! tar -xzf "../$KERNEL_APK" 2>/dev/null; then
    echo -e "${RED}Error: Could not extract kernel package${NC}"
    exit 1
fi

# Find vmlinuz file
VMLINUZ_PATH=$(find . -name "vmlinuz-lts" -o -name "vmlinuz*" | head -1)

if [ -z "$VMLINUZ_PATH" ]; then
    echo -e "${RED}Error: Could not find vmlinuz in package${NC}"
    exit 1
fi

# Copy kernel to output directory with standardized name
cp "$VMLINUZ_PATH" "../$KERNEL_NAME"
cd ..

# Clean up
rm -rf kernel-extract
rm -f APKINDEX APKINDEX.tar.gz

echo -e "${GREEN}✓ Kernel extracted: $KERNEL_NAME${NC}"

# Also extract modules if needed
echo ""
echo -e "${BLUE}Downloading kernel modules...${NC}"

# Find linux-lts package for modules
MODULES_PKG="linux-lts-${KERNEL_PKG}.apk"
# Already downloaded as KERNEL_APK, so we extract modules

mkdir -p modules-extract
cd modules-extract
tar -xzf "../$KERNEL_APK" 2>/dev/null || true

# Copy modules if they exist
if [ -d "lib/modules" ]; then
    mkdir -p ../modules
    cp -r lib/modules/* ../modules/ 2>/dev/null || true
    echo -e "${GREEN}✓ Modules extracted${NC}"
else
    echo -e "${YELLOW}Note: No modules found in package${NC}"
fi

cd ..
rm -rf modules-extract

# Get kernel version
KERNEL_VERSION=$(strings "$KERNEL_NAME" | grep -E "^[0-9]+\.[0-9]+\.[0-9]+" | head -1)
if [ -z "$KERNEL_VERSION" ]; then
    KERNEL_VERSION="unknown"
fi

# Create metadata file
cat > kernel-info.txt <<EOF
Kernel Information
==================
Architecture: $ARCH ($ALPINE_ARCH)
Alpine Version: $VERSION
Kernel File: $KERNEL_NAME
Kernel Version: $KERNEL_VERSION
Package: $KERNEL_APK
Downloaded: $(date -u +"%Y-%m-%d %H:%M:%S UTC")
Source: $KERNEL_URL
EOF

echo ""
echo -e "${GREEN}✓ Kernel download complete!${NC}"
echo ""
echo "Output directory: $(pwd)"
echo "Kernel file: $KERNEL_NAME"
echo "Kernel version: $KERNEL_VERSION"
echo ""

# Display file info
ls -lh "$KERNEL_NAME"
echo ""
echo -e "${BLUE}Files available:${NC}"
ls -lh

# Optional cleanup
echo ""
read -p "Remove temporary files (minirootfs, apk)? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    rm -f "$MINIROOTFS_FILE" "$MINIROOTFS_FILE.sha256" "$KERNEL_APK"
    echo -e "${GREEN}✓ Temporary files removed${NC}"
fi

echo ""
echo -e "${GREEN}Done! Kernel ready to use.${NC}"
