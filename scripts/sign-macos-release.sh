#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Sign, notarize, staple, and verify macOS LUCID release artifacts using the
# maintainer's local macOS Keychain (zero Apple credentials ever enter GitHub).
#
# Usage:
#   scripts/sign-macos-release.sh --from-release <tag> [--dry-run] [--out-dir DIR] [--publish <tag>] [--repo OWNER/REPO]
#   scripts/sign-macos-release.sh --from-dir <dir>     [--dry-run] [--out-dir DIR] [--publish <tag>] [--repo OWNER/REPO]
#
# Required environment variables (store outside the repo, e.g. ~/.config/lucid/signing.env):
#   LUCID_APPLE_TEAM_ID           10-character Apple Team ID (used to select Developer ID Application in Keychain)
#                                 (or set LUCID_APPLE_SIGNING_IDENTITY directly)
#   LUCID_APPLE_NOTARY_PROFILE    notarytool Keychain profile name (required unless --dry-run)
#                                 Create once via:
#                                   xcrun notarytool store-credentials <profile> --apple-id <id> --team-id <team>
set -euo pipefail
export PATH="$PATH:/usr/sbin"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

FROM_RELEASE=""
FROM_DIR=""
OUT_DIR=""
PUBLISH_TAG=""
GH_REPO_OVERRIDE=""
DRY_RUN=0

usage() {
    sed -n '3,16p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

step() {
    printf '\n==> %s\n' "$*"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --from-release)
            FROM_RELEASE="${2:?--from-release requires a tag (e.g. v0.2.0)}"
            shift 2
            ;;
        --from-release=*)
            FROM_RELEASE="${1#--from-release=}"
            shift
            ;;
        --from-dir)
            FROM_DIR="${2:?--from-dir requires a directory path}"
            shift 2
            ;;
        --from-dir=*)
            FROM_DIR="${1#--from-dir=}"
            shift
            ;;
        --out-dir)
            OUT_DIR="${2:?--out-dir requires a directory path}"
            shift 2
            ;;
        --out-dir=*)
            OUT_DIR="${1#--out-dir=}"
            shift
            ;;
        --publish)
            PUBLISH_TAG="${2:?--publish requires a release tag}"
            shift 2
            ;;
        --publish=*)
            PUBLISH_TAG="${1#--publish=}"
            shift
            ;;
        --repo)
            GH_REPO_OVERRIDE="${2:?--repo requires OWNER/REPO}"
            shift 2
            ;;
        --repo=*)
            GH_REPO_OVERRIDE="${1#--repo=}"
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage 0
            ;;
        *)
            die "unknown argument: $1 (see --help)"
            ;;
    esac
done

[[ "$(uname -s)" == "Darwin" ]] || die "sign-macos-release.sh must run on macOS"
if [[ -n "$FROM_RELEASE" && -n "$FROM_DIR" ]]; then
    die "pass only one of --from-release or --from-dir"
fi
if [[ -z "$FROM_RELEASE" && -z "$FROM_DIR" ]]; then
    die "must pass --from-release <tag> or --from-dir <dir>"
fi
if [[ "$DRY_RUN" -eq 1 && -n "$PUBLISH_TAG" ]]; then
    die "--dry-run cannot be combined with --publish"
fi

for cmd in codesign xcrun hdiutil ditto spctl shasum tar file security xattr; do
    command -v "$cmd" >/dev/null 2>&1 || die "required macOS tool not found: $cmd"
done
if [[ -n "$FROM_RELEASE" || -n "$PUBLISH_TAG" ]]; then
    command -v gh >/dev/null 2>&1 || die "gh CLI is required for --from-release / --publish"
fi

resolve_repo() {
    if [[ -n "$GH_REPO_OVERRIDE" ]]; then
        printf '%s' "$GH_REPO_OVERRIDE"
        return 0
    fi
    (cd "$REPO_ROOT" && gh repo view --json nameWithOwner --jq .nameWithOwner) \
        || die "could not determine GitHub repository; pass --repo OWNER/REPO"
}

