# Task 5 — Reproducible Builds


**Agent**: Release Agent, Systems Agent
**Estimated**: 4 days

#### 5.1 Identify sources of non-determinism
- [ ] Common issues:
  - Timestamps in filesystems (mtime, ctime)
  - Build timestamps in binaries (compiler-inserted)
  - Random ordering (file lists, dictionaries)
  - UUIDs or random identifiers
- [ ] Audit: M1-M4 build scripts and Makefile targets

**Dependencies**: None
**Output**: Non-determinism audit report

#### 5.2 Eliminate non-determinism
- [ ] Timestamps:
  - Set fixed mtime for all files: `touch -t 202501010000 <file>`
  - Use `SOURCE_DATE_EPOCH` environment variable (standard for reproducible builds)
- [ ] Random ordering:
  - Sort file lists: `find . | sort`
  - Deterministic tar: `tar --sort=name --mtime=@0`
- [ ] UUIDs:
  - Use deterministic UUIDs (hash-based, not random)
  - Partition UUIDs: Fixed values or derived from image hash
- [ ] Compiler flags:
  - Strip debug info: `-fno-ident`, `-fno-emit-build-id`
  - Rust: `RUSTFLAGS="-C link-arg=-Wl,-z,defs,-z,now,-z,relro"`

**Dependencies**: Task 5.1
**Output**: Reproducibility fixes in build scripts

#### 5.3 Reproducibility verification
- [ ] Script: `boot/scripts/test-reproducible.sh`
- [ ] Steps:
  - Clean build environment: `make clean`
  - Build release: `make release`
  - Save checksums: `cp SHA256SUMS SHA256SUMS.build1`
  - Clean again: `make clean`
  - Build release again: `make release`
  - Save checksums: `cp SHA256SUMS SHA256SUMS.build2`
  - Compare: `diff SHA256SUMS.build1 SHA256SUMS.build2`
- [ ] Expected: `diff` returns no differences (identical checksums)

**Dependencies**: Task 5.2
**Output**: Reproducibility test script

#### 5.4 Document reproducible build process
- [ ] Document: `boot/docs/reproducible-builds.md`
- [ ] Content:
  - Prerequisites: Build environment setup (Docker container recommended)
  - Build commands: `make release`
  - Environment variables: `SOURCE_DATE_EPOCH`, `RUSTFLAGS`
  - Verification: How to verify your build matches official release
- [ ] Goal: Third parties can reproduce exact checksums

**Dependencies**: Task 5.3
**Output**: Reproducible builds documentation

---
