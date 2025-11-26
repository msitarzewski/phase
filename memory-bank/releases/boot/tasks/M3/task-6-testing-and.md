# Task 6 — Testing & Validation


**Agent**: Security Agent, Transport Agent
**Estimated**: 6 days

#### 6.1 End-to-end test: Happy path
- [ ] Setup:
  - Signed manifest on DHT/mDNS
  - Artifacts hosted on HTTPS mirror
  - Boot Phase USB (Internet Mode)
- [ ] Validation:
  - Boot to initramfs
  - Discover manifest
  - Verify manifest signature → SUCCESS
  - Fetch all artifacts (kernel, initramfs, rootfs)
  - Verify all artifact hashes → SUCCESS
  - Console displays "All artifacts ready"

**Dependencies**: All M3 tasks
**Output**: End-to-end happy path test results

#### 6.2 Tamper test: Invalid manifest signature
- [ ] Setup: Modify manifest JSON after signing (change artifact URL)
- [ ] Validation:
  - Verify manifest → FAILED
  - Boot aborts with "Manifest signature verification failed"
  - Does NOT fetch artifacts

**Dependencies**: Task 2.6 (verification tests)
**Output**: Tamper test results (signature)

#### 6.3 Tamper test: Invalid artifact hash
- [ ] Setup:
  - Valid signed manifest
  - Serve corrupted kernel (hash mismatch)
- [ ] Validation:
  - Fetch kernel → Hash verification fails
  - Boot aborts with "Kernel hash mismatch"
  - Does NOT proceed to kexec

**Dependencies**: Task 3.7 (fetch tests)
**Output**: Tamper test results (hash)

#### 6.4 Rollback test
- [ ] Setup:
  - Cache manifest version 100
  - Serve manifest version 99 (signed correctly, but older)
- [ ] Validation:
  - Verify manifest → FAILED (rollback detected)
  - Boot aborts with "Manifest version downgrade detected"

**Dependencies**: Task 2.5 (rollback protection)
**Output**: Rollback test results

#### 6.5 Cache test
- [ ] Setup: Boot twice with same manifest
- [ ] Validation:
  - First boot: Downloads all artifacts (cache miss)
  - Second boot: Uses cached artifacts (cache hit), no downloads
  - Console displays "Cache hit" messages

**Dependencies**: Task 4.6 (cache tests)
**Output**: Cache hit/miss test results

#### 6.6 Private mode test
- [ ] Setup: Boot with Private Mode entry
- [ ] Validation:
  - Manifest verification works (in-memory)
  - Artifacts fetched (no cache lookup)
  - No writes to `/cache/` partition
  - Second boot: Re-downloads all artifacts (no cache persistence)

**Dependencies**: Task 5.2 (Private Mode integration)
**Output**: Private mode test results

---
