#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# One-command operator setup for a dedicated LUCID Cloud Relay (Linux x86_64/ARM64
# with a direct static public IPv4).
#
# Security & isolation:
#   - NEVER runs `lucidd` as root. If invoked as root (typical on fresh VPS
#     droplets), creates a locked-down unprivileged `lucid` service user
#     (`/usr/sbin/nologin`, zero sudo) and installs a sandboxed systemd unit
#     (`User=lucid`, `NoNewPrivileges=true`, `ProtectSystem=strict`,
#     `ProtectHome=read-only`, `MemoryDenyWriteExecute=true`).
#   - Verifies the downloaded release archive against SHA256SUMS.txt fail-closed.
#   - Opens ONLY inbound TCP 4001 in ufw (leaves 80/443, SSH, Caddy, and
#     Tailscale untouched).
#   - Rejects loopback, RFC1918, and Tailscale CGNAT (100.64.0.0/10) IPs.

set -euo pipefail

REPO="${PHASE_RELEASE_REPO:-msitarzewski/phase}"
TAG="${PHASE_RELEASE_TAG:-v0.2.0-rc4}"
PUBLIC_IP=""
SKIP_FIREWALL=0
PREFIX="${HOME}/.local"
SERVICE_USER="lucid"

usage() {
    cat <<'EOF'
Set up a LUCID Cloud Relay on a Linux host with a static public IPv4.

Usage:
  scripts/setup-cloud-relay.sh [OPTIONS]

Options:
  --ip IPV4             Static public IPv4 address (auto-detected if omitted)
  --tag TAG             GitHub Release tag to install (default: v0.2.0-rc4)
  --repo OWNER/REPO     GitHub repository (default: msitarzewski/phase)
  --prefix DIR          Install prefix for bin/lucidd (non-root mode, default: ~/.local)
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

# Locate or download install-lucid.sh once.
TMP_INSTALLER=""
cleanup() {
    if [[ -n "$TMP_INSTALLER" ]]; then
        rm -f "$TMP_INSTALLER"
    fi
}
trap cleanup EXIT

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"
if [[ -n "$SCRIPT_DIR" && -f "$SCRIPT_DIR/install-lucid.sh" ]]; then
    INSTALLER="$SCRIPT_DIR/install-lucid.sh"
else
    TMP_INSTALLER="$(mktemp)"
    curl -fsSL --proto '=https' --tlsv1.2 \
        "https://raw.githubusercontent.com/${REPO}/${TAG}/scripts/install-lucid.sh" \
        -o "$TMP_INSTALLER"
    chmod 0755 "$TMP_INSTALLER"
    INSTALLER="$TMP_INSTALLER"
fi

if [[ "$(id -u)" -eq 0 ]]; then
    # Root mode: create dedicated unprivileged service user `lucid` (never runs lucidd as root).
    if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
        echo "==> Creating unprivileged service account '${SERVICE_USER}' (no login shell, no sudo)..."
        useradd --system --create-home --home-dir "/home/${SERVICE_USER}" --shell /usr/sbin/nologin "$SERVICE_USER"
    fi
    SVC_HOME="$(getent passwd "$SERVICE_USER" | cut -d: -f6)"
    SVC_PREFIX="${SVC_HOME}/.local"
    chmod 0750 "$SVC_HOME"

    # Install binary as unprivileged user (SHA-256 verified fail-closed).
    # `cd` out of `/root` first so `find` inside the installer can restore cwd.
    su -s /bin/bash "$SERVICE_USER" -c \
        "cd $(printf '%q' "$SVC_HOME") && bash $(printf '%q' "$INSTALLER") --repo $(printf '%q' "$REPO") --tag $(printf '%q' "$TAG") --prefix $(printf '%q' "$SVC_PREFIX")"

    install -d -m 0700 -o "$SERVICE_USER" -g "$SERVICE_USER" "${SVC_HOME}/.config/lucidd" "${SVC_HOME}/.config/phase"
    ENV_FILE="${SVC_HOME}/.config/lucidd/infrastructure.env"
    cat > "$ENV_FILE" <<EOF
# Configured by setup-cloud-relay.sh
LUCIDD_EXTERNAL_ADDRESS=/ip4/${PUBLIC_IP}/tcp/4001
LUCIDD_LIBP2P_PORT=4001
EOF
    chown "${SERVICE_USER}:${SERVICE_USER}" "$ENV_FILE"
    chmod 0600 "$ENV_FILE"

    UNIT_FILE="/etc/systemd/system/lucidd-relay.service"
    cat > "$UNIT_FILE" <<EOF
[Unit]
Description=lucidd — bounded Phase relay/AutoNAT infrastructure node
Documentation=https://github.com/msitarzewski/phase
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=600
StartLimitBurst=10

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_USER}
UMask=0077
Environment=LUCIDD_LIBP2P_PORT=4001
Environment=LUCIDD_PORT=11434
Environment=LUCIDD_HOST=127.0.0.1
Environment=RUST_LOG=info,phase_net=info,lucidd=info
EnvironmentFile=${ENV_FILE}
ExecStart=${SVC_PREFIX}/bin/lucidd --mode infrastructure --libp2p-port \${LUCIDD_LIBP2P_PORT} --external-address \${LUCIDD_EXTERNAL_ADDRESS}
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=lucidd
LimitNOFILE=65536
TasksMax=512
MemoryMax=1G
CPUQuota=200%
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=${SVC_HOME}/.config/lucidd ${SVC_HOME}/.config/phase
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
RestrictSUIDSGID=true
RestrictRealtime=true
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
EOF
    chmod 0644 "$UNIT_FILE"

    if [[ "$SKIP_FIREWALL" -eq 0 ]] && command -v ufw >/dev/null 2>&1; then
        if ufw status 2>/dev/null | grep -qi '^Status: active'; then
            echo "==> Allowing inbound TCP 4001/tcp in ufw (leaving 80/443, SSH, Caddy, Tailscale untouched)..."
            ufw allow 4001/tcp comment "LUCID libp2p relay"
        fi
    fi

    echo "==> Enabling and starting sandboxed lucidd-relay.service (User=${SERVICE_USER})..."
    systemctl daemon-reload
    systemctl enable --now lucidd-relay.service
    systemctl restart lucidd-relay.service
    JOURNAL_CMD=(journalctl -u lucidd-relay.service -n 80 --no-pager)
    STATUS_CMD=(systemctl --no-pager status lucidd-relay.service)