# ── Preflight: signing identity & notarytool Keychain profile ─────────────────

IDENTITY="${LUCID_APPLE_SIGNING_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
    TEAM_ID="${LUCID_APPLE_TEAM_ID:-}"
    [[ -n "$TEAM_ID" ]] || die "set LUCID_APPLE_TEAM_ID (10-char Team ID) or LUCID_APPLE_SIGNING_IDENTITY"
    [[ "$TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || die "LUCID_APPLE_TEAM_ID must be a 10-character alphanumeric Apple Team ID"
    MATCHES="$(security find-identity -v -p codesigning | grep "Developer ID Application" | grep "($TEAM_ID)" || true)"
    MATCH_COUNT="$(printf '%s\n' "$MATCHES" | sed '/^$/d' | wc -l | tr -d ' ')"
    [[ "$MATCH_COUNT" -ge 1 ]] || die "no Developer ID Application certificate found in Keychain for Team ID $TEAM_ID"
    [[ "$MATCH_COUNT" -eq 1 ]] || die "multiple Developer ID Application certificates match Team ID $TEAM_ID; set LUCID_APPLE_SIGNING_IDENTITY explicitly"
    IDENTITY="$(printf '%s\n' "$MATCHES" | awk '{print $2}')"
fi

NOTARY_PROFILE="${LUCID_APPLE_NOTARY_PROFILE:-}"
if [[ "$DRY_RUN" -eq 0 ]]; then
    [[ -n "$NOTARY_PROFILE" ]] || die "set LUCID_APPLE_NOTARY_PROFILE (Keychain profile from xcrun notarytool store-credentials)"
    step "Preflight: verifying notarytool Keychain profile"
    NOTARY_CHECK=""
    if ! NOTARY_CHECK="$(xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" 2>&1)"; then
        echo "$NOTARY_CHECK" | head -n 5 >&2
        case "$NOTARY_CHECK" in
            *agreement*|*Agreement*)
                die "Apple notary service rejected the request due to a pending App Store Connect agreement"
                ;;
            *)
                die "notarytool check failed for Keychain profile '$NOTARY_PROFILE'"
                ;;
        esac
    fi
fi

# ── Workspace setup ───────────────────────────────────────────────────────────

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lucid-macos-sign.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [[ -z "$OUT_DIR" ]]; then
    label="${FROM_RELEASE:-$(date -u +%Y%m%dT%H%M%SZ)}"
    OUT_DIR="$REPO_ROOT/build/signed-macos/$label"
fi
mkdir -p "$OUT_DIR"

INPUT_DIR="$WORK/input"
mkdir -p "$INPUT_DIR"

DMG_NAME=""
GH_REPO=""
if [[ -n "$FROM_RELEASE" || -n "$PUBLISH_TAG" ]]; then
    GH_REPO="$(resolve_repo)"
fi

if [[ -n "$FROM_RELEASE" ]]; then
    step "Downloading macOS release artifacts for $FROM_RELEASE from $GH_REPO"
    gh release download "$FROM_RELEASE" \
        --repo "$GH_REPO" \
        --dir "$INPUT_DIR" \
        --pattern "SHA256SUMS.txt" \
        --pattern "lucidd-*-aarch64-apple-darwin.tar.gz" \
        --pattern "LUCID-*-aarch64-apple-darwin.zip"

    [[ -f "$INPUT_DIR/SHA256SUMS.txt" ]] || die "SHA256SUMS.txt missing from release $FROM_RELEASE"

    # Preserve exact .dmg asset filename published by Tauri if present in SHA256SUMS.txt.
    DMG_NAME="$(awk '$2 ~ /\.dmg$/ { print $2; exit }' "$INPUT_DIR/SHA256SUMS.txt" || true)"

    step "Verifying downloaded artifact SHA-256 checksums"
    (
        cd "$INPUT_DIR"
        for f in lucidd-*-aarch64-apple-darwin.tar.gz LUCID-*-aarch64-apple-darwin.zip; do
            [[ -f "$f" ]] || continue
            expected="$(awk -v name="$f" '$2 == name { print $1; exit }' SHA256SUMS.txt)"
            [[ -n "$expected" ]] || die "no SHA256SUMS.txt entry for $f"
            actual="$(shasum -a 256 "$f" | awk '{print $1}')"
            [[ "$actual" == "$expected" ]] || die "checksum mismatch on downloaded $f"
            echo "  verified $f"
        done
    )
