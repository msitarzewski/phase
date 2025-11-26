#!/bin/bash
set -e

IMAGE_FILE="$1"
ESP_SOURCE="$2"
ROOTFS_SOURCE="$3"

if [ "$EUID" -ne 0 ]; then
    echo "Requesting sudo for partition operations..."
    exec sudo "$0" "$@"
fi

echo "Setting up loop device..."
LOOP_DEV=$(losetup -f)
losetup -P "$LOOP_DEV" "$IMAGE_FILE"

# Wait for partition devices
sleep 1

echo "Formatting ESP partition (FAT32)..."
mkfs.vfat -F 32 -n "PHASE_ESP" "${LOOP_DEV}p1" >/dev/null

echo "Copying ESP contents..."
MOUNT_POINT=$(mktemp -d)
mount "${LOOP_DEV}p1" "$MOUNT_POINT"
cp -r "$ESP_SOURCE"/* "$MOUNT_POINT"/
sync
umount "$MOUNT_POINT"

echo "Creating seed partition..."
dd if="$ROOTFS_SOURCE" of="${LOOP_DEV}p2" bs=1M status=none

echo "Formatting cache partition (ext4)..."
mkfs.ext4 -L "PHASE_CACHE" -q "${LOOP_DEV}p3" >/dev/null

echo "Cleaning up..."
losetup -d "$LOOP_DEV"
rm -rf "$MOUNT_POINT"

echo "Image build complete!"
