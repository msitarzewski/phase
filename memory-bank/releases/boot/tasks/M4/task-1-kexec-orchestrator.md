# Task 1 — kexec Orchestrator


**Agent**: Systems Agent
**Estimated**: 5 days

#### 1.1 Orchestrator script: kexec-orchestrator.sh
- [ ] Script: `boot/initramfs/scripts/kexec-orchestrator.sh`
- [ ] Inputs:
  - Mode: `$PHASE_MODE` (from M2 Task 5.1)
  - Artifact paths: `/tmp/kernel`, `/tmp/initramfs`, `/tmp/rootfs` (from M3)
  - Architecture: Detected from `uname -m`
- [ ] Steps:
  1. Verify artifacts exist and readable
  2. Set up OverlayFS based on mode (Task 2)
  3. Build kernel cmdline (Task 3)
  4. Load kernel with kexec (Task 4)
  5. Exec kexec (handoff to target kernel)
- [ ] Error handling: Any step failure → abort, display error, drop to shell

**Dependencies**: M3 Task 5.1 (verified artifacts ready)
**Output**: Orchestrator script skeleton

#### 1.2 Pre-kexec validation
- [ ] Checks before kexec:
  - Kernel file exists and is ELF binary (verify magic bytes)
  - Initramfs file exists and is gzip/cpio archive
  - Rootfs file exists and is SquashFS
  - Sufficient free RAM: At least 500MB free (for kernel + initramfs decompression)
- [ ] Validation failures:
  - Display specific error (e.g., "Kernel file corrupt")
  - Abort kexec, drop to shell

**Dependencies**: Task 1.1
**Output**: Pre-kexec validation logic

#### 1.3 Integrate into init script
- [ ] Update: `boot/initramfs/init`
- [ ] Flow:
  - Network up (M1, M2)
  - Discover manifest (M2)
  - Verify + Fetch artifacts (M3)
  - **NEW**: Execute kexec orchestrator (`kexec-orchestrator.sh`)
  - On kexec failure: Display error, drop to shell
- [ ] Console output:
  - "Preparing kexec..."
  - "Setting up overlay filesystem..."
  - "Loading kernel..."
  - "Handing off to target OS..." (last message before kexec)

**Dependencies**: Tasks 1.1, 1.2, M3 Task 5.2
**Output**: Updated init script with kexec orchestration

---
