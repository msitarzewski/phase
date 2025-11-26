# Task 2 — Manifest Signing & Verification


**Agent**: Security Agent
**Estimated**: 6 days

#### 2.1 Extend manifest schema with signatures
- [ ] Update: `boot/schemas/manifest.schema.json` (from M2 Task 4.1)
- [ ] Add `signatures` array:
  ```json
  {
    "version": "0.1",
    "manifest_version": 123,  // Monotonic counter for rollback protection
    "channel": "stable",
    "arch": "x86_64",
    "artifacts": { ... },
    "signatures": [
      {
        "keyid": "targets-key-1",
        "sig": "base64-encoded-ed25519-signature"
      }
    ],
    "signed": {
      "data": "base64-encoded-canonical-manifest"
    }
  }
  ```
- [ ] Signing payload: Canonical JSON (sorted keys, no whitespace) of manifest minus `signatures` field

**Dependencies**: M2 Task 4.1 (manifest schema)
**Output**: Updated manifest schema with signatures

#### 2.2 Manifest signing tool
- [ ] Rust binary: `boot/tools/manifest-sign/`
- [ ] CLI: `phase-manifest-sign --input manifest.json --key targets.priv --output manifest.signed.json`
- [ ] Steps:
  - Load manifest JSON
  - Canonicalize: Sort keys, remove whitespace, exclude `signatures` field
  - Sign with Ed25519 private key
  - Append signature to `signatures` array
  - Write signed manifest
- [ ] Build as standalone tool (not embedded in initramfs)

**Dependencies**: Task 2.1
**Output**: Manifest signing tool source code

#### 2.3 Verification binary: phase-verify
- [ ] Rust binary: `boot/tools/verify/`
- [ ] CLI: `phase-verify --manifest manifest.json --root-key root.pub [--targets-key targets.pub]`
- [ ] Steps:
  1. Load manifest JSON
  2. Extract `signatures` field
  3. Canonicalize manifest (same as signing process)
  4. Load root public key (embedded in binary)
  5. Load targets public key (embedded OR from file)
  6. Verify targets key signature with root key (if targets key not embedded)
  7. Verify manifest signature with targets key
  8. Check manifest_version ≥ cached version (rollback protection)
  9. Output: `VERIFIED` or `FAILED` with error details

**Dependencies**: Tasks 1.3, 2.1
**Output**: Verification binary source code

#### 2.4 Build phase-verify (static, dual-arch)
- [ ] Build script: `boot/tools/verify/build.sh`
- [ ] Cross-compile:
  - `cargo build --release --target x86_64-unknown-linux-musl`
  - `cargo build --release --target aarch64-unknown-linux-musl`
- [ ] Embed root public key at compile time:
  - Use `include_bytes!("../../configs/keys/root.pub")` in Rust
- [ ] Strip symbols: `strip phase-verify`
- [ ] Verify size: <3MB per binary
- [ ] Install to initramfs: `boot/initramfs/bin/phase-verify`

**Dependencies**: Task 2.3
**Output**: Static verification binaries (x86_64, arm64)

#### 2.5 Rollback protection
- [ ] Cache manifest version:
  - File: `/cache/manifests/<channel>/<arch>/latest.version`
  - Content: Manifest version number (integer)
- [ ] Verification logic:
  - Load cached version (if exists)
  - Compare with new manifest's `manifest_version` field
  - Reject if new version < cached version (rollback attack)
  - Update cached version on successful verification
- [ ] Private Mode: Skip version caching (in-memory only)

**Dependencies**: Task 2.3
**Output**: Rollback protection logic in phase-verify

#### 2.6 Test manifest verification
- [ ] Valid manifest test:
  - Sign manifest with correct targets key
  - Verify with phase-verify
  - Expected: VERIFIED
- [ ] Invalid signature test:
  - Modify signed manifest JSON (change artifact URL)
  - Verify with phase-verify
  - Expected: FAILED (signature mismatch)
- [ ] Rollback test:
  - Cache manifest version 100
  - Try to verify manifest version 99
  - Expected: FAILED (rollback detected)
- [ ] Wrong key test:
  - Sign manifest with different key
  - Verify with phase-verify
  - Expected: FAILED (unknown key)

**Dependencies**: Tasks 2.4, 2.5
**Output**: Verification test results

---
