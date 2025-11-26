#!/bin/sh
#
# Phase Boot - Mode Handler
# Orchestrates boot based on phase.mode
#

set -e

SCRIPTS_DIR="$(dirname "$0")"
LOG_FILE="/tmp/phase-boot.log"

# Logging
log() {
    echo "[MODE] $1"
    echo "$(date '+%H:%M:%S') [MODE] $1" >> "$LOG_FILE"
}

warn() {
    echo "[MODE] WARNING: $1"
    echo "$(date '+%H:%M:%S') [MODE] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[MODE] ERROR: $1"
    echo "$(date '+%H:%M:%S') [MODE] ERROR: $1" >> "$LOG_FILE"
}

# Parse phase parameters from kernel cmdline
parse_phase_params() {
    local cmdline
    cmdline=$(cat /proc/cmdline 2>/dev/null || echo "")

    # Defaults
    PHASE_MODE="internet"
    PHASE_CHANNEL="stable"
    PHASE_CACHE="true"
    PHASE_NOWRITE="false"

    for param in $cmdline; do
        case "$param" in
            phase.mode=*)
                PHASE_MODE="${param#phase.mode=}"
                ;;
            phase.channel=*)
                PHASE_CHANNEL="${param#phase.channel=}"
                ;;
            phase.cache=*)
                PHASE_CACHE="${param#phase.cache=}"
                ;;
            phase.nowrite=*)
                PHASE_NOWRITE="${param#phase.nowrite=}"
                ;;
        esac
    done

    # Validate mode
    case "$PHASE_MODE" in
        internet|local|private)
            ;;
        *)
            warn "Unknown phase.mode=$PHASE_MODE, defaulting to 'internet'"
            PHASE_MODE="internet"
            ;;
    esac

    log "Phase parameters:"
    log "  Mode:     $PHASE_MODE"
    log "  Channel:  $PHASE_CHANNEL"
    log "  Cache:    $PHASE_CACHE"
    log "  No-Write: $PHASE_NOWRITE"
}

# Check network status
check_network() {
    log "Checking network status..."

    if [ -f /tmp/network.status ] && [ "$(cat /tmp/network.status)" = "up" ]; then
        NETWORK_UP="true"
        NETWORK_INTERFACE=$(cat /tmp/network.interface 2>/dev/null || echo "unknown")
        NETWORK_IP=$(cat /tmp/network.ip 2>/dev/null || echo "unknown")
        log "Network is UP: $NETWORK_INTERFACE ($NETWORK_IP)"
        return 0
    else
        NETWORK_UP="false"
        log "Network is DOWN"
        return 1
    fi
}

# Handle Internet mode
handle_internet_mode() {
    log "=== Internet Mode ==="
    log "Full network access with DHT discovery"

    # Verify network is available
    if [ "$NETWORK_UP" != "true" ]; then
        error "Internet mode requires network access"
        error "Network initialization failed"
        return 1
    fi

    # Run DHT discovery to find manifest
    log "Running DHT discovery..."

    if [ ! -x "/bin/phase-discover" ]; then
        error "phase-discover binary not found"
        error "Cannot proceed with Internet mode"
        return 1
    fi

    # Build discovery arguments
    local discover_args="--arch $(uname -m) --channel $PHASE_CHANNEL --timeout 60"

    log "Discovery command: phase-discover $discover_args"

    if /bin/phase-discover $discover_args --quiet > /tmp/manifest_url 2>> "$LOG_FILE"; then
        MANIFEST_URL=$(cat /tmp/manifest_url)
        log "Discovery successful: $MANIFEST_URL"
    else
        error "DHT discovery failed or timed out"
        return 1
    fi

    # Fetch manifest (M3 feature - placeholder for now)
    log "Fetching manifest from: $MANIFEST_URL"
    warn "M3 manifest fetch not yet implemented"
    # TODO: M3 will implement: curl/wget manifest, parse, verify signatures

    # Check cache first if enabled
    if [ "$PHASE_CACHE" = "true" ]; then
        log "Cache enabled - checking for cached image..."
        warn "Cache implementation not yet available"
        # TODO: M3 will check cache based on manifest hash
    fi

    # Download kernel and initramfs (M3 feature - placeholder)
    log "Would download kernel and initramfs here (M3)"
    warn "M3 download not yet implemented"
    # TODO: M3 will implement: download kernel, initramfs, verify hashes

    # For now, check if we have a kernel already available
    if [ -f "/tmp/phase-kernel" ] && [ -f "/tmp/phase-initramfs" ]; then
        log "Found downloaded kernel and initramfs"

        # Execute kexec
        log "Preparing to kexec into new kernel..."

        if [ -x "$SCRIPTS_DIR/kexec-boot.sh" ]; then
            log "Executing kexec-boot.sh..."
            exec "$SCRIPTS_DIR/kexec-boot.sh" \
                --kernel /tmp/phase-kernel \
                --initramfs /tmp/phase-initramfs
        else
            error "kexec-boot.sh not found or not executable"
            return 1
        fi
    else
        warn "Kernel/initramfs not available - M3 download not implemented"
        log "Dropping to shell for manual testing"
        return 1
    fi

    return 0
}