else
    [[ -d "$FROM_DIR" ]] || die "--from-dir directory does not exist: $FROM_DIR"
    cp "$FROM_DIR"/lucidd-*-aarch64-apple-darwin.tar.gz "$INPUT_DIR/" 2>/dev/null || true
    cp "$FROM_DIR"/LUCID-*-aarch64-apple-darwin.zip "$INPUT_DIR/" 2>/dev/null || true
    for existing_dmg in "$FROM_DIR"/*.dmg; do
        if [[ -f "$existing_dmg" ]]; then
            DMG_NAME="$(basename "$existing_dmg")"
            break
        fi
    done
fi

CLI_TARBALL="$(find "$INPUT_DIR" -maxdepth 1 -type f -name 'lucidd-*-aarch64-apple-darwin.tar.gz' | head -n 1 || true)"
APP_ZIP="$(find "$INPUT_DIR" -maxdepth 1 -type f -name 'LUCID-*-aarch64-apple-darwin.zip' | head -n 1 || true)"
[[ -n "$CLI_TARBALL" || -n "$APP_ZIP" ]] || die "no macOS artifacts (lucidd-*-aarch64-apple-darwin.tar.gz or LUCID-*-aarch64-apple-darwin.zip) found"

# ── Signing & notarization helpers ────────────────────────────────────────────

sign_runtime() {
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$1"
}

check_signed() {
    local target="$1"
    local require_runtime="${2:-}"
    local info
    info="$(codesign -dvv "$target" 2>&1)" || die "$target is not signed"
    grep -q "Authority=Developer ID Application" <<<"$info" || die "$target is not signed with Developer ID Application"
    grep -q "^Timestamp=" <<<"$info" || die "$target has no secure timestamp"
    if [[ "$require_runtime" == "runtime" ]]; then
        grep -q "flags=.*runtime" <<<"$info" || die "$target is missing hardened runtime"
    fi
}

notarize_artifact() {
    local artifact="$1"
    local json_out="$WORK/notary.json"
    rm -f "$json_out"
    xcrun notarytool submit "$artifact" \
        --keychain-profile "$NOTARY_PROFILE" \
        --wait --output-format json > "$json_out" || true
    local sub_id
    sub_id="$(sed -nE 's/.*"id" *: *"([^"]+)".*/\1/p' "$json_out" | head -n 1)"
    if ! grep -q '"status" *: *"Accepted"' "$json_out"; then
        echo "Notarization failed for $(basename "$artifact"):" >&2
        cat "$json_out" >&2
        if [[ -n "$sub_id" ]]; then
            xcrun notarytool log "$sub_id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
        fi
        exit 1
    fi
    echo "  notarization accepted (submission $sub_id)"
}

SIGNED_ASSETS=()

# ── 1. Standalone CLI tarball (lucidd) ────────────────────────────────────────

