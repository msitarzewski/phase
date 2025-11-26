#!/bin/sh
#
# Phase Boot - Plasm Daemon Initialization
# Initializes and starts the Plasm distributed compute daemon
#

set -e

LOG_FILE="/tmp/plasm-init.log"
MAX_WAIT=30  # Maximum seconds to wait for daemon ready

# Logging
log() {
    echo "[PLASM-INIT] $1"
    echo "$(date '+%H:%M:%S') [PLASM-INIT] $1" >> "$LOG_FILE"
}

warn() {
    echo "[PLASM-INIT] WARNING: $1"
    echo "$(date '+%H:%M:%S') [PLASM-INIT] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[PLASM-INIT] ERROR: $1"
    echo "$(date '+%H:%M:%S') [PLASM-INIT] ERROR: $1" >> "$LOG_FILE"
}

# Find plasmd binary
find_plasmd() {
    if [ -x "/bin/plasmd" ]; then
        echo "/bin/plasmd"
        return 0
    elif [ -x "/usr/bin/plasmd" ]; then
        echo "/usr/bin/plasmd"
        return 0
    elif [ -x "/usr/local/bin/plasmd" ]; then
        echo "/usr/local/bin/plasmd"
        return 0
    else
        return 1
    fi
}

# Check if plasmd is already running
is_plasmd_running() {
    pgrep -x plasmd > /dev/null 2>&1
}

# Wait for daemon to be ready
wait_for_daemon() {
    local wait_time=0

    log "Waiting for daemon to be ready (max ${MAX_WAIT}s)..."

    while [ $wait_time -lt $MAX_WAIT ]; do
        if is_plasmd_running; then
            log "Daemon is running"
            # Additional check: see if we can reach it (if network tools available)
            if command -v curl > /dev/null 2>&1; then
                if curl -s -f http://localhost:4001/health > /dev/null 2>&1; then
                    log "Daemon health check passed"
                    return 0
                fi
            else
                # No curl, just verify process is running
                sleep 1
                if is_plasmd_running; then
                    log "Daemon process verified"
                    return 0
                fi
            fi
        fi

        sleep 1
        wait_time=$((wait_time + 1))
    done

    warn "Daemon readiness check timed out after ${MAX_WAIT}s"
    return 1
}

# Initialize Plasm daemon
init_plasm() {
    log "Initializing Plasm daemon"

    # Check if already running
    if is_plasmd_running; then
        warn "Daemon already running"
        return 0
    fi

    # Find plasmd binary
    local plasmd_bin
    if ! plasmd_bin=$(find_plasmd); then
        error "plasmd binary not found in /bin, /usr/bin, or /usr/local/bin"
        return 1
    fi

    log "Found plasmd at: $plasmd_bin"

    # Create config directory
    if [ ! -d "/etc/plasm" ]; then
        log "Creating /etc/plasm directory"
        mkdir -p /etc/plasm
    fi

    # Create data directory
    if [ ! -d "/var/lib/plasm" ]; then
        log "Creating /var/lib/plasm directory"
        mkdir -p /var/lib/plasm
    fi

    # Verify config exists
    if [ ! -f "/etc/plasm/config.json" ]; then
        warn "Config file not found at /etc/plasm/config.json"
        warn "Daemon will use default configuration"
    fi

    # Start daemon in background
    log "Starting plasmd daemon..."

    # Start with config file if available
    if [ -f "/etc/plasm/config.json" ]; then
        "$plasmd_bin" start --config /etc/plasm/config.json >> "$LOG_FILE" 2>&1 &
    else
        "$plasmd_bin" start >> "$LOG_FILE" 2>&1 &
    fi

    local daemon_pid=$!
    log "Daemon started with PID: $daemon_pid"

    # Wait for daemon to be ready
    if wait_for_daemon; then
        log "Plasm daemon initialized successfully"
        echo "ready" > /tmp/plasm.status
        echo "$daemon_pid" > /tmp/plasm.pid
        return 0
    else
        error "Daemon failed to become ready"
        return 1
    fi
}

# Show daemon status
show_status() {
    echo ""
    echo "=== Plasm Daemon Status ==="

    if [ -f /tmp/plasm.status ] && [ "$(cat /tmp/plasm.status)" = "ready" ]; then
        local pid
        pid=$(cat /tmp/plasm.pid 2>/dev/null || echo "unknown")
        echo "Status: READY"
        echo "PID:    $pid"

        # Show listening address if available
        if [ -f /etc/plasm/config.json ]; then
            echo "Config: /etc/plasm/config.json"
        else
            echo "Config: (using defaults)"
        fi
    else
        echo "Status: NOT READY"
    fi

    echo "==========================="
    echo ""
}

# Main
main() {
    touch "$LOG_FILE"

    log "Phase Boot - Plasm Daemon Initialization"

    if init_plasm; then
        show_status
        return 0
    else
        show_status
        warn "Continuing without Plasm daemon (distributed compute unavailable)"
        return 1
    fi
}

# Run main
main "$@"
