#!/bin/sh
#
# Phase Boot - OverlayFS Setup
# Creates writable overlay for boot filesystem
#

set -e

SCRIPTS_DIR="$(dirname "$0")"
LOG_FILE="/tmp/phase-boot.log"

# Logging
log() {
    echo "[OVERLAY] $1"
    echo "$(date '+%H:%M:%S') [OVERLAY] $1" >> "$LOG_FILE"
}

warn() {
    echo "[OVERLAY] WARNING: $1"
    echo "$(date '+%H:%M:%S') [OVERLAY] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[OVERLAY] ERROR: $1"
    echo "$(date '+%H:%M:%S') [OVERLAY] ERROR: $1" >> "$LOG_FILE"
}

# Parse command line arguments
parse_args() {
    LOWER_DIR=""
    UPPER_DIR=""
    WORK_DIR=""
    MERGED_DIR=""

    while [ $# -gt 0 ]; do
        case "$1" in
            --lower)
                LOWER_DIR="$2"
                shift 2
                ;;
            --upper)
                UPPER_DIR="$2"
                shift 2
                ;;
            --work)
                WORK_DIR="$2"
                shift 2
                ;;
            --merged)
                MERGED_DIR="$2"
                shift 2
                ;;
            *)
                error "Unknown argument: $1"
                usage
                exit 1
                ;;
        esac
    done

    # Validate required arguments
    if [ -z "$LOWER_DIR" ]; then
        error "Missing required argument: --lower"
        usage
        exit 1
    fi

    if [ -z "$UPPER_DIR" ]; then
        error "Missing required argument: --upper"
        usage
        exit 1
    fi

    if [ -z "$WORK_DIR" ]; then
        error "Missing required argument: --work"
        usage
        exit 1
    fi

    if [ -z "$MERGED_DIR" ]; then
        error "Missing required argument: --merged"
        usage
        exit 1
    fi
}

# Usage information
usage() {
    cat <<EOF
Usage: $0 --lower PATH --upper PATH --work PATH --merged PATH

Required:
  --lower PATH    Lower (read-only) directory
  --upper PATH    Upper (read-write) directory
  --work PATH     Work directory (must be on same filesystem as upper)
  --merged PATH   Merged mount point

Description:
  Creates an OverlayFS mount that combines a read-only lower directory
  with a read-write upper directory, presenting a unified view at the
  merged mount point. Changes are written to the upper directory only.

Examples:
  # Basic overlay
  $0 --lower /mnt/base --upper /tmp/upper --work /tmp/work --merged /mnt/root

  # Cache overlay for downloaded images
  $0 --lower /cache/image --upper /tmp/changes --work /tmp/work --merged /newroot

Notes:
  - lower directory must exist and be readable
  - upper and work directories will be created if they don't exist
  - work directory must be on the same filesystem as upper
  - merged directory will be created if it doesn't exist

EOF
}

# Validate lower directory
validate_lower() {
    local dir="$1"

    if [ ! -d "$dir" ]; then
        error "Lower directory does not exist: $dir"
        return 1
    fi

    if [ ! -r "$dir" ]; then
        error "Lower directory not readable: $dir"
        return 1
    fi

    log "Lower directory validated: $dir"
    return 0
}

# Create directory if it doesn't exist
ensure_directory() {
    local dir="$1"
    local desc="$2"

    if [ ! -d "$dir" ]; then
        log "Creating $desc: $dir"
        if ! mkdir -p "$dir" 2>> "$LOG_FILE"; then
            error "Failed to create $desc: $dir"
            return 1
        fi
    else
        log "$desc exists: $dir"
    fi

    # Verify directory is writable (except for merged, which will be mount point)
    if [ "$desc" != "merged directory" ]; then
        if [ ! -w "$dir" ]; then
            error "$desc not writable: $dir"
            return 1
        fi
    fi

    return 0
}

# Check if overlay is supported
check_overlay_support() {
    log "Checking OverlayFS support..."

    # Check if overlay module is loaded or built-in
    if ! grep -q overlay /proc/filesystems 2>/dev/null; then
        warn "OverlayFS not found in /proc/filesystems, attempting to load module..."

        if ! modprobe overlay 2>> "$LOG_FILE"; then
            error "Failed to load overlay module"
            error "OverlayFS may not be supported by this kernel"
            return 1
        fi

        # Check again
        if ! grep -q overlay /proc/filesystems 2>/dev/null; then
            error "OverlayFS still not available after modprobe"
            return 1
        fi
    fi

    log "OverlayFS support confirmed"
    return 0
}