if [[ -n "$CLI_TARBALL" ]]; then
    TAR_BASENAME="$(basename "$CLI_TARBALL")"
    step "Signing standalone macOS CLI ($TAR_BASENAME)"
    CLI_WORK="$WORK/cli"
    mkdir -p "$CLI_WORK"
    tar -C "$CLI_WORK" -xzf "$CLI_TARBALL"
    CLI_BIN="$(find "$CLI_WORK" -type f -path '*/bin/lucidd' | head -n 1 || true)"
    [[ -n "$CLI_BIN" ]] || die "bin/lucidd not found inside $TAR_BASENAME"
    CLI_ROOT_DIR="$(basename "$(dirname "$(dirname "$CLI_BIN")")")"

    xattr -cr "$CLI_BIN" 2>/dev/null || true
    sign_runtime "$CLI_BIN"
    codesign --verify --strict --verbose=2 "$CLI_BIN"
    check_signed "$CLI_BIN" runtime
    "$CLI_BIN" --help >/dev/null

    if [[ "$DRY_RUN" -eq 0 ]]; then
        step "Notarizing standalone lucidd binary"
        CLI_NOTARY_ZIP="$WORK/lucidd-cli-notary.zip"
        ditto -c -k --keepParent "$CLI_BIN" "$CLI_NOTARY_ZIP"
        notarize_artifact "$CLI_NOTARY_ZIP"
    fi

    SIGNED_TAR="$OUT_DIR/$TAR_BASENAME"
    COPYFILE_DISABLE=1 tar -C "$CLI_WORK" -czf "$SIGNED_TAR" "$CLI_ROOT_DIR"
    SIGNED_ASSETS+=("$SIGNED_TAR")
    echo "  wrote $SIGNED_TAR"
fi

# ── 2. LUCID.app (.zip) and .dmg ──────────────────────────────────────────────

if [[ -n "$APP_ZIP" ]]; then
    ZIP_BASENAME="$(basename "$APP_ZIP")"
    step "Signing LUCID.app from $ZIP_BASENAME"
    APP_WORK="$WORK/app"
    mkdir -p "$APP_WORK"
    ditto -x -k "$APP_ZIP" "$APP_WORK"
    APP_BUNDLE="$APP_WORK/LUCID.app"
    [[ -d "$APP_BUNDLE" ]] || die "LUCID.app not found inside $ZIP_BASENAME"

    xattr -cr "$APP_BUNDLE" 2>/dev/null || true

    # Sign all nested Mach-O binaries inside-out before signing the outer .app bundle.
    while IFS= read -r -d '' candidate; do
        if file -b "$candidate" | grep -q "Mach-O"; then
            echo "  signing nested Mach-O: ${candidate#"$APP_BUNDLE"/}"
            sign_runtime "$candidate"
        fi
    done < <(find "$APP_BUNDLE/Contents" -type f \( -perm -u+x -o -name '*.dylib' -o -name '*.so' \) -print0 | sort -z)

    sign_runtime "$APP_BUNDLE"

    step "Verifying LUCID.app signatures and hardened runtime"
    codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"
    check_signed "$APP_BUNDLE" runtime
    while IFS= read -r -d '' candidate; do
        if file -b "$candidate" | grep -q "Mach-O"; then
            check_signed "$candidate" runtime
        fi
    done < <(find "$APP_BUNDLE/Contents" -type f \( -perm -u+x -o -name '*.dylib' -o -name '*.so' \) -print0)

    APP_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || true)"
    [[ -n "$APP_VERSION" ]] || APP_VERSION="$(printf '%s' "$ZIP_BASENAME" | sed -nE 's/^LUCID-([^-]+)-.*/\1/p')"
    [[ -n "$APP_VERSION" ]] || die "could not determine LUCID.app version"

    if [[ "$DRY_RUN" -eq 0 ]]; then
        step "Notarizing and stapling LUCID.app"
        APP_NOTARY_ZIP="$WORK/LUCID-app-notary.zip"
        ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "$APP_NOTARY_ZIP"
        notarize_artifact "$APP_NOTARY_ZIP"
        xcrun stapler staple "$APP_BUNDLE"
        xcrun stapler validate "$APP_BUNDLE"
        spctl --assess --type execute --verbose=2 "$APP_BUNDLE"
    fi

    SIGNED_ZIP="$OUT_DIR/$ZIP_BASENAME"
    rm -f "$SIGNED_ZIP"
    ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "$SIGNED_ZIP"
    SIGNED_ASSETS+=("$SIGNED_ZIP")
    echo "  wrote $SIGNED_ZIP"

    # Build, sign, notarize, and staple the outermost .dmg.
    if [[ -z "$DMG_NAME" ]]; then
        DMG_NAME="LUCID_${APP_VERSION}_aarch64.dmg"
    fi
    SIGNED_DMG="$OUT_DIR/$DMG_NAME"
    step "Creating and signing $DMG_NAME"
    DMG_STAGE="$WORK/dmg-stage"
    mkdir -p "$DMG_STAGE"
    ditto "$APP_BUNDLE" "$DMG_STAGE/LUCID.app"
    ln -s /Applications "$DMG_STAGE/Applications"
    rm -f "$SIGNED_DMG"
    hdiutil create -volname "LUCID $APP_VERSION" -srcfolder "$DMG_STAGE" -ov -format UDZO "$SIGNED_DMG" >/dev/null
    codesign --force --timestamp --sign "$IDENTITY" "$SIGNED_DMG"
    check_signed "$SIGNED_DMG"

    if [[ "$DRY_RUN" -eq 0 ]]; then
        step "Notarizing and stapling $DMG_NAME"
        notarize_artifact "$SIGNED_DMG"
        xcrun stapler staple "$SIGNED_DMG"
        xcrun stapler validate "$SIGNED_DMG"
        spctl --assess --type open --context context:primary-signature --verbose=2 "$SIGNED_DMG"
    fi

    SIGNED_ASSETS+=("$SIGNED_DMG")
    echo "  wrote $SIGNED_DMG"
