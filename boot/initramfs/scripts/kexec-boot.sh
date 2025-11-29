#!/bin/sh
#
# Phase Boot - kexec Boot Script
# Loads and executes new kernel via kexec
#

set -e

SCRIPTS_DIR="$(dirname "$0")"
LOG_FILE="/tmp/phase-boot.log"

# Logging
log() {
    echo "[KEXEC] $1"
    echo "$(date '+%H:%M:%S') [KEXEC] $1" >> "$LOG_FILE"
}

warn() {
    echo "[KEXEC] WARNING: $1"
    echo "$(date '+%H:%M:%S') [KEXEC] WARNING: $1" >> "$LOG_FILE"
}

error() {
    echo "[KEXEC] ERROR: $1"
    echo "$(date '+%H:%M:%S') [KEXEC] ERROR: $1" >> "$LOG_FILE"
}

# Parse command line arguments
parse_args() {
    KERNEL_PATH=""
    INITRAMFS_PATH=""
    CMDLINE_EXTRA=""
    DTB_PATH=""

    while [ $# -gt 0 ]; do
        case "$1" in
            --kernel)
                KERNEL_PATH="$2"
                shift 2
                ;;
            --initramfs)
                INITRAMFS_PATH="$2"
                shift 2
                ;;
            --cmdline)
                CMDLINE_EXTRA="$2"
                shift 2
                ;;
            --dtb)
                DTB_PATH="$2"
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
    if [ -z "$KERNEL_PATH" ]; then
        error "Missing required argument: --kernel"
        usage
        exit 1
    fi

    if [ -z "$INITRAMFS_PATH" ]; then
        error "Missing required argument: --initramfs"
        usage
        exit 1
    fi
}

# Usage information
usage() {
    cat <<EOF
Usage: $0 --kernel PATH --initramfs PATH [OPTIONS]

Required:
  --kernel PATH       Path to kernel image
  --initramfs PATH    Path to initramfs image

Optional:
  --cmdline "..."     Additional kernel command line parameters
  --dtb PATH          Path to device tree blob (ARM/ARM64)

Examples:
  $0 --kernel /boot/vmlinuz --initramfs /boot/initramfs.img
  $0 --kernel /boot/vmlinuz --initramfs /boot/initramfs.img --cmdline "debug"
  $0 --kernel /boot/vmlinuz --initramfs /boot/initramfs.img --dtb /boot/bcm2711.dtb

EOF
}

# Validate file exists and is readable
validate_file() {
    local file="$1"
    local desc="$2"

    if [ ! -f "$file" ]; then
        error "$desc not found: $file"
        return 1
    fi

    if [ ! -r "$file" ]; then
        error "$desc not readable: $file"
        return 1
    fi

    log "$desc validated: $file"
    return 0
}

# Parse phase.mode from current kernel cmdline
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

# Extract relevant parameters from current cmdline
get_current_cmdline_params() {
    local cmdline
    cmdline=$(cat /proc/cmdline 2>/dev/null || echo "")

    local params=""

    for param in $cmdline; do
        case "$param" in
            phase.mode=*|phase.channel=*|phase.cache=*|phase.nowrite=*)
                params="$params $param"
                ;;
            console=*|root=*|earlyprintk=*|video=*)
                params="$params $param"
                ;;
        esac
    done

    echo "$params"
}

# Build complete kernel command line
build_cmdline() {
    local current_params
    current_params=$(get_current_cmdline_params)

    local phase_mode
    phase_mode=$(get_phase_mode)

    # Start with current phase parameters
    local cmdline="$current_params"

    # Add any extra parameters
    if [ -n "$CMDLINE_EXTRA" ]; then
        cmdline="$cmdline $CMDLINE_EXTRA"
    fi

    # Ensure we have at least basic required params
    if ! echo "$cmdline" | grep -q "console="; then
        cmdline="$cmdline console=tty0"
    fi

    # Remove leading/trailing spaces
    cmdline=$(echo "$cmdline" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')

    echo "$cmdline"
}

# Load kernel with kexec
load_kernel() {
    local kernel="$1"
    local initramfs="$2"
    local cmdline="$3"
    local dtb="$4"

    log "Loading kernel via kexec..."
    log "  Kernel:    $kernel"
    log "  Initramfs: $initramfs"
    log "  Cmdline:   $cmdline"
    [ -n "$dtb" ] && log "  DTB:       $dtb"

    # Build kexec command
    local kexec_cmd="kexec -l \"$kernel\" --initrd=\"$initramfs\" --command-line=\"$cmdline\""

    # Add DTB if provided
    if [ -n "$dtb" ]; then
        kexec_cmd="$kexec_cmd --dtb=\"$dtb\""
    fi

    # Execute kexec load
    log "Executing: $kexec_cmd"

    if [ -n "$dtb" ]; then
        if ! kexec -l "$kernel" --initrd="$initramfs" --command-line="$cmdline" --dtb="$dtb" 2>> "$LOG_FILE"; then
            error "Failed to load kernel with kexec"
            return 1
        fi
    else
        if ! kexec -l "$kernel" --initrd="$initramfs" --command-line="$cmdline" 2>> "$LOG_FILE"; then
            error "Failed to load kernel with kexec"
            return 1
        fi
    fi

    log "Kernel loaded successfully"
    return 0
}

# Execute kexec
execute_kexec() {
    log "Executing kexec to boot new kernel..."
    log "This will replace the current kernel - no return!"

    # Give user a moment to see the message
    sleep 1

    # Execute kexec
    if ! kexec -e 2>> "$LOG_FILE"; then
        error "Failed to execute kexec"
        error "System may be in inconsistent state"
        return 1
    fi

    # Should never reach here
    error "kexec -e returned unexpectedly"
    return 1
}

# Main
main() {
    # Ensure log file exists
    touch "$LOG_FILE"

    log "=== Phase Boot kexec Script ==="

    # Parse arguments
    parse_args "$@"

    log "Validating kernel and initramfs files..."

    # Validate kernel
    if ! validate_file "$KERNEL_PATH" "Kernel"; then
        error "Kernel validation failed"
        exit 1
    fi

    # Validate initramfs
    if ! validate_file "$INITRAMFS_PATH" "Initramfs"; then
        error "Initramfs validation failed"
        exit 1
    fi

    # Validate DTB if provided
    if [ -n "$DTB_PATH" ]; then
        if ! validate_file "$DTB_PATH" "Device Tree Blob"; then
            error "DTB validation failed"
            exit 1
        fi
    fi

    # Build kernel command line
    log "Building kernel command line..."
    FINAL_CMDLINE=$(build_cmdline)
    log "Final cmdline: $FINAL_CMDLINE"

    # Check if kexec is available
    if ! command -v kexec >/dev/null 2>&1; then
        error "kexec command not found"
        error "Please ensure kexec-tools is installed"
        exit 1
    fi

    # Load kernel
    if ! load_kernel "$KERNEL_PATH" "$INITRAMFS_PATH" "$FINAL_CMDLINE" "$DTB_PATH"; then
        error "Failed to load kernel"
        exit 1
    fi

    # Execute kexec
    log "Ready to execute kexec"
    execute_kexec

    # Should never reach here
    error "FATAL: kexec execution failed"
    exit 1
}

# Run main
main "$@"
