#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build a local, ad-hoc-signed macOS application containing the actual lucidd daemon.
set -euo pipefail

APP_SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$APP_SOURCE/../.." && pwd)"
OUTPUT="$REPO_ROOT/target/lucid-macos"
DAEMON=""
SIGN=true

usage() {
    cat <<'EOF'
Usage: apps/lucid-macos/build-app.sh [--daemon /absolute/path/to/lucidd] [--output DIRECTORY] [--no-sign]

Without --daemon, build lucidd from this checkout using locked, offline Cargo.
The result is a local development app; Apple Developer signing/notarization is
required separately before public distribution. Never run this script as root.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --daemon) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; DAEMON="$2"; shift 2 ;;
        --output) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; OUTPUT="$2"; shift 2 ;;
        --no-sign) SIGN=false; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[[ "$(uname -s)" == Darwin ]] || { echo "ERROR: macOS is required." >&2; exit 1; }
[[ "$(id -u)" != 0 ]] || { echo "ERROR: Build and run LUCID as your normal user." >&2; exit 1; }
command -v xcrun >/dev/null
command -v python3 >/dev/null
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
export CLANG_MODULE_CACHE_PATH="$OUTPUT/module-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$OUTPUT/module-cache"

if [[ -z "$DAEMON" ]]; then
    (cd "$REPO_ROOT" && cargo build -p lucidd --release --locked --offline)
    DAEMON="$REPO_ROOT/target/release/lucidd"
fi
[[ "$DAEMON" = /* && -f "$DAEMON" && -x "$DAEMON" ]] || { echo "ERROR: --daemon must be an absolute executable file." >&2; exit 1; }
DAEMON_HELP="$("$DAEMON" --help)"
[[ "$DAEMON_HELP" == *--outbound-only* ]] || { echo "ERROR: The bundled daemon must support --outbound-only." >&2; exit 1; }
[[ "$DAEMON_HELP" == *--control-token-file* ]] || { echo "ERROR: The bundled daemon must support local control-token authentication." >&2; exit 1; }
ARCH="$(uname -m)"
DAEMON_ARCHS="$(xcrun lipo -archs "$DAEMON")"
[[ " $DAEMON_ARCHS " == *" $ARCH "* ]] || { echo "ERROR: The selected daemon must include the native $ARCH architecture." >&2; exit 1; }

SWIFT_OPTIONS=(--package-path "$APP_SOURCE" --scratch-path "$OUTPUT/swift-build" --cache-path "$OUTPUT/cache" --config-path "$OUTPUT/config" --security-path "$OUTPUT/security" --disable-sandbox -Xswiftc -module-cache-path -Xswiftc "$OUTPUT/module-cache" -Xswiftc -warnings-as-errors)
xcrun swift build "${SWIFT_OPTIONS[@]}" -c release
SWIFT_BIN="$(xcrun swift build "${SWIFT_OPTIONS[@]}" -c release --show-bin-path)"
STAGE="$(mktemp -d "$OUTPUT/.lucid-build.XXXXXX")"
trap 'rm -rf -- "$STAGE"' EXIT
APP="$STAGE/LUCID.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp "$SWIFT_BIN/Lucid" "$APP/Contents/MacOS/Lucid"
cp "$DAEMON" "$APP/Contents/Helpers/lucidd"
cp "$APP_SOURCE/Info.plist" "$APP/Contents/Info.plist"
cp "$REPO_ROOT/LICENSE" "$APP/Contents/Resources/Phase-LICENSE"
chmod 755 "$APP/Contents/MacOS/Lucid" "$APP/Contents/Helpers/lucidd"

# Render a simple native vector mark into the standard macOS icon sizes.
ICONSET="$STAGE/AppIcon.iconset"
mkdir -p "$ICONSET"
cat > "$STAGE/render-icon.swift" <<'SWIFT'
import AppKit
import Foundation
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0), let context = NSGraphicsContext(bitmapImageRep: bitmap) else { fatalError("Cannot create app icon context") }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        let side = CGFloat(pixels)
        let inset = side * 0.05
        let background = NSBezierPath(roundedRect: NSRect(x: inset, y: inset, width: side - inset * 2, height: side - inset * 2), xRadius: side * 0.22, yRadius: side * 0.22)
        NSGradient(starting: NSColor(red: 0.20, green: 0.23, blue: 0.66, alpha: 1), ending: NSColor(red: 0.13, green: 0.57, blue: 0.88, alpha: 1))?.draw(in: background, angle: 45)
        NSColor.white.setStroke()
        for ratio in [0.24, 0.42, 0.60] {
            let width = side * ratio
            let circle = NSBezierPath(ovalIn: NSRect(x: (side - width) / 2, y: (side - width) / 2, width: width, height: width))
            circle.lineWidth = max(1, side * 0.022)
            circle.stroke()
        }
        NSColor.white.setFill()
        NSBezierPath(ovalIn: NSRect(x: side * 0.465, y: side * 0.465, width: side * 0.07, height: side * 0.07)).fill()
        NSGraphicsContext.restoreGraphicsState()
        guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("Cannot render app icon") }
        let suffix = scale == 2 ? "@2x" : ""
        try png.write(to: directory.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
    }
}
SWIFT
xcrun swift -module-cache-path "$OUTPUT/module-cache" "$STAGE/render-icon.swift" "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

if [[ "$SIGN" == true ]]; then
    codesign --force --sign - --timestamp=none --identifier net.phasebased.lucid.daemon "$APP/Contents/Helpers/lucidd"
fi

python3 - "$APP" "$REPO_ROOT" <<'PY'
from pathlib import Path
import hashlib, json, platform, plistlib, re, subprocess, sys
app, repo = map(Path, sys.argv[1:])
binary = app / "Contents/Helpers/lucidd"
digest = hashlib.sha256(binary.read_bytes()).hexdigest()
manifest = (repo / "crates/lucidd/Cargo.toml").read_text()
version = re.search(r'^version = "([^"]+)"', manifest, re.M).group(1)
path = app / "Contents/Info.plist"
with path.open('rb') as f:
    info = plistlib.load(f)
info['CFBundleShortVersionString'] = version
info['LucidDaemonSHA256'] = digest
with path.open('wb') as f:
    plistlib.dump(info, f)
commit = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
dirty = bool(subprocess.check_output(['git', '-C', str(repo), 'status', '--porcelain'], text=True).strip())
(app / 'Contents/Resources/build-info.json').write_text(json.dumps({
    'version': version, 'source_commit': commit, 'source_dirty': dirty,
    'daemon_sha256': digest, 'architecture': platform.machine(), 'distribution': 'local-development'
}, indent=2) + '\n')
PY

if [[ "$SIGN" == true ]]; then
    codesign --force --sign - --timestamp=none "$APP"
    codesign --verify --deep --strict "$APP"
fi
DESTINATION="$OUTPUT/LUCID.app"
if [[ -e "$DESTINATION" ]]; then
    [[ -f "$DESTINATION/Contents/Info.plist" ]] || { echo "ERROR: Refusing to replace an unrecognized destination." >&2; exit 1; }
    IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$DESTINATION/Contents/Info.plist")"
    [[ "$IDENTIFIER" == net.phasebased.lucid ]] || { echo "ERROR: Refusing to replace another app." >&2; exit 1; }
    rm -rf -- "$DESTINATION"
fi
mv "$APP" "$DESTINATION"
echo "Built: $DESTINATION"