fi

# ── Checksums ─────────────────────────────────────────────────────────────────

step "Local macOS SHA-256 checksums"
(
    cd "$OUT_DIR"
    shasum -a 256 -- "${SIGNED_ASSETS[@]##*/}" | LC_ALL=C sort -k2 > SHA256SUMS.macos.txt
    cat SHA256SUMS.macos.txt
)

if [[ "$DRY_RUN" -eq 1 ]]; then
    step "Dry run complete (signed and verified locally; skipped notarization and publish)"
    exit 0
fi

# ── Optional publish back to GitHub Release + refresh SHA256SUMS.txt ──────────

if [[ -n "$PUBLISH_TAG" ]]; then
    step "Uploading signed macOS artifacts to $GH_REPO release $PUBLISH_TAG"
    gh release upload "$PUBLISH_TAG" "${SIGNED_ASSETS[@]}" --repo "$GH_REPO" --clobber

    step "Regenerating SHA256SUMS.txt across all assets on $PUBLISH_TAG"
    ALL_ASSETS="$WORK/all-release-assets"
    mkdir -p "$ALL_ASSETS"
    gh release download "$PUBLISH_TAG" \
        --repo "$GH_REPO" \
        --dir "$ALL_ASSETS" \
        --pattern "*.tar.gz" \
        --pattern "*.zip" \
        --pattern "*.dmg" \
        --pattern "*.deb"

    (
        cd "$ALL_ASSETS"
        shopt -s nullglob
        files=(*.tar.gz *.zip *.dmg *.deb)
        [[ ${#files[@]} -gt 0 ]] || die "no release assets found when regenerating SHA256SUMS.txt"
        LC_ALL=C shasum -a 256 -- "${files[@]}" | LC_ALL=C sort -k2 > SHA256SUMS.txt
        cat SHA256SUMS.txt
    )

    gh release upload "$PUBLISH_TAG" "$ALL_ASSETS/SHA256SUMS.txt" --repo "$GH_REPO" --clobber
    step "Published signed macOS artifacts and updated SHA256SUMS.txt on $PUBLISH_TAG"
else
    step "Done! Signed and notarized artifacts are in $OUT_DIR"
    echo "To publish them to a GitHub release and refresh SHA256SUMS.txt, re-run with --publish <tag>."
fi