# Verify work and upper are on same filesystem
verify_same_filesystem() {
    local upper="$1"
    local work="$2"

    log "Verifying upper and work directories are on same filesystem..."

    # Get device numbers
    local upper_dev work_dev

    # Use stat if available
    if command -v stat >/dev/null 2>&1; then
        upper_dev=$(stat -c '%d' "$upper" 2>/dev/null || echo "")
        work_dev=$(stat -c '%d' "$work" 2>/dev/null || echo "")

        if [ -n "$upper_dev" ] && [ -n "$work_dev" ]; then
            if [ "$upper_dev" != "$work_dev" ]; then
                error "Upper and work directories must be on the same filesystem"
                error "Upper device: $upper_dev"
                error "Work device:  $work_dev"
                return 1
            fi
            log "Filesystem verification passed"
            return 0
        fi
    fi

    # Fallback: Just warn, mount will fail if it's actually an issue
    warn "Could not verify filesystem match (stat unavailable)"
    warn "Mount will fail if upper and work are on different filesystems"
    return 0
}

# Mount overlay filesystem
mount_overlay() {
    local lower="$1"
    local upper="$2"
    local work="$3"
    local merged="$4"

    log "Mounting OverlayFS..."
    log "  Lower:  $lower"
    log "  Upper:  $upper"
    log "  Work:   $work"
    log "  Merged: $merged"

    # Build mount options
    local mount_opts="lowerdir=$lower,upperdir=$upper,workdir=$work"

    # Execute mount
    if ! mount -t overlay overlay -o "$mount_opts" "$merged" 2>> "$LOG_FILE"; then
        error "Failed to mount overlay filesystem"
        error "Mount command: mount -t overlay overlay -o '$mount_opts' '$merged'"
        return 1
    fi

    log "OverlayFS mounted successfully"
    return 0
}

# Verify mount succeeded
verify_mount() {
    local merged="$1"

    log "Verifying mount..."

    # Check if directory is a mount point
    if ! mountpoint -q "$merged" 2>/dev/null; then
        # Fallback check using mount command
        if ! mount | grep -q " on $merged "; then
            error "Verification failed: $merged is not a mount point"
            return 1
        fi
    fi

    # Check if we can read the directory
    if ! ls "$merged" >/dev/null 2>&1; then
        error "Verification failed: Cannot read merged directory"
        return 1
    fi

    # Test write (unless we're in phase.nowrite mode)
    local phase_nowrite
    phase_nowrite=$(grep -o 'phase\.nowrite=true' /proc/cmdline 2>/dev/null || echo "")

    if [ -z "$phase_nowrite" ]; then
        local test_file="$merged/.overlay_test_$$"
        if ! touch "$test_file" 2>/dev/null; then
            error "Verification failed: Cannot write to merged directory"
            return 1
        fi
        rm -f "$test_file" 2>/dev/null || true
    fi

    log "Mount verification successful"
    return 0
}

# Main
main() {
    # Ensure log file exists
    touch "$LOG_FILE"

    log "=== Phase Boot OverlayFS Setup ==="

    # Parse arguments
    parse_args "$@"

    # Check overlay support
    if ! check_overlay_support; then
        error "OverlayFS setup failed: kernel support check failed"
        exit 1
    fi

    # Validate lower directory
    if ! validate_lower "$LOWER_DIR"; then
        error "OverlayFS setup failed: lower directory validation failed"
        exit 1
    fi

    # Ensure upper directory exists
    if ! ensure_directory "$UPPER_DIR" "upper directory"; then
        error "OverlayFS setup failed: upper directory creation failed"
        exit 1
    fi

    # Ensure work directory exists
    if ! ensure_directory "$WORK_DIR" "work directory"; then
        error "OverlayFS setup failed: work directory creation failed"
        exit 1
    fi

    # Ensure merged directory exists
    if ! ensure_directory "$MERGED_DIR" "merged directory"; then
        error "OverlayFS setup failed: merged directory creation failed"
        exit 1
    fi

    # Verify upper and work are on same filesystem
    if ! verify_same_filesystem "$UPPER_DIR" "$WORK_DIR"; then
        error "OverlayFS setup failed: filesystem verification failed"
        exit 1
    fi

    # Mount overlay
    if ! mount_overlay "$LOWER_DIR" "$UPPER_DIR" "$WORK_DIR" "$MERGED_DIR"; then
        error "OverlayFS setup failed: mount failed"
        exit 1
    fi

    # Verify mount
    if ! verify_mount "$MERGED_DIR"; then
        error "OverlayFS setup failed: mount verification failed"
        # Attempt cleanup
        umount "$MERGED_DIR" 2>/dev/null || true
        exit 1
    fi

    log "OverlayFS setup complete"
    log "Merged filesystem available at: $MERGED_DIR"

    return 0
}

# Run main
main "$@"
