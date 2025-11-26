# Task 4 — Checksums & Signatures


**Agent**: Security Agent, Release Agent
**Estimated**: 3 days

#### 4.1 Generate checksums for all artifacts
- [ ] Script: `boot/scripts/checksum-release.sh`
- [ ] Artifacts to checksum:
  - `phase-boot-usb.img.gz`
  - `phase-boot-amd64.qcow2.gz`
  - `phase-boot-arm64.qcow2.gz`
  - `phase-boot-parallels.tar.gz`
- [ ] Output: `SHA256SUMS` file
  ```
  abc123...  phase-boot-usb.img.gz
  def456...  phase-boot-amd64.qcow2.gz
  ghi789...  phase-boot-arm64.qcow2.gz
  jkl012...  phase-boot-parallels.tar.gz
  ```
- [ ] Makefile target: `make checksums`

**Dependencies**: Tasks 1.2, 2.4, 3.3
**Output**: Checksum generation script, `SHA256SUMS` file

#### 4.2 Sign checksums with release key
- [ ] Use: Phase release signing key (Ed25519, from M3 trust model)
- [ ] Script: `boot/scripts/sign-release.sh`
- [ ] Command: `phase-manifest-sign --input SHA256SUMS --key release.priv --output SHA256SUMS.sig`
  - Note: Reuse manifest signing tool from M3 Task 2.2, or create dedicated signing script
- [ ] Output: `SHA256SUMS.sig` (Ed25519 signature over checksums file)
- [ ] Makefile target: `make sign-release`

**Dependencies**: Task 4.1, M3 Task 2.2 (signing tool)
**Output**: Signing script, `SHA256SUMS.sig` file

#### 4.3 Distribute public verification key
- [ ] Public key: `phase-release.pub`
- [ ] Distribution:
  - Embed in website: `https://phase.io/phase-release.pub`
  - Embed in documentation: `boot/docs/verification.md`
  - Publish to keyserver (optional): `keys.openpgp.org` (if using GPG instead of Ed25519)
- [ ] Note: For MVP, Ed25519 preferred (simpler than GPG)

**Dependencies**: Task 4.2
**Output**: Public key distribution plan

#### 4.4 Verification instructions
- [ ] Document: `boot/docs/verification.md`
- [ ] Steps for users:
  1. Download artifacts: `phase-boot-usb.img.gz`, `SHA256SUMS`, `SHA256SUMS.sig`
  2. Download public key: `wget https://phase.io/phase-release.pub`
  3. Verify checksums: `sha256sum -c SHA256SUMS`
  4. Verify signature:
     - Tool: `phase-verify --checksums SHA256SUMS --sig SHA256SUMS.sig --key phase-release.pub`
     - OR: Provide standalone verification script if `phase-verify` not user-facing
  5. If both pass: Artifacts are authentic and untampered

**Dependencies**: Tasks 4.1-4.3
**Output**: Verification instructions document

---
