# Task 3 — Kernel Cmdline Builder


**Agent**: Kernel Agent
**Estimated**: 3 days

#### 3.1 Define cmdline parameters
- [ ] Document: `boot/docs/kernel-cmdline.md`
- [ ] Required parameters:
  - `root=/dev/ram0` OR `root=overlay:/mnt/target` (OverlayFS root)
  - `init=/sbin/init` (systemd or busybox init)
  - `ro` (read-only initial mount)
  - `console=tty0 console=ttyS0,115200n8` (console output)
- [ ] Mode-specific parameters:
  - Internet Mode: `phase.mode=internet phase.channel=stable`
  - Local Mode: `phase.mode=local phase.network=lan`
  - Private Mode: `phase.mode=private phase.nowrite=true`
- [ ] Optional parameters:
  - `quiet` (suppress verbose kernel logs)
  - `loglevel=3` (warnings and errors only)
  - `crashkernel=auto` (optional, for kdump)

**Dependencies**: None
**Output**: Cmdline parameter documentation

#### 3.2 Cmdline builder script
- [ ] Script: `boot/initramfs/scripts/build-cmdline.sh`
- [ ] Inputs:
  - Mode: `$PHASE_MODE`
  - Channel: `$PHASE_CHANNEL`
  - Architecture: Detected from `uname -m`
- [ ] Logic:
  - Start with base cmdline: `root=overlay:/mnt/target init=/sbin/init ro console=tty0 console=ttyS0`
  - Append mode parameters: `phase.mode=$PHASE_MODE phase.channel=$PHASE_CHANNEL`
  - Private Mode: Add `phase.nowrite=true`
  - ARM64: Add `dtb=<path>` if needed
- [ ] Output: String written to `/tmp/kernel-cmdline`

**Dependencies**: Task 3.1, M2 Task 5.1 (mode parsing)
**Output**: Cmdline builder script

#### 3.3 Test cmdline generation
- [ ] Test: Internet Mode
  - Expected: `root=overlay:/mnt/target init=/sbin/init ro phase.mode=internet phase.channel=stable`
- [ ] Test: Local Mode
  - Expected: `root=overlay:/mnt/target init=/sbin/init ro phase.mode=local phase.channel=stable`
- [ ] Test: Private Mode
  - Expected: `root=overlay:/mnt/target init=/sbin/init ro phase.mode=private phase.nowrite=true`

**Dependencies**: Task 3.2
**Output**: Cmdline generation test results

---
