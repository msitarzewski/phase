#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Stage the compiled `lucidd` binary at the exact Tauri externalBin sidecar path
# (`src-tauri/binaries/lucidd-<target-triple>`).
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$APP_DIR/../.." && pwd)"
DAEMON="${1:-$REPO_ROOT/target/release/lucidd}"
TRIPLE="$(rustc -vV | awk '/^host:/ { print $2 }')"

if [[ ! -f "$DAEMON" || ! -x "$DAEMON" ]]; then
    echo "ERROR: lucidd binary not found or not executable at $DAEMON" >&2
    exit 1
fi

DEST_DIR="$APP_DIR/src-tauri/binaries"
mkdir -p "$DEST_DIR"
DEST="$DEST_DIR/lucidd-${TRIPLE}"
cp "$DAEMON" "$DEST"
chmod 0755 "$DEST"
echo "Staged sidecar: $DEST"
