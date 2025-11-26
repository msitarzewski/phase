#!/bin/sh
#
# Phase Boot - Wired Network Initialization
# Brings up wired Ethernet interfaces and obtains DHCP lease
#

set -e

# Configuration
MAX_RETRIES=3
RETRY_DELAY=5
DHCP_TIMEOUT=10
PING_TARGET="1.1.1.1"
LOG_FILE="/tmp/network.log"

# Logging
log() {
    echo "[NET-WIRED] $1"
    echo "$(date '+%H:%M:%S') [WIRED] $1" >> "$LOG_FILE"
}

warn() {
    echo "[NET-WIRED] WARNING: $1"
    echo "$(date '+%H:%M:%S') [WIRED] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[NET-WIRED] ERROR: $1"
    echo "$(date '+%H:%M:%S') [WIRED] ERROR: $1" >> "$LOG_FILE"
}

# Find wired interfaces
find_wired_interfaces() {
    # Look for eth* and en* interfaces (exclude loopback and wireless)
    for iface in /sys/class/net/*; do
        iface_name=$(basename "$iface")
        case "$iface_name" in
            eth*|en*|eno*|enp*|ens*)
                # Check it's not wireless
                if [ ! -d "/sys/class/net/$iface_name/wireless" ]; then
                    echo "$iface_name"
                fi
                ;;
        esac
    done
}

# Bring up interface
bring_up_interface() {
    local iface="$1"

    log "Bringing up interface: $iface"

    # Check if interface exists
    if [ ! -d "/sys/class/net/$iface" ]; then
        error "Interface $iface not found"
        return 1
    fi

    # Bring interface up
    ip link set "$iface" up
    sleep 1

    # Check link state
    local state
    state=$(cat "/sys/class/net/$iface/operstate" 2>/dev/null || echo "unknown")
    log "Interface $iface state: $state"

    if [ "$state" = "up" ] || [ "$state" = "unknown" ]; then
        return 0
    fi

    warn "Interface $iface not fully up (state: $state)"
    return 0  # Continue anyway, DHCP might work
}

# Request DHCP lease
request_dhcp() {
    local iface="$1"

    log "Requesting DHCP lease on $iface..."

    # Try udhcpc (busybox) first, then dhcpcd
    if command -v udhcpc >/dev/null 2>&1; then
        log "Using udhcpc"
        if udhcpc -i "$iface" -t "$DHCP_TIMEOUT" -T 2 -n -q 2>>"$LOG_FILE"; then
            log "DHCP successful via udhcpc"
            return 0
        fi
    fi

    if command -v dhcpcd >/dev/null 2>&1; then
        log "Using dhcpcd"
        if dhcpcd "$iface" --timeout "$DHCP_TIMEOUT" --waitip 2>>"$LOG_FILE"; then
            log "DHCP successful via dhcpcd"
            return 0
        fi
    fi

    error "DHCP failed on $iface"
    return 1
}

# Test connectivity
test_connectivity() {
    log "Testing connectivity to $PING_TARGET..."

    if ping -c 1 -W 2 "$PING_TARGET" >/dev/null 2>&1; then
        log "Connectivity test passed"
        return 0
    fi

    warn "Connectivity test failed"
    return 1
}

# Get IP address
get_ip_address() {
    local iface="$1"
    ip -4 addr show "$iface" 2>/dev/null | grep -o 'inet [0-9.]*' | cut -d' ' -f2
}

# Main function
main() {
    touch "$LOG_FILE"
    log "Starting wired network initialization"

    # Find wired interfaces
    local interfaces
    interfaces=$(find_wired_interfaces)

    if [ -z "$interfaces" ]; then
        error "No wired network interfaces found"
        return 1
    fi

    log "Found wired interfaces: $interfaces"

    # Try each interface
    for iface in $interfaces; do
        log "Trying interface: $iface"

        # Retry loop
        local attempt=1
        while [ $attempt -le $MAX_RETRIES ]; do
            log "Attempt $attempt of $MAX_RETRIES"

            if bring_up_interface "$iface"; then
                if request_dhcp "$iface"; then
                    local ip
                    ip=$(get_ip_address "$iface")
                    if [ -n "$ip" ]; then
                        log "SUCCESS: Interface $iface has IP $ip"

                        # Test connectivity
                        if test_connectivity; then
                            log "Network is fully operational"
                            echo "up" > /tmp/network.status
                            echo "$iface" > /tmp/network.interface
                            echo "$ip" > /tmp/network.ip
                            return 0
                        fi
                    fi
                fi
            fi

            attempt=$((attempt + 1))
            if [ $attempt -le $MAX_RETRIES ]; then
                log "Retrying in ${RETRY_DELAY}s..."
                sleep $RETRY_DELAY
            fi
        done

        warn "Failed on interface $iface after $MAX_RETRIES attempts"
    done

    error "All wired interfaces failed"
    echo "down" > /tmp/network.status
    return 1
}

# Run main
main "$@"