# Handle Local mode
handle_local_mode() {
    log "=== Local Mode ==="
    log "LAN-only access with local cache preferred"

    # Check network (LAN is OK, Internet not required)
    if [ "$NETWORK_UP" = "true" ]; then
        log "Network available for LAN access"

        # Try mDNS discovery on local network
        log "Attempting mDNS discovery on LAN..."
        warn "mDNS discovery not yet implemented"
        # TODO: M3+ will implement mDNS-based local discovery
    else
        log "No network - will use local cache only"
    fi

    # Check for local cache
    local cache_dir="/cache/phase"

    if [ -d "$cache_dir" ]; then
        log "Found cache directory: $cache_dir"

        # Look for latest cached image matching channel
        local cached_kernel cached_initramfs

        cached_kernel=$(find "$cache_dir" -name "*-$PHASE_CHANNEL-vmlinuz" -type f | sort -r | head -n 1)
        cached_initramfs=$(find "$cache_dir" -name "*-$PHASE_CHANNEL-initramfs" -type f | sort -r | head -n 1)

        if [ -n "$cached_kernel" ] && [ -n "$cached_initramfs" ]; then
            log "Found cached kernel: $cached_kernel"
            log "Found cached initramfs: $cached_initramfs"

            # Setup overlay for writable filesystem
            log "Setting up OverlayFS for writable root..."

            if [ -x "$SCRIPTS_DIR/overlayfs-setup.sh" ]; then
                # Create temporary directories for overlay
                local upper_dir="/tmp/overlay-upper"
                local work_dir="/tmp/overlay-work"
                local merged_dir="/tmp/newroot"

                # Note: In real implementation, lower would be extracted image
                # For now, just demonstrate overlay creation
                mkdir -p /tmp/overlay-lower

                if "$SCRIPTS_DIR/overlayfs-setup.sh" \
                    --lower /tmp/overlay-lower \
                    --upper "$upper_dir" \
                    --work "$work_dir" \
                    --merged "$merged_dir" 2>> "$LOG_FILE"; then
                    log "OverlayFS setup successful"
                else
                    error "OverlayFS setup failed"
                    return 1
                fi
            else
                warn "overlayfs-setup.sh not found"
            fi

            # Execute kexec with cached image
            if [ -x "$SCRIPTS_DIR/kexec-boot.sh" ]; then
                log "Executing kexec with cached image..."
                exec "$SCRIPTS_DIR/kexec-boot.sh" \
                    --kernel "$cached_kernel" \
                    --initramfs "$cached_initramfs"
            else
                error "kexec-boot.sh not found or not executable"
                return 1
            fi
        else
            warn "No cached images found for channel: $PHASE_CHANNEL"
            log "Cache requires: *-$PHASE_CHANNEL-vmlinuz and *-$PHASE_CHANNEL-initramfs"
        fi
    else
        log "No cache directory found at $cache_dir"
    fi

    log "Local mode cannot proceed - no cached image available"
    return 1
}

