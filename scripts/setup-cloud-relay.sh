#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# One-command operator setup for a dedicated LUCID Cloud Relay (Linux x86_64/ARM64
# with a direct static public IPv4).
#
# What this script does:
#   1. Detects or validates the host's public IPv4 (rejecting loopback, RFC1918,
#      and Tailscale CGNAT 100.64.0.0/10 addresses).
#   2. Delegates binary download, SHA-256 verification, systemd user unit
#      install, and ~/.config/lucidd/infrastructure.env creation to
#      `scripts/install-lucid.sh --setup-relay`.
#   3. Opens inbound TCP 4001 ONLY in ufw when ufw is active (never touches
#      ports 80/443, SSH, Caddy, or Tailscale).
#   4. Enables user linger + starts `lucidd-relay.service`.
#   5. Extracts the relay's PeerId and prints the exact DNS TXT record value
#      for `bootstrap.phasebased.net`.

set -euo pipefail

REPO="${PHASE_RELEASE_REPO:-msitarzewski/phase}"
TAG="${PHASE_RELEASE_TAG:-v0.2.0-rc3}"
PUBLIC_IP=""
SKIP_FIREWALL=0
PREFIX="${HOME}/.local"

usage() {
    cat <<'EOF'
Set up a LUCID Cloud Relay on a Linux host with a static public IPv4.

Usage:
  scripts/setup-cloud-relay.sh [OPTIONS]

Options:
  --ip IPV4             Static public IPv4 address (auto-detected if omitted)
  --tag TAG             GitHub Release tag to install (default: v0.2.0-rc3)
  --repo OWNER/REPO     GitHub repository (default: msitarzewski/phase)
  --prefix DIR          Install prefix for bin/lucidd (default: ~/.local)
  --no-firewall         Skip adding the `ufw allow 4001/tcp` rule
  -h, --help            Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ip) PUBLIC_IP="$2"; shift 2 ;;
        --tag) TAG="$2"; shift 2 ;;
        --repo) REPO="$2"; shift 2 ;;
        --prefix) PREFIX="$2"; shift 2 ;;
        --no-firewall) SKIP_FIREWALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
done

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "ERROR: Cloud Relay setup must run on Linux." >&2
    exit 1
fi
if [[ "$(id -u)" -eq 0 ]]; then
    echo "ERROR: Run this script as your normal operator user (not root). It will use sudo only for ufw/loginctl." >&2
    exit 1
fi

