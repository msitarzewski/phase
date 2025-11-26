# Task 6 — Testing & Validation


**Agent**: Systems Agent, Kernel Agent
**Estimated**: 6 days

#### 6.1 QEMU x86_64 kexec test
- [ ] Setup:
  - Boot Phase USB in QEMU
  - Internet Mode
  - Network bridge configured
- [ ] Validation:
  - Seed initramfs boots
  - Discovery, verification, fetch successful (M2, M3)
  - OverlayFS mounts
  - kexec loads kernel
  - kexec exec → target kernel boots
  - Target OS console accessible
  - Verify mode metadata: `cat /etc/phase-mode`

**Dependencies**: All M4 tasks, M1-M3 complete
**Output**: QEMU x86_64 kexec test results

#### 6.2 QEMU ARM64 kexec test
- [ ] Setup: Same as 6.1, but QEMU ARM64
- [ ] Additional validation:
  - DTB loaded correctly
  - Target kernel boots on ARM64 architecture

**Dependencies**: All M4 tasks, M1-M3 complete
**Output**: QEMU ARM64 kexec test results

#### 6.3 Physical x86_64 kexec test
- [ ] Hardware: Intel NUC or generic x86_64 PC
- [ ] Validation: Same as 6.1
- [ ] Check: kexec not blocked by firmware settings (Secure Boot disabled for M4)

**Dependencies**: All M4 tasks
**Output**: Physical x86_64 kexec test results

#### 6.4 Physical ARM64 kexec test
- [ ] Hardware: Raspberry Pi 4 with UEFI firmware
- [ ] Validation: Same as 6.2
- [ ] Check: DTB compatibility, kexec firmware support

**Dependencies**: All M4 tasks
**Output**: Physical ARM64 kexec test results

#### 6.5 Mode-specific tests
- [ ] **Internet Mode**:
  - Overlay upper on cache partition
  - Write test file, reboot, verify persistence
  - Network unrestricted
- [ ] **Local Mode**:
  - Overlay upper on cache partition
  - Network restricted to LAN (verify routes)
- [ ] **Private Mode**:
  - Overlay upper on tmpfs
  - Write test file, reboot, verify NO persistence
  - Mode metadata includes `nowrite=true`

**Dependencies**: Tasks 2.5, 5.2
**Output**: Mode-specific test results

#### 6.6 Fallback tests
- [ ] Simulate kexec load failure:
  - Corrupt kernel file
  - Validation: Abort, display error, drop to seed shell
- [ ] Simulate kexec exec failure:
  - kexec disabled in kernel (simulate via config)
  - Validation: Abort, display error, drop to seed shell
- [ ] Test diagnostic output:
  - Verify error messages clear and actionable
  - Check memory, cmdline, kexec status displayed

**Dependencies**: Task 4.4
**Output**: Fallback test results

---
