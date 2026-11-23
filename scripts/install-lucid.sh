#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# Verified installer for LUCID (`lucidd` CLI + optional systemd user unit on
# Linux or `LUCID.app` on macOS Apple Silicon). Downloads from GitHub Releases
# (or reads a local release directory via `--from-dir`) and verifies SHA-256
# checksums fail-closed before extracting or installing anything.

set -euo pipefail

REPO="${PHASE_RELEASE_REPO:-msitarzewski/phase}"
TAG="${PHASE_RELEASE_TAG:-latest}"
PREFIX="${HOME}/.local"
FROM_DIR=""
INSTALL_APP=0
INSTALL_SYSTEMD=0
SETUP_RELAY_IP=""

usage() {
    cat <<'EOF'
Install verified LUCID release artifacts (zero manual copy/paste).

Usage:
  scripts/install-lucid.sh [OPTIONS]

Options:
  --repo OWNER/REPO     GitHub repository (default: msitarzewski/phase)
  --tag TAG             Release tag (e.g. v0.2.0-rc2) or 'latest' (default: latest)
  --from-dir PATH       Install from a local directory containing the archive + SHA256SUMS.txt
  --prefix DIR          Install prefix for `bin/lucidd` (default: ~/.local)
  --app                 macOS Apple Silicon only: install LUCID.app to ~/Applications/LUCID.app
  --install-systemd     Linux only: install user systemd service + example env (never overwrites existing env)
  --setup-relay IP      Linux only: install systemd unit, write infrastructure.env with IP, and print next steps
  -h, --help            Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) REPO="$2"; shift 2 ;;
        --tag) TAG="$2"; shift 2 ;;
        --from-dir) FROM_DIR="$2"; shift 2 ;;
        --prefix) PREFIX="$2"; shift 2 ;;
        --app) INSTALL_APP=1; shift ;;
        --install-systemd) INSTALL_SYSTEMD=1; shift ;;
        --setup-relay) SETUP_RELAY_IP="$2"; INSTALL_SYSTEMD=1; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown argument: $1" >&2; usage >&2; exit 1 ;;
    esac
done

detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}:${arch}" in
        Linux:x86_64)   echo "x86_64-unknown-linux-gnu" ;;
        Linux:aarch64|Linux:arm64) echo "aarch64-unknown-linux-gnu" ;;
        Darwin:arm64)   echo "aarch64-apple-darwin" ;;
        *)
            echo "ERROR: unsupported platform ${os}/${arch}" >&2
            exit 1
            ;;
    esac
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        echo "ERROR: sha256sum or shasum is required" >&2
        exit 1
    fi
}

verify_checksum() {
    local sums_file="$1"
    local archive_path="$2"
    local archive_name expected actual
    archive_name="$(basename "$archive_path")"
    expected="$(awk -v name="$archive_name" '$2 == name || $2 == "*" name { print $1; exit }' "$sums_file")"
    if [[ -z "$expected" ]]; then
        echo "ERROR: no SHA-256 entry for ${archive_name} in $(basename "$sums_file")" >&2
        exit 1
    fi
    actual="$(sha256_file "$archive_path")"
    if [[ "$actual" != "$expected" ]]; then
        echo "ERROR: SHA-256 mismatch for ${archive_name}: expected ${expected}, got ${actual}" >&2
        exit 1
    fi
    echo "Verified SHA-256 (${archive_name}): ${actual}"
}

TARGET="$(detect_target)"
if [[ "$INSTALL_APP" -eq 1 && "$TARGET" != "aarch64-apple-darwin" ]]; then
    echo "ERROR: --app is only supported on macOS Apple Silicon" >&2
    exit 1
fi
if [[ "$INSTALL_SYSTEMD" -eq 1 && "$(uname -s)" != "Linux" ]]; then
    echo "ERROR: --install-systemd is only supported on Linux" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SUMS_FILE="$TMP_DIR/SHA256SUMS.txt"

if [[ -n "$FROM_DIR" ]]; then
    cp "$FROM_DIR/SHA256SUMS.txt" "$SUMS_FILE"
else
    if [[ "$TAG" == "latest" ]]; then
        BASE_URL="https://github.com/${REPO}/releases/latest/download"
    else
        BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"
    fi
    curl -fsSL --proto '=https' --tlsv1.2 "${BASE_URL}/SHA256SUMS.txt" -o "$SUMS_FILE"
fi

if [[ "$INSTALL_APP" -eq 1 ]]; then
    ARCHIVE_NAME="$(awk -v suffix="-${TARGET}.zip" '$2 ~ /^LUCID-/ && substr($2, length($2)-length(suffix)+1) == suffix { print $2; exit }' "$SUMS_FILE")"