else
    bash "$INSTALLER" --repo "$REPO" --tag "$TAG" --prefix "$PREFIX" --setup-relay "$PUBLIC_IP"

    if [[ "$SKIP_FIREWALL" -eq 0 ]] && command -v ufw >/dev/null 2>&1; then
        if sudo ufw status 2>/dev/null | grep -qi '^Status: active'; then
            echo "==> Allowing inbound TCP 4001/tcp in ufw (leaving 80/443, SSH, Caddy, Tailscale untouched)..."
            sudo ufw allow 4001/tcp comment "LUCID libp2p relay"
        fi
    fi

    echo "==> Enabling user linger and starting lucidd-relay.service..."
    sudo loginctl enable-linger "$USER"
    systemctl --user daemon-reload
    systemctl --user enable --now lucidd-relay.service
    systemctl --user restart lucidd-relay.service
    JOURNAL_CMD=(journalctl --user -u lucidd-relay.service -n 80 --no-pager)
    STATUS_CMD=(systemctl --user --no-pager status lucidd-relay.service)
fi

echo "==> Waiting for relay PeerId..."
PEER_ID=""
for _ in $(seq 1 15); do
    PEER_ID="$("${JOURNAL_CMD[@]}" 2>/dev/null \
        | grep -oE '12D3KooW[1-9A-HJ-NP-Za-km-z]{40,60}' | tail -n 1 || true)"
    if [[ -n "$PEER_ID" ]]; then
        break
    fi
    sleep 1
done

echo ""
"${STATUS_CMD[@]}" | head -n 12 || true
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
    echo "  ${JOURNAL_CMD[*]}"
fi