# Handle Private mode
handle_private_mode() {
    log "=== Private Mode ==="
    log "Ephemeral identity, no persistent writes"

    # Enforce no-write mode
    if [ "$PHASE_NOWRITE" != "true" ]; then
        log "Forcing phase.nowrite=true for Private mode"
        PHASE_NOWRITE="true"
    fi

    # Verify network is available
    if [ "$NETWORK_UP" != "true" ]; then
        error "Private mode requires network access for discovery"
        warn "Local cache not used in Private mode (could leak identity)"
        return 1
    fi

    # Run DHT discovery with ephemeral identity
    log "Running DHT discovery with ephemeral identity..."

    if [ ! -x "/bin/phase-discover" ]; then
        error "phase-discover binary not found"
        return 1
    fi

    # Build discovery arguments with ephemeral flag
    local discover_args="--arch $(uname -m) --channel $PHASE_CHANNEL --ephemeral --timeout 60"

    log "Discovery command: phase-discover $discover_args"

    if /bin/phase-discover $discover_args --quiet > /tmp/manifest_url 2>> "$LOG_FILE"; then
        MANIFEST_URL=$(cat /tmp/manifest_url)
        log "Discovery successful: $MANIFEST_URL"
    else
        error "DHT discovery failed or timed out"
        return 1
    fi

    # Download to tmpfs only (no disk caching)
    log "Will download to tmpfs only (no persistent storage)"
    warn "M3 download not yet implemented"
    # TODO: M3 will download directly to tmpfs

    # Setup overlay with tmpfs-only upper layer
    log "Setting up tmpfs-only overlay for truly ephemeral root..."

    if [ -x "$SCRIPTS_DIR/overlayfs-setup.sh" ]; then
        # Create tmpfs for upper layer
        local tmpfs_dir="/tmp/ephemeral"
        mkdir -p "$tmpfs_dir"

        if ! mount -t tmpfs -o size=1G tmpfs "$tmpfs_dir" 2>> "$LOG_FILE"; then
            error "Failed to create tmpfs for ephemeral storage"
            return 1
        fi

        local upper_dir="$tmpfs_dir/upper"
        local work_dir="$tmpfs_dir/work"
        local merged_dir="/tmp/newroot"

        mkdir -p /tmp/overlay-lower

        if "$SCRIPTS_DIR/overlayfs-setup.sh" \
            --lower /tmp/overlay-lower \
            --upper "$upper_dir" \
            --work "$work_dir" \
            --merged "$merged_dir" 2>> "$LOG_FILE"; then
            log "Ephemeral OverlayFS setup successful"
        else
            error "OverlayFS setup failed"
            return 1
        fi
    fi

    # Check if we have kernel/initramfs
    if [ -f "/tmp/phase-kernel" ] && [ -f "/tmp/phase-initramfs" ]; then
        log "Found kernel and initramfs in tmpfs"

        # Execute kexec
        if [ -x "$SCRIPTS_DIR/kexec-boot.sh" ]; then
            log "Executing kexec with ephemeral image..."
            exec "$SCRIPTS_DIR/kexec-boot.sh" \
                --kernel /tmp/phase-kernel \
                --initramfs /tmp/phase-initramfs \
                --cmdline "phase.nowrite=true"
        else
            error "kexec-boot.sh not found or not executable"
            return 1
        fi
    else
        warn "Kernel/initramfs not available - M3 download not implemented"
        log "Dropping to shell for manual testing"
        return 1
    fi

    return 0
}

# Main orchestration
main() {
    # Ensure log file exists
    touch "$LOG_FILE"

    log "=== Phase Boot Mode Handler ==="

    # Parse phase parameters
    parse_phase_params

    # Check network status
    check_network || true  # Don't fail here, modes will check individually

    # Route to appropriate mode handler
    case "$PHASE_MODE" in
        internet)
            if handle_internet_mode; then
                log "Internet mode completed successfully"
            else
                error "Internet mode failed"
                exit 1
            fi
            ;;

        local)
            if handle_local_mode; then
                log "Local mode completed successfully"
            else
                error "Local mode failed"
                warn "Falling back to shell"
                exit 1
            fi
            ;;

        private)
            if handle_private_mode; then
                log "Private mode completed successfully"
            else
                error "Private mode failed"
                exit 1
            fi
            ;;

        *)
            error "Unknown mode: $PHASE_MODE"
            exit 1
            ;;
    esac

    # Should not reach here if kexec was successful
    log "Mode handler completed, returning control to init"
    return 0
}

# Run main
main "$@"
