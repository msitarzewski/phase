#!/bin/sh
#
# Phase Boot - Network Mode Dispatcher
# Initializes network based on boot mode (Internet/Local/Private)
#

set -e

SCRIPTS_DIR="$(dirname "$0")"
LOG_FILE="/tmp/network.log"

# Logging
log() {
    echo "[NET-INIT] $1"
    echo "$(date '+%H:%M:%S') [INIT] $1" >> "$LOG_FILE"
}

warn() {
    echo "[NET-INIT] WARNING: $1"
    echo "$(date '+%H:%M:%S') [INIT] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[NET-INIT] ERROR: $1"
    echo "$(date '+%H:%M:%S') [INIT] ERROR: $1" >> "$LOG_FILE"
}

# Parse mode from kernel cmdline
get_phase_mode() {
    local cmdline
    cmdline=$(cat /proc/cmdline 2>/dev/null || echo "")

    local mode="internet"  # Default

    for param in $cmdline; do
        case "$param" in
            phase.mode=*)
                mode="${param#phase.mode=}"
                ;;
        esac
    done

    echo "$mode"
}

# Initialize network
init_network() {
    local mode="$1"

    log "Initializing network for mode: $mode"

    # Always try wired first
    log "Attempting wired network..."
    if "$SCRIPTS_DIR/net-wired.sh"; then
        log "Wired network successful"
        return 0
    fi

    log "Wired network failed"

    # Handle based on mode
    case "$mode" in
        internet)
            log "Internet mode: Will try Wi-Fi if available"
            if [ -x "$SCRIPTS_DIR/net-wifi.sh" ]; then
                if "$SCRIPTS_DIR/net-wifi.sh"; then
                    log "Wi-Fi network successful"
                    return 0
                fi
            fi
            ;;

        local)
            log "Local mode: Will try Wi-Fi for LAN access"
            if [ -x "$SCRIPTS_DIR/net-wifi.sh" ]; then
                if "$SCRIPTS_DIR/net-wifi.sh"; then
                    log "Wi-Fi network successful"
                    return 0
                fi
            fi
            ;;

        private)
            warn "Private mode: Network required but consider privacy implications"
            echo ""
            echo "WARNING: Private mode requires network access."
            echo "Your network activity may be logged by your ISP."
            echo "Consider using a VPN or Tor for additional privacy."
            echo ""

            if [ -x "$SCRIPTS_DIR/net-wifi.sh" ]; then
                if "$SCRIPTS_DIR/net-wifi.sh"; then
                    log "Wi-Fi network successful"
                    return 0
                fi
            fi
            ;;
    esac

    error "Network initialization failed"
    return 1
}

# Display network status
show_status() {
    echo ""
    echo "=== Network Status ==="

    if [ -f /tmp/network.status ] && [ "$(cat /tmp/network.status)" = "up" ]; then
        local iface ip
        iface=$(cat /tmp/network.interface 2>/dev/null || echo "unknown")
        ip=$(cat /tmp/network.ip 2>/dev/null || echo "unknown")
        echo "Status:    UP"
        echo "Interface: $iface"
        echo "IP:        $ip"
    else
        echo "Status:    DOWN"
    fi

    echo "====================="
    echo ""
}

# Main
main() {
    touch "$LOG_FILE"

    local mode
    mode=$(get_phase_mode)

    log "Detected phase mode: $mode"
    echo "Phase mode: $mode"

    # Bring up loopback first
    log "Bringing up loopback interface"
    ip link set lo up 2>/dev/null || true

    # Initialize network
    if init_network "$mode"; then
        show_status
        return 0
    else
        show_status
        warn "Continuing without network (some features unavailable)"
        return 1
    fi
}

# Run main
main "$@"
