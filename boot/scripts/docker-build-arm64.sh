#!/bin/bash
# Build ARM64 Phase Boot image using Docker
# This enables building on macOS

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BOOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== Phase Boot ARM64 Docker Build ==="
echo "Boot directory: $BOOT_DIR"
echo ""

# Check Docker
if ! command -v docker &> /dev/null; then
    echo "ERROR: Docker not found"
    exit 1
fi

# Build in Docker container
echo "Starting Docker build..."
docker run --rm \
    -v "$BOOT_DIR:/work" \
    -w /work \
    --platform linux/arm64 \
    ubuntu:22.04 \
    bash -c '
        set -e
        echo "=== Installing dependencies ==="
        apt-get update -qq
        apt-get install -y -qq \
            build-essential \
            squashfs-tools \
            dosfstools \
            gdisk \
            cpio \
            gzip \
            wget \
            curl \
            busybox-static \
            grub-efi-arm64-bin \
            > /dev/null 2>&1
        
        echo "=== Downloading ARM64 kernel ==="
        # Non-interactive kernel download
        mkdir -p build/kernel
        cd build/kernel
        
        ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"
        VERSION="edge"
        ALPINE_ARCH="aarch64"
        
        # Get package index
        REPO_URL="${ALPINE_MIRROR}/${VERSION}/main/${ALPINE_ARCH}"
        curl -fsSL -o APKINDEX.tar.gz "${REPO_URL}/APKINDEX.tar.gz"
        tar -xzf APKINDEX.tar.gz APKINDEX
        
        # Find kernel package version
        KERNEL_PKG=$(grep -A20 "^P:linux-lts$" APKINDEX | grep "^V:" | head -1 | cut -d: -f2)
        KERNEL_APK="linux-lts-${KERNEL_PKG}.apk"
        
        echo "Downloading: $KERNEL_APK"
        curl -fsSL -o "$KERNEL_APK" "${REPO_URL}/${KERNEL_APK}"
        
        # Extract kernel
        mkdir -p kernel-extract
        cd kernel-extract
        tar -xzf "../$KERNEL_APK"
        VMLINUZ=$(find . -name "vmlinuz*" | head -1)
        cp "$VMLINUZ" ../vmlinuz-arm64
        cd ..
        
        # Cleanup
        rm -rf kernel-extract APKINDEX* "$KERNEL_APK"
        
        echo "Kernel ready: build/kernel/vmlinuz-arm64"
        ls -lh vmlinuz-arm64
        cd /work
        
        echo ""
        echo "=== Building ARM64 image ==="
        make ARCH=arm64 esp initramfs rootfs
        
        echo ""
        echo "=== Build complete ==="
        echo "ESP contents:"
        ls -la build/esp/
        echo ""
        echo "Initramfs:"
        ls -lh build/initramfs/initramfs-arm64.img
        echo ""
        echo "Rootfs:"
        ls -lh build/rootfs/rootfs-arm64.sqfs
    '

echo ""
echo "=== Docker build finished ==="
echo ""
echo "Build artifacts in: $BOOT_DIR/build/"
ls -la "$BOOT_DIR/build/" 2>/dev/null || echo "Build directory contents not visible yet"
