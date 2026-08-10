#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# Shared, local-only Phase validation entrypoint. It delegates to the existing
# Cargo test/lint surfaces and stores auditable logs beneath ignored `target/`.
# Remote deployment or mutation is intentionally out of scope.

set -uo pipefail

# Helper functions are passed by name through run_step for isolated logging;
# per-function ShellCheck annotations document those indirect invocations.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
EVIDENCE_BASE="${PHASE_VALIDATION_EVIDENCE_DIR:-$REPO_ROOT/target/phase-validation}"
PROFILE=""

usage() {
    cat <<'EOF'
Phase validation runner

Usage:
  scripts/phase-validate.sh [--evidence-dir PATH] PROFILE

Profiles:
  phase-net  Run phase-net package tests and clippy.
  lucid      Run LUCID library, compiled HTTP, backend integration, and clippy.
  artifact   Run phase-artifact-server tests and clippy.
  adversarial Run protocol, HTTP-boundary, backend, artifact, and security gates.
  linux      Run the complete workspace build/test/lint gates on a Linux host.
  release    Build, inspect, smoke-test, and checksum the native lucidd bundle.
  security   Run cargo-audit and cargo-deny locally when installed.
  workspace  Run the whole-workspace format, test, and clippy gates.
  qualification Run workspace, release, adversarial, and security gates.
  all        Alias for qualification.

Exit status:
  0  Every required step passed.
  1  One or more required steps failed.
  2  Nothing failed, but a required prerequisite/step was skipped.

Evidence defaults to target/phase-validation/<UTC-run-id>/, which is already
ignored by the repository. This runner never contacts or mutates remote hosts.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --evidence-dir)
            if [[ $# -lt 2 ]]; then
                echo "ERROR: --evidence-dir requires a path" >&2
                exit 1
            fi
            EVIDENCE_BASE="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        phase-net|lucid|artifact|adversarial|linux|release|security|workspace|qualification|all)
            if [[ -n "$PROFILE" ]]; then
                echo "ERROR: choose exactly one profile" >&2
                exit 1
            fi
            PROFILE="$1"
            shift
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [[ -z "$PROFILE" ]]; then
    echo "ERROR: a profile is required" >&2
    usage >&2
    exit 1
fi

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        return 1
    fi
}

source_fingerprint() {
    (
        cd "$REPO_ROOT" || exit 1
        find Cargo.toml Cargo.lock deny.toml crates scripts -type f -print \
            | LC_ALL=C sort \
            | while IFS= read -r path; do
                printf '%s\t' "$path"
                sha256_file "$path" || exit 1
            done
    ) | if command -v sha256sum >/dev/null 2>&1; then
        sha256sum | awk '{ print $1 }'
    else
        shasum -a 256 | awk '{ print $1 }'
    fi
}

RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
EVIDENCE_DIR="$EVIDENCE_BASE/$RUN_ID"
SUMMARY="$EVIDENCE_DIR/summary.tsv"
METADATA="$EVIDENCE_DIR/metadata.txt"
mkdir -p "$EVIDENCE_DIR"

SOURCE_FINGERPRINT="$(source_fingerprint)" || exit 1
CARGO_LOCK_SHA256="$(sha256_file "$REPO_ROOT/Cargo.lock")" || exit 1

printf 'step\tstatus\texit_code\tduration_seconds\tcommand\tlog\n' > "$SUMMARY"

{
    echo "run_id=$RUN_ID"
    echo "profile=$PROFILE"
    echo "started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "repository=$REPO_ROOT"
    echo "commit=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "source_fingerprint=$SOURCE_FINGERPRINT"
    echo "cargo_lock_sha256=$CARGO_LOCK_SHA256"
    echo "cargo=$(cargo --version 2>/dev/null || echo unavailable)"
    echo "rustc=$(rustc --version 2>/dev/null || echo unavailable)"
    echo "host_os=$(uname -s 2>/dev/null || echo unavailable)"
    echo "host_arch=$(uname -m 2>/dev/null || echo unavailable)"
    echo "uname=$(uname -a 2>/dev/null || echo unavailable)"
    echo "dirty_state_begin"
    git -C "$REPO_ROOT" status --short 2>/dev/null || true
    echo "dirty_state_end"
} > "$METADATA"

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

render_command() {
    local rendered=""
    local arg
    for arg in "$@"; do
        printf -v rendered '%s%q ' "$rendered" "$arg"
    done
    printf '%s' "${rendered% }"
}

record_result() {
    local step="$1"
    local status="$2"
    local exit_code="$3"
    local duration="$4"
    local command="$5"
    local log="$6"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$step" "$status" "$exit_code" "$duration" "$command" "$log" >> "$SUMMARY"
}

skip_step() {
    local step="$1"
    local reason="$2"
    local log="$EVIDENCE_DIR/$step.log"
    printf 'SKIP: %s\n' "$reason" | tee "$log"
    record_result "$step" "SKIP" "2" "0" "$reason" "$(basename "$log")"
    SKIP_COUNT=$((SKIP_COUNT + 1))
}

run_step() {
    local step="$1"
    shift
    local log="$EVIDENCE_DIR/$step.log"
    local command
    command="$(render_command "$@")"
    local started
    started="$(date +%s)"

    echo "==> $step"
    echo "    $command"
    (
        cd "$REPO_ROOT" || exit 1
        "$@"
    ) 2>&1 | tee "$log"
    local exit_code=${PIPESTATUS[0]}
    local duration=$(( $(date +%s) - started ))

    if [[ $exit_code -eq 0 ]]; then
        echo "PASS: $step (${duration}s)"
        record_result "$step" "PASS" "$exit_code" "$duration" "$command" "$(basename "$log")"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo "FAIL: $step (exit $exit_code, ${duration}s)" >&2
        record_result "$step" "FAIL" "$exit_code" "$duration" "$command" "$(basename "$log")"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

# shellcheck disable=SC2329
build_lucidd_release_bundle() {
    command -v tar >/dev/null 2>&1 || return 1
    local host
    host="$(rustc -vV | awk '/^host:/ { print $2 }')"
    [[ -n "$host" ]] || return 1
    local version
    version="$(awk -F '"' '/^version = / { print $2; exit }' "$REPO_ROOT/crates/lucidd/Cargo.toml")"
    [[ -n "$version" ]] || return 1
    local bundle_name="lucidd-${version}-${host}"
    local stage="$EVIDENCE_DIR/$bundle_name"
    mkdir -p "$stage/bin" "$stage/systemd" "$stage/config"
    cp "$REPO_ROOT/target/release/lucidd" "$stage/bin/lucidd" || return 1
    cp "$REPO_ROOT/crates/lucidd/systemd/lucidd-relay.service" "$stage/systemd/" || return 1
    cp "$REPO_ROOT/crates/lucidd/systemd/infrastructure.env.example" "$stage/config/" || return 1
    cp "$REPO_ROOT/LICENSE" "$stage/LICENSE" || return 1
    tar -C "$EVIDENCE_DIR" -czf "$EVIDENCE_DIR/$bundle_name.tar.gz" "$bundle_name"
}

# shellcheck disable=SC2329
validate_lucidd_bundle_contents() {
    local archive
    archive="$(find "$EVIDENCE_DIR" -maxdepth 1 -type f -name 'lucidd-*.tar.gz' -print -quit)"
    [[ -n "$archive" && -f "$archive" ]] || return 1
    local package_list
    package_list="$(tar -tzf "$archive")" || return 1
    local path
    while IFS= read -r path; do
        local lower
        lower="$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')"
        case "$lower" in
            */config/infrastructure.env.example)
                ;;
            .git/*|*/.git/*|target/*|*/target/*|*.env|*.env.*|*.pem|*.key|*.p12|*.pfx|*/id_rsa|*/id_ed25519|*/credentials|*/.netrc|*.gguf|*.safetensors|*.onnx|*.mlmodel|*.mlpackage/*|*.bin|*.pt|*.pth|*.ckpt|*.npz|*.npy)
                echo "forbidden package content: $path" >&2
                return 1
                ;;
        esac
    done <<< "$package_list"
    local binary_count
    binary_count="$(printf '%s\n' "$package_list" | awk '/\/bin\/lucidd$/ { count++ } END { print count + 0 }')"
    local service_count
    service_count="$(printf '%s\n' "$package_list" | awk '/\/systemd\/lucidd-relay.service$/ { count++ } END { print count + 0 }')"
    local config_count
    config_count="$(printf '%s\n' "$package_list" | awk '/\/config\/infrastructure.env.example$/ { count++ } END { print count + 0 }')"
    [[ "$binary_count" -eq 1 && "$service_count" -eq 1 && "$config_count" -eq 1 ]] || return 1
    printf '%s\n' "$package_list"
}

# shellcheck disable=SC2329
smoke_test_lucidd_bundle() {
    local archive
    archive="$(find "$EVIDENCE_DIR" -maxdepth 1 -type f -name 'lucidd-*.tar.gz' -print -quit)"
    [[ -n "$archive" && -f "$archive" ]] || return 1
    local unpack="$EVIDENCE_DIR/unpacked"
    mkdir -p "$unpack"
    tar -C "$unpack" -xzf "$archive" || return 1
    local binary
    binary="$(find "$unpack" -type f -path '*/bin/lucidd' -print -quit)"
    [[ -n "$binary" && -x "$binary" ]] || return 1
    "$binary" --help >/dev/null
}

# shellcheck disable=SC2329
checksum_lucidd_packages() {
    local found=0
    local archive
    for archive in "$EVIDENCE_DIR"/lucidd-*.tar.gz; do
        [[ -f "$archive" ]] || continue
        found=1
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$archive"
        elif command -v shasum >/dev/null 2>&1; then
            shasum -a 256 "$archive"
        else
            echo "no SHA-256 utility is installed" >&2
            return 1
        fi
    done
    [[ $found -eq 1 ]]
}

# shellcheck disable=SC2329
validate_linux_evidence() {
    local evidence="$1"
    local expected_arch="$2"
    local summary="$evidence"
    if [[ -d "$evidence" ]]; then
        summary="$evidence/summary.tsv"
    fi
    local evidence_dir
    evidence_dir="$(dirname "$summary")"
    local metadata="$evidence_dir/metadata.txt"
    [[ -f "$summary" && -f "$metadata" ]] || return 1
    grep -q '^profile=linux$' "$metadata" || return 1
    grep -q '^host_os=Linux$' "$metadata" || return 1
    grep -q "^host_arch=${expected_arch}$" "$metadata" || return 1
    grep -q "^source_fingerprint=${SOURCE_FINGERPRINT}$" "$metadata" || return 1
    grep -q "^cargo_lock_sha256=${CARGO_LOCK_SHA256}$" "$metadata" || return 1
    awk -F '\t' 'NR > 1 && $2 != "PASS" { exit 1 } END { if (NR <= 1) exit 1 }' "$summary" || return 1
    local required_step
    for required_step in \
        linux-workspace-release-build \
        linux-workspace-tests \
        linux-workspace-clippy; do
        awk -F '\t' -v required="$required_step" '
            $1 == required && $2 == "PASS" { count++ }
            END { exit(count == 1 ? 0 : 1) }
        ' "$summary" || return 1
    done
}

require_linux_qualification_evidence() {
    local arch
    local variable
    local evidence
    for arch in x86_64 aarch64; do
        case "$arch" in
            x86_64) variable="PHASE_LINUX_X86_64_EVIDENCE" ;;
            aarch64) variable="PHASE_LINUX_AARCH64_EVIDENCE" ;;
        esac
        evidence="${!variable:-}"
        if [[ -z "$evidence" ]]; then
            skip_step "linux-${arch}-evidence" "set $variable to a passing Linux profile evidence directory"
        else
            run_step "linux-${arch}-evidence" validate_linux_evidence "$evidence" "$arch"
        fi
    done
}