else
    ARCHIVE_NAME="$(awk -v suffix="-${TARGET}.tar.gz" '$2 ~ /^lucidd-/ && substr($2, length($2)-length(suffix)+1) == suffix { print $2; exit }' "$SUMS_FILE")"
fi

if [[ -z "$ARCHIVE_NAME" ]]; then
    echo "ERROR: could not locate matching archive for ${TARGET} in SHA256SUMS.txt" >&2
    exit 1
fi

ARCHIVE_PATH="$TMP_DIR/$ARCHIVE_NAME"
if [[ -n "$FROM_DIR" ]]; then
    cp "$FROM_DIR/$ARCHIVE_NAME" "$ARCHIVE_PATH"
else
    echo "Downloading ${ARCHIVE_NAME} from ${REPO} (${TAG})..."
    curl -fsSL --proto '=https' --tlsv1.2 "${BASE_URL}/${ARCHIVE_NAME}" -o "$ARCHIVE_PATH"
fi

verify_checksum "$SUMS_FILE" "$ARCHIVE_PATH"

if [[ "$INSTALL_APP" -eq 1 ]]; then
    APP_STAGE="$TMP_DIR/app"
    mkdir -p "$APP_STAGE" "$HOME/Applications"
    ditto -x -k "$ARCHIVE_PATH" "$APP_STAGE"
    if [[ ! -d "$APP_STAGE/LUCID.app" ]]; then
        echo "ERROR: archive did not contain LUCID.app" >&2
        exit 1
    fi
    codesign --verify --deep --strict "$APP_STAGE/LUCID.app"
    rm -rf "$HOME/Applications/LUCID.app"
    mv "$APP_STAGE/LUCID.app" "$HOME/Applications/LUCID.app"
    echo "Installed: $HOME/Applications/LUCID.app"
    exit 0
fi

UNPACK="$TMP_DIR/unpack"
mkdir -p "$UNPACK" "$PREFIX/bin"
tar -C "$UNPACK" -xzf "$ARCHIVE_PATH"

BIN_SRC="$(find "$UNPACK" -type f -path '*/bin/lucidd' -print -quit)"
if [[ -z "$BIN_SRC" || ! -x "$BIN_SRC" ]]; then
    echo "ERROR: archive did not contain executable bin/lucidd" >&2
    exit 1
fi

install -m 0755 "$BIN_SRC" "$PREFIX/bin/lucidd"
"$PREFIX/bin/lucidd" --help >/dev/null
echo "Installed: $PREFIX/bin/lucidd"

if [[ "$INSTALL_SYSTEMD" -eq 1 ]]; then
    UNIT_SRC="$(find "$UNPACK" -type f -path '*/systemd/lucidd-relay.service' -print -quit)"
    ENV_SRC="$(find "$UNPACK" -type f -path '*/config/infrastructure.env.example' -print -quit)"
    mkdir -p "$HOME/.config/systemd/user" "$HOME/.config/lucidd"
    chmod 700 "$HOME/.config/lucidd"
    if [[ -n "$UNIT_SRC" ]]; then
        UNIT_DST="$HOME/.config/systemd/user/lucidd-relay.service"
        sed "s|ExecStart=%h/bin/lucidd|ExecStart=${PREFIX}/bin/lucidd|" "$UNIT_SRC" > "$UNIT_DST"
        chmod 0644 "$UNIT_DST"
        echo "Installed unit: $UNIT_DST (ExecStart=${PREFIX}/bin/lucidd)"
    fi
    ENV_DST="$HOME/.config/lucidd/infrastructure.env"
    if [[ -n "$SETUP_RELAY_IP" ]]; then
        printf '# Configured by install-lucid.sh --setup-relay\nLUCIDD_EXTERNAL_ADDRESS=/ip4/%s/tcp/4001\nLUCIDD_LIBP2P_PORT=4001\n' "$SETUP_RELAY_IP" > "$ENV_DST"
        chmod 0600 "$ENV_DST"
        echo "Wrote relay config: $ENV_DST"
        echo "Next: open TCP 4001, then run:"
        echo "  systemctl --user daemon-reload && systemctl --user enable --now lucidd-relay.service"
    elif [[ -n "$ENV_SRC" && ! -f "$ENV_DST" ]]; then
        install -m 0600 "$ENV_SRC" "$ENV_DST"
        echo "Seeded template: $ENV_DST (edit before enabling service)"
    fi
fi
