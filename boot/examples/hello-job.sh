#!/bin/bash
#
# Phase Boot - Hello World WASM Job Demo
# Demonstrates executing a WASM module via Plasm daemon
#

set -e

# Colors for output (if terminal supports it)
if [ -t 1 ]; then
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    RED='\033[0;31m'
    NC='\033[0m' # No Color
else
    GREEN=''
    YELLOW=''
    RED=''
    NC=''
fi

echo ""
echo "======================================"
echo "  Phase Boot - WASM Job Demo"
echo "======================================"
echo ""

# Check if plasmd is running
check_daemon() {
    echo -n "Checking Plasm daemon... "

    if pgrep -x plasmd > /dev/null 2>&1; then
        echo -e "${GREEN}RUNNING${NC}"
        return 0
    else
        echo -e "${RED}NOT RUNNING${NC}"
        echo ""
        echo "ERROR: plasmd is not running"
        echo ""
        echo "To start the daemon:"
        echo "  sudo systemctl start plasmd"
        echo "  OR"
        echo "  sudo /usr/bin/plasmd start --config /etc/plasm/config.json"
        echo ""
        return 1
    fi
}

# Find plasmd binary
find_plasmd() {
    if [ -x "/bin/plasmd" ]; then
        echo "/bin/plasmd"
    elif [ -x "/usr/bin/plasmd" ]; then
        echo "/usr/bin/plasmd"
    elif [ -x "/usr/local/bin/plasmd" ]; then
        echo "/usr/local/bin/plasmd"
    else
        return 1
    fi
}

# Check for example WASM file
check_wasm() {
    # Look for example WASM files in common locations
    local wasm_locations=(
        "/home/user/phase/boot/examples/hello.wasm"
        "/usr/share/plasm/examples/hello.wasm"
        "/var/lib/plasm/examples/hello.wasm"
        "./hello.wasm"
    )

    for wasm_path in "${wasm_locations[@]}"; do
        if [ -f "$wasm_path" ]; then
            echo "$wasm_path"
            return 0
        fi
    done

    return 1
}

# Execute the WASM job
execute_job() {
    local plasmd_bin="$1"
    local wasm_file="$2"

    echo ""
    echo "Executing WASM module..."
    echo "  Binary:  $plasmd_bin"
    echo "  WASM:    $wasm_file"
    echo "  Command: execute-job"
    echo ""

    # Check if execute-job command exists
    if ! "$plasmd_bin" --help 2>&1 | grep -q "execute-job"; then
        echo -e "${YELLOW}NOTE:${NC} execute-job command not available in this version"
        echo ""
        echo "Using 'run' command instead (local execution):"
        echo ""

        if "$plasmd_bin" run "$wasm_file" "World"; then
            echo ""
            echo -e "${GREEN}SUCCESS:${NC} WASM module executed successfully"
            return 0
        else
            echo ""
            echo -e "${RED}FAILED:${NC} WASM execution failed"
            return 1
        fi
    else
        # Use execute-job command (M3+ feature)
        echo "Job execution flow:"
        echo "  1. Load WASM module"
        echo "  2. Create job manifest"
        echo "  3. Sign with local keypair"
        echo "  4. Execute in isolated runtime"
        echo "  5. Generate signed receipt"
        echo ""

        if "$plasmd_bin" execute-job "$wasm_file" "World"; then
            echo ""
            echo -e "${GREEN}SUCCESS:${NC} Job executed with signed receipt"
            return 0
        else
            echo ""
            echo -e "${RED}FAILED:${NC} Job execution failed"
            return 1
        fi
    fi
}

# Show documentation
show_docs() {
    echo ""
    echo "======================================"
    echo "  Understanding the WASM Flow"
    echo "======================================"
    echo ""
    echo "1. WASM Module Compilation:"
    echo "   - Source code (Rust/C/AssemblyScript) → WASM bytecode"
    echo "   - Compiled .wasm files are portable and sandboxed"
    echo ""
    echo "2. Job Submission (M3+):"
    echo "   - Client creates JobManifest with requirements"
    echo "   - Manifest includes WASM hash, args, resource limits"
    echo "   - Client signs manifest with ed25519 keypair"
    echo ""
    echo "3. Execution:"
    echo "   - Plasm daemon validates signature"
    echo "   - Loads WASM module into isolated runtime"
    echo "   - Enforces resource limits (memory, CPU, timeout)"
    echo "   - Executes with provided arguments"
    echo ""
    echo "4. Receipt Generation:"
    echo "   - Daemon captures stdout, stderr, exit code"
    echo "   - Creates ExecutionReceipt with results"
    echo "   - Signs receipt with daemon's keypair"
    echo "   - Returns to client for verification"
    echo ""
    echo "5. Verification:"
    echo "   - Client verifies daemon signature"
    echo "   - Confirms job manifest hash matches"
    echo "   - Can prove execution to third parties"
    echo ""
    echo "For more information:"
    echo "  https://github.com/yourusername/phase"
    echo ""
}

# Main execution
main() {
    # Check daemon
    if ! check_daemon; then
        exit 1
    fi

    # Find plasmd binary
    local plasmd_bin
    if ! plasmd_bin=$(find_plasmd); then
        echo -e "${RED}ERROR:${NC} plasmd binary not found"
        exit 1
    fi

    # Check for WASM file
    local wasm_file
    if wasm_file=$(check_wasm); then
        echo -e "Found WASM file: ${GREEN}$wasm_file${NC}"
    else
        echo -e "${YELLOW}WARNING:${NC} No example WASM file found"
        echo ""
        echo "Expected locations:"
        echo "  /home/user/phase/boot/examples/hello.wasm"
        echo "  /usr/share/plasm/examples/hello.wasm"
        echo ""
        echo "To create a hello world WASM module:"
        echo "  See: /home/user/phase/boot/docs/wasm-examples.md"
        echo ""
        show_docs
        exit 1
    fi

    # Execute the job
    if execute_job "$plasmd_bin" "$wasm_file"; then
        show_docs
        exit 0
    else
        echo ""
        echo "Check logs:"
        echo "  journalctl -u plasmd"
        echo "  OR"
        echo "  /tmp/plasm-init.log"
        echo ""
        exit 1
    fi
}

# Run main
main "$@"