require_native_macos_arm64() {
    if [[ "$(uname -s 2>/dev/null || true)" != "Darwin" \
        || "$(uname -m 2>/dev/null || true)" != "arm64" ]]; then
        skip_step "macos-arm64-host" "qualification must run on macOS arm64 in addition to attached Linux evidence"
    fi
}

require_cargo() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi
    skip_step "$1" "required command not found: cargo"
    return 1
}

run_phase_net() {
    if ! require_cargo "phase-net-prerequisite"; then
        return
    fi
    run_step "phase-net-tests" cargo test --locked --offline -p phase-net --all-targets
    run_step "phase-net-clippy" cargo clippy --locked --offline -p phase-net --all-targets -- -D warnings
}

run_lucid() {
    if ! require_cargo "lucid-prerequisite"; then
        return
    fi
    run_step "lucid-lib-tests" cargo test --locked --offline -p lucidd --lib
    run_step "lucid-http-tests" cargo test --locked --offline -p lucidd --test http_api
    run_step "lucid-backend-tests" cargo test --locked --offline -p lucidd --test llama_worker
    run_step "lucid-clippy" cargo clippy --locked --offline -p lucidd --all-targets -- -D warnings
}

run_artifact() {
    if ! require_cargo "artifact-prerequisite"; then
        return
    fi
    run_step "artifact-tests" cargo test --locked --offline -p phase-artifact-server
    run_step "artifact-clippy" cargo clippy --locked --offline -p phase-artifact-server --all-targets -- -D warnings
}

