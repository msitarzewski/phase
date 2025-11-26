# Task 6 — Release Workflow & Distribution


**Agent**: Release Agent
**Estimated**: 4 days

#### 6.1 Create Makefile release target
- [ ] Target: `make release`
- [ ] Steps:
  - Build all images: USB, QCOW2 (x86_64, ARM64), Parallels
  - Generate checksums: `make checksums`
  - Sign checksums: `make sign-release`
  - Organize artifacts: Move to `release/` directory
- [ ] Output structure:
  ```
  release/
  ├── phase-boot-usb.img.gz
  ├── phase-boot-amd64.qcow2.gz
  ├── phase-boot-arm64.qcow2.gz
  ├── phase-boot-parallels.tar.gz
  ├── SHA256SUMS
  ├── SHA256SUMS.sig
  └── phase-release.pub
  ```

**Dependencies**: Tasks 1.2, 2.4, 3.3, 4.1, 4.2
**Output**: `make release` target, organized release directory

#### 6.2 GitHub Releases integration
- [ ] Create release script: `boot/scripts/publish-release.sh`
- [ ] Steps:
  - Tag release: `git tag -s v0.1.0 -m "Phase Boot v0.1.0"`
  - Push tag: `git push origin v0.1.0`
  - Create GitHub Release via `gh` CLI:
    ```bash
    gh release create v0.1.0 \
      release/phase-boot-usb.img.gz \
      release/phase-boot-amd64.qcow2.gz \
      release/phase-boot-arm64.qcow2.gz \
      release/phase-boot-parallels.tar.gz \
      release/SHA256SUMS \
      release/SHA256SUMS.sig \
      release/phase-release.pub \
      --title "Phase Boot v0.1.0" \
      --notes "Full release notes here"
    ```
- [ ] Automation: Optional GitHub Actions workflow (future)

**Dependencies**: Task 6.1
**Output**: Release publishing script

#### 6.3 Mirror distribution
- [ ] Primary: GitHub Releases (CDN-backed, free)
- [ ] Mirrors:
  - HTTPS mirror: Phase-operated or community mirrors (for M2 fetch)
  - IPFS: Publish to IPFS, pin CIDs on pinning service
    - Command: `ipfs add -r release/`
    - Pin: Pinata, Web3.Storage, or self-hosted IPFS node
- [ ] Update manifest: Publish manifest JSON (M2, M3) pointing to release artifacts

**Dependencies**: Task 6.1
**Output**: Mirror distribution plan, IPFS CIDs

#### 6.4 Release checklist
- [ ] Document: `boot/docs/release-checklist.md`
- [ ] Pre-release:
  - [ ] All M1-M4 tests pass
  - [ ] Reproducible build verified
  - [ ] Checksums and signatures generated
  - [ ] Release notes drafted
- [ ] Release:
  - [ ] Tag and push to GitHub
  - [ ] Publish GitHub Release with artifacts
  - [ ] Publish to IPFS and pin
  - [ ] Update website with download links
  - [ ] Announce on community channels
- [ ] Post-release:
  - [ ] Monitor downloads and user feedback
  - [ ] Address critical bugs with patch releases
  - [ ] Update documentation with known issues

**Dependencies**: All M5 tasks
**Output**: Release checklist document

---
