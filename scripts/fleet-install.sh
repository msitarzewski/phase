#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# Install a verified LUCID GitHub Release across every configured machine in a
# private, gitignored `.phase-fleet.local.env` file — eliminating manual
# copy/pasting while keeping all hostnames, SSH aliases, and IPs out of git.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
CONFIG_FILE="${PHASE_FLEET_ENV:-$REPO_ROOT/.phase-fleet.local.env}"
TAG_OVERRIDE=""
TARGETS="all"

usage() {
    cat <<'EOF'
Install a verified LUCID release across your private fleet.

Usage:
  scripts/fleet-install.sh [--config PATH] [--tag TAG] [--targets LIST]

Options:
  --config PATH     Private env file (default: .phase-fleet.local.env)
  --tag TAG         Override PHASE_RELEASE_TAG (e.g. v0.2.0-rc1 or latest)
  --targets LIST    Comma-separated subset of:
                    cloud-relay,ubuntu-desktop,ubuntu-laptop,mac-m1-max,local-app
                    or 'all' (default: all configured hosts)
  -h, --help        Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --config) CONFIG_FILE="$2"; shift 2 ;;
        --tag) TAG_OVERRIDE="$2"; shift 2 ;;
        --targets) TARGETS="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
done

if [[ ! -f "$CONFIG_FILE" ]]; then
    echo "ERROR: private fleet config not found at $CONFIG_FILE" >&2
    echo "Copy .phase-fleet.env.example to .phase-fleet.local.env and edit locally." >&2
    exit 1
fi

# shellcheck source=/dev/null
source "$CONFIG_FILE"

REPO="${PHASE_RELEASE_REPO:-msitarzewski/phase}"
TAG="${TAG_OVERRIDE:-${PHASE_RELEASE_TAG:-latest}}"
INSTALLER="$REPO_ROOT/scripts/install-lucid.sh"

should_run() {
    local name="$1"
    [[ "$TARGETS" == "all" ]] && return 0
    [[ ",${TARGETS}," == *",${name},"* ]]
}

install_remote() {
    local label="$1"
    local ssh_target="$2"
    shift 2
    if [[ -z "$ssh_target" ]]; then
        echo "SKIP: ${label} (SSH target unset in $(basename "$CONFIG_FILE"))"
        return 0
    fi
    echo "==> Installing LUCID (${TAG}) on ${label}..."
    ssh -o BatchMode=yes "$ssh_target" "bash -s -- --repo $(printf '%q' "$REPO") --tag $(printf '%q' "$TAG") $*" < "$INSTALLER"
}

if should_run "cloud-relay"; then
    install_remote "Cloud Relay" "${PHASE_CLOUD_RELAY_SSH:-}" "--install-systemd"
fi

if should_run "ubuntu-desktop"; then
    install_remote "Ubuntu Desktop (5060ti 16GB, 32GB RAM)" "${PHASE_UBUNTU_DESKTOP_SSH:-}"
fi

if should_run "ubuntu-laptop"; then
    install_remote "Ubuntu Laptop (2012 Retina MacBook Pro, 16GB RAM)" "${PHASE_UBUNTU_LAPTOP_SSH:-}"
fi

if should_run "mac-m1-max"; then
    install_remote "Mac M1 Max (32GB RAM)" "${PHASE_MAC_M1_MAX_SSH:-}"
fi

if should_run "local-app" && [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
    echo "==> Installing local CLI + LUCID.app (${TAG}) on Mac M5 Max (128GB RAM)..."
    bash "$INSTALLER" --repo "$REPO" --tag "$TAG"
    bash "$INSTALLER" --repo "$REPO" --tag "$TAG" --app
fi

echo "Fleet install complete."