run_security() {
    if ! command -v cargo >/dev/null 2>&1; then
        skip_step "security-audit" "required command not found: cargo"
        skip_step "security-deny" "required command not found: cargo"
        return
    fi

    if cargo audit --version >/dev/null 2>&1; then
        # cargo-audit has no --locked flag. --no-fetch keeps the scan strictly
        # local; --stale permits use of the already-cached advisory database.
        run_step "security-audit" cargo audit --no-fetch --stale
    else
        skip_step "security-audit" "cargo-audit is not installed"
    fi

    if cargo deny --version >/dev/null 2>&1; then
        # Both Cargo dependency resolution and advisory lookup stay offline.
        run_step "security-deny" cargo deny --locked --offline check --disable-fetch
    else
        skip_step "security-deny" "cargo-deny is not installed"
    fi
}

run_adversarial() {
    if ! require_cargo "adversarial-prerequisite"; then
        return
    fi
    # These suites contain the malformed-frame, signature, replay, bounded-I/O,
    # CORS/body-limit, receipt, content-transfer, and backend abuse cases. Run
    # complete targets so a renamed test cannot silently disappear behind a
    # string filter that still exits zero.
    run_step "adversarial-phase-net" cargo test --locked --offline -p phase-net --all-targets
    run_step "adversarial-lucid-lib" cargo test --locked --offline -p lucidd --lib
    run_step "adversarial-lucid-http" cargo test --locked --offline -p lucidd --test http_api
    run_step "adversarial-lucid-backends" cargo test --locked --offline -p lucidd --test llama_worker
    run_step "adversarial-artifacts" cargo test --locked --offline -p phase-artifact-server
    run_security
}

