# Task 5 — Integration: Verify → Fetch Pipeline


**Agent**: Tooling Agent, Security Agent
**Estimated**: 4 days

#### 5.1 Orchestrator: fetch-verify-artifacts.sh
- [ ] Script: `boot/initramfs/scripts/fetch-verify-artifacts.sh`
- [ ] Inputs:
  - Manifest JSON (from M2 discovery)
  - Mode (Internet/Local/Private)
  - Cache directory path
- [ ] Steps:
  1. Verify manifest with `phase-verify`
  2. Abort if verification fails (clear error message)
  3. Extract artifact URLs and hashes from manifest
  4. For each artifact (kernel, initramfs, rootfs):
     - Check cache: `cache-lookup <hash>`
     - If cache hit: Use cached artifact
     - If cache miss: Fetch with `phase-fetch --url <url> --hash <hash>`
     - Verify hash after fetch
     - Store in cache: `cache-store <hash> <file>` (if not Private Mode)
  5. Symlink artifacts to expected paths:
     - `/tmp/kernel` → `/cache/artifacts/<kernel-hash>/data`
     - `/tmp/initramfs` → `/cache/artifacts/<initramfs-hash>/data`
     - `/tmp/rootfs` → `/cache/artifacts/<rootfs-hash>/data`
  6. Output: SUCCESS or FAILURE with logs

**Dependencies**: Tasks 2.4 (verify), 3.6 (fetch), 4.3-4.4 (cache)
**Output**: Orchestrator script

#### 5.2 Integrate into init script
- [ ] Update: `boot/initramfs/init`
- [ ] Flow:
  - Network up (M1, M2)
  - Discover manifest (M2)
  - **NEW**: Verify + Fetch artifacts (`fetch-verify-artifacts.sh`)
  - On success: Proceed to kexec preparation (M4 placeholder)
  - On failure: Display error, drop to shell with diagnostics
- [ ] Console output:
  - "Verifying manifest..." → VERIFIED or FAILED
  - "Fetching kernel..." → cache hit/downloading/verified
  - "Fetching initramfs..." → ...
  - "Fetching rootfs..." → ...
  - "All artifacts ready. Preparing kexec..." (M4)

**Dependencies**: Task 5.1, M2 Task 4.3 (discovery flow)
**Output**: Updated init script with verification + fetch flow

#### 5.3 Error handling and diagnostics
- [ ] Error scenarios:
  - Manifest signature invalid → "FATAL: Manifest signature verification failed"
  - Artifact hash mismatch → "FATAL: Artifact <name> hash mismatch (expected <hash>)"
  - Fetch failure (all mirrors) → "ERROR: Failed to fetch <artifact> after 3 retries"
  - Rollback detected → "FATAL: Manifest version downgrade detected (possible attack)"
- [ ] Diagnostic information:
  - Display manifest version, channel, arch
  - Display artifact URLs tried
  - Display error logs from phase-verify, phase-fetch
  - Suggest: Check network, verify manifest source, contact support

**Dependencies**: Task 5.2
**Output**: Error handling logic

---
