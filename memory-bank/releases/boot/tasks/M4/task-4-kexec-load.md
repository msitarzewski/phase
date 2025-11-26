# Task 4 — kexec Load & Execute


**Agent**: Systems Agent
**Estimated**: 4 days

#### 4.1 kexec load command (x86_64)
- [ ] Command: `kexec --load /tmp/kernel --initrd=/tmp/initramfs --append="$(cat /tmp/kernel-cmdline)"`
- [ ] Parameters:
  - `--load`: Load kernel into memory without executing
  - `--initrd`: Specify initramfs
  - `--append`: Kernel cmdline from Task 3.2
- [ ] Verification: `kexec --load` returns exit code 0

**Dependencies**: Tasks 1.2, 3.2
**Output**: kexec load command for x86_64

#### 4.2 kexec load command (ARM64)
- [ ] Command: `kexec --load /tmp/kernel --initrd=/tmp/initramfs --dtb=/boot/dtbs/<board>.dtb --append="$(cat /tmp/kernel-cmdline)"`
- [ ] Additional parameter:
  - `--dtb`: Device Tree Blob for ARM64 board
- [ ] DTB selection:
  - Detect board: Parse `/proc/device-tree/model` or `/proc/cpuinfo`
  - Match to DTB: E.g., Raspberry Pi 4 → `bcm2711-rpi-4-b.dtb`
  - Load from: `/boot/dtbs/<dtb>` (copied from M1 ESP)
- [ ] Fallback: If DTB detection fails, use generic or abort with error

**Dependencies**: Tasks 1.2, 3.2, M1 Task 3.2 (DTBs)
**Output**: kexec load command for ARM64

#### 4.3 kexec execute
- [ ] Command: `kexec --exec`
- [ ] Behavior: Immediately hands off to loaded kernel (point of no return)
- [ ] Pre-exec:
  - Sync filesystems: `sync`
  - Unmount non-essential mounts (optional, kexec may handle)
  - Print final message: "Handing off to target OS..."
- [ ] Post-exec: This line never reached (control transferred to new kernel)

**Dependencies**: Tasks 4.1, 4.2
**Output**: kexec exec command

#### 4.4 Error handling and fallback
- [ ] kexec load failures:
  - Exit code non-zero → Display error: "kexec load failed: <reason>"
  - Common reasons: Kernel corrupt, insufficient memory, kexec disabled in kernel
  - Action: Abort, drop to seed shell
- [ ] kexec exec failures:
  - Rare (load validates most issues), but possible (firmware issues)
  - Fallback: If exec returns (shouldn't happen), display error, drop to shell
- [ ] Diagnostic information:
  - Kernel path, initramfs path, cmdline
  - Available memory: `free -m`
  - kexec kernel config: `cat /proc/sys/kernel/kexec_load_disabled` (should be 0)

**Dependencies**: Tasks 4.1-4.3
**Output**: Error handling and fallback logic

---
