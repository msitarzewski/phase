# Task 5 — Mode Policy Enforcement


**Agent**: Tooling Agent
**Estimated**: 3 days

#### 5.1 Validate mode policies at kexec
- [ ] Pre-kexec checks:
  - **Internet Mode**: No restrictions
  - **Local Mode**: Verify network routes only to LAN (no default gateway to WAN)
  - **Private Mode**: Verify upper layer is tmpfs (not cache partition)
- [ ] Validation failures:
  - Local Mode: WAN route detected → Warn or abort (configurable)
  - Private Mode: Cache partition mounted → Abort (security violation)

**Dependencies**: Task 2.3 (overlay setup), M2 Task 5.2 (mode selection)
**Output**: Mode policy validation logic

#### 5.2 Private Mode: Verify no persistent writes
- [ ] Post-overlay check:
  - Ensure `/mnt/target` upper layer is tmpfs
  - Write test file: `touch /mnt/target/phase-test`
  - Verify file does NOT appear on cache partition
  - Remove test file: `rm /mnt/target/phase-test`
- [ ] Fail-safe: If persistent write detected, abort kexec with error

**Dependencies**: Task 2.5 (overlay test), Task 5.1
**Output**: Private Mode write verification

#### 5.3 Mode metadata for target OS
- [ ] Create metadata file: `/mnt/target/etc/phase-mode`
- [ ] Content:
  ```
  PHASE_MODE=<internet|local|private>
  PHASE_CHANNEL=<stable|testing>
  PHASE_VERSION=<manifest version>
  PHASE_BOOT_TIME=<timestamp>
  ```
- [ ] Purpose: Target OS can read mode and adjust services accordingly
  - Private Mode: Disable logging, networking services, telemetry
  - Local Mode: Restrict networking to LAN interfaces

**Dependencies**: Task 1.1, M2 Task 5.1
**Output**: Mode metadata file creation

---