run_linux() {
    if [[ "$(uname -s 2>/dev/null || true)" != "Linux" ]]; then
        skip_step "linux-host" "linux profile must run on the target Linux host"
        return
    fi
    if ! require_cargo "linux-prerequisite"; then
        return
    fi
    run_step "linux-workspace-release-build" cargo build --workspace --all-targets --release --locked --offline
    run_step "linux-workspace-tests" cargo test --workspace --locked --offline
    run_step "linux-workspace-clippy" cargo clippy --workspace --all-targets --locked --offline -- -D warnings
}

run_release() {
    if ! require_cargo "release-prerequisite"; then
        return
    fi
    run_step "release-native-build" cargo build --workspace --all-targets --release --locked --offline
    # Build and verify the actual host-native operator bundle. Cargo source
    # packaging cannot verify unpublished Phase path dependencies; the release
    # artifact is therefore the already-tested binary plus its service/config.
    run_step "release-lucidd-package" build_lucidd_release_bundle
    run_step "release-lucidd-package-contents" validate_lucidd_bundle_contents
    run_step "release-lucidd-package-smoke" smoke_test_lucidd_bundle
    run_step "release-lucidd-package-checksum" checksum_lucidd_packages
}

run_workspace() {
    if ! require_cargo "workspace-prerequisite"; then
        return
    fi
    run_step "workspace-fmt" cargo fmt --all -- --check
    run_step "workspace-tests" cargo test --workspace --locked --offline
    run_step "workspace-clippy" cargo clippy --workspace --all-targets --locked --offline -- -D warnings
}

case "$PROFILE" in
    phase-net)
        run_phase_net
        ;;
    lucid)
        run_lucid
        ;;
    artifact)
        run_artifact
        ;;
    adversarial)
        run_adversarial
        ;;
    linux)
        run_linux
        ;;
    release)
        run_release
        ;;
    security)
        run_security
        ;;
    workspace)
        run_workspace
        ;;
    qualification|all)
        # Workspace tests/clippy cover every component. Adversarial repeats the
        # security-critical targets intentionally so its evidence logs remain
        # self-contained and reviewable beside optimized/package evidence.
        run_workspace
        run_release
        run_adversarial
        require_native_macos_arm64
        # Qualification is intentionally incomplete without evidence from
        # both supported Linux architecture profiles. A macOS-only run must
        # return SKIP/2 rather than a false green.
        require_linux_qualification_evidence
        ;;
esac

{
    echo "finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "passes=$PASS_COUNT"
    echo "failures=$FAIL_COUNT"
    echo "skips=$SKIP_COUNT"
} >> "$METADATA"

echo
echo "Validation summary: PASS=$PASS_COUNT FAIL=$FAIL_COUNT SKIP=$SKIP_COUNT"
echo "Evidence: $EVIDENCE_DIR"

if [[ $FAIL_COUNT -gt 0 ]]; then
    exit 1
fi
if [[ $SKIP_COUNT -gt 0 ]]; then
    exit 2
fi
exit 0