is_public_ipv4() {
    local ip="$1"
    [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || return 1
    local o1 o2 o3 o4
    IFS='.' read -r o1 o2 o3 o4 <<< "$ip"
    for oct in "$o1" "$o2" "$o3" "$o4"; do
        (( oct >= 0 && oct <= 255 )) || return 1
    done
    # Reject loopback (127/8), RFC1918 (10/8, 172.16/12, 192.168/16),
    # link-local (169.254/16), CGNAT/Tailscale (100.64/10), and 0/8.
    (( o1 == 0 || o1 == 10 || o1 == 127 )) && return 1
    (( o1 == 169 && o2 == 254 )) && return 1
    (( o1 == 172 && o2 >= 16 && o2 <= 31 )) && return 1
    (( o1 == 192 && o2 == 168 )) && return 1
    (( o1 == 100 && o2 >= 64 && o2 <= 127 )) && return 1
    return 0
}

if [[ -z "$PUBLIC_IP" ]]; then
    # Prefer the primary non-Tailscale interface address, then fall back to HTTPS IP lookup.
    if command -v ip >/dev/null 2>&1; then
        CANDIDATE="$(ip -4 -o addr show scope global 2>/dev/null | awk '!/tailscale|docker|br-|veth/ { split($4, a, "/"); print a[1] }' | head -n 1 || true)"
        if [[ -n "$CANDIDATE" ]] && is_public_ipv4 "$CANDIDATE"; then
            PUBLIC_IP="$CANDIDATE"
        fi
    fi
    if [[ -z "$PUBLIC_IP" ]]; then
        CANDIDATE="$(curl -4 -fsSL --max-time 5 https://ifconfig.me/ip 2>/dev/null | tr -d '[:space:]' || true)"
        if [[ -n "$CANDIDATE" ]] && is_public_ipv4 "$CANDIDATE"; then
            PUBLIC_IP="$CANDIDATE"
        fi
    fi
fi

if [[ -z "$PUBLIC_IP" ]] || ! is_public_ipv4 "$PUBLIC_IP"; then
    echo "ERROR: Could not confirm a valid public IPv4 address. Pass it explicitly with --ip <PUBLIC_IPV4>." >&2
    exit 1
fi

echo "==> Setting up LUCID Cloud Relay (${TAG}) on public IPv4 ${PUBLIC_IP}:4001"

# Reuse install-lucid.sh (local checkout if present, otherwise fetch pinned tag).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"
if [[ -n "$SCRIPT_DIR" && -f "$SCRIPT_DIR/install-lucid.sh" ]]; then
    bash "$SCRIPT_DIR/install-lucid.sh" \
        --repo "$REPO" --tag "$TAG" --prefix "$PREFIX" --setup-relay "$PUBLIC_IP"
else
    TMP_INSTALLER="$(mktemp)"
    trap 'rm -f "$TMP_INSTALLER"' EXIT
    curl -fsSL --proto '=https' --tlsv1.2 \
        "https://raw.githubusercontent.com/${REPO}/${TAG}/scripts/install-lucid.sh" \
        -o "$TMP_INSTALLER"
    bash "$TMP_INSTALLER" \
        --repo "$REPO" --tag "$TAG" --prefix "$PREFIX" --setup-relay "$PUBLIC_IP"
fi

if [[ "$SKIP_FIREWALL" -eq 0 ]] && command -v ufw >/dev/null 2>&1; then
    if sudo ufw status 2>/dev/null | grep -qi '^Status: active'; then
        echo "==> Allowing inbound TCP 4001/tcp in ufw (leaving 80/443, SSH, Caddy, Tailscale untouched)..."
        sudo ufw allow 4001/tcp comment "LUCID libp2p relay"
    else
        echo "Note: ufw is installed but inactive; no ufw rule changed."
    fi
fi

echo "==> Enabling user linger and starting lucidd-relay.service..."
sudo loginctl enable-linger "$USER"
systemctl --user daemon-reload
systemctl --user enable --now lucidd-relay.service
systemctl --user restart lucidd-relay.service

echo "==> Waiting for relay PeerId..."
PEER_ID=""
for _ in $(seq 1 15); do
    PEER_ID="$(journalctl --user -u lucidd-relay.service -n 80 --no-pager 2>/dev/null \
        | grep -oE '12D3KooW[1-9A-HJ-NP-Za-km-z]{40,60}' | tail -n 1 || true)"
    if [[ -n "$PEER_ID" ]]; then
        break
    fi
    sleep 1
done

echo ""
systemctl --user --no-pager status lucidd-relay.service | head -n 12 || true
echo ""

if [[ -n "$PEER_ID" ]]; then
    MULTIADDR="/ip4/${PUBLIC_IP}/tcp/4001/p2p/${PEER_ID}"
    echo "================================================================"
    echo "Cloud Relay is UP!"
    echo ""
    echo "Relay multiaddr:"
    echo "  ${MULTIADDR}"
    echo ""
    echo "Add this single DNS TXT record for zero-config auto-discovery:"
    echo "  Host:  bootstrap.phasebased.net"
    echo "  Type:  TXT"
    echo "  Value: ${MULTIADDR}"
    echo "================================================================"
else
    echo "Cloud Relay started, but PeerId was not spotted in logs yet. Inspect with:"
    echo "  journalctl --user -u lucidd-relay.service -n 50 --no-pager"
fi
