# Task 7 — Testing & Validation


**Agent**: Systems Agent, Kernel Agent
**Estimated**: 5 days

#### 7.1 QEMU x86_64 test
- [ ] Script: `boot/scripts/test-qemu-x86.sh`
- [ ] Command: `qemu-system-x86_64 -bios /usr/share/ovmf/OVMF.fd -drive file=phase-boot.img,format=raw -m 2G -enable-kvm`
- [ ] Validation:
  - Boot menu appears with 3 entries
  - Internet Mode boots to initramfs shell
  - Network interface visible (`ip link`)
  - kexec binary present (`which kexec`)

**Dependencies**: Task 6.1
**Output**: QEMU test script, test results documented

#### 7.2 QEMU ARM64 test
- [ ] Script: `boot/scripts/test-qemu-arm.sh`
- [ ] Command: `qemu-system-aarch64 -M virt -cpu cortex-a57 -bios /usr/share/qemu-efi-aarch64/QEMU_EFI.fd -drive file=phase-boot.img,format=raw -m 2G`
- [ ] Validation: Same as 7.1 for ARM64 entry

**Dependencies**: Task 6.1
**Output**: QEMU test script, test results

#### 7.3 Physical hardware test: x86_64
- [ ] Hardware: Intel NUC or generic x86_64 PC
- [ ] Write USB image to physical media (Task 6.4)
- [ ] Boot from USB (UEFI settings: disable Secure Boot initially)
- [ ] Validation: Boot menu, initramfs shell, network functional

**Dependencies**: Task 6.4
**Output**: Hardware compatibility report (`boot/docs/tested-hardware.md`)

#### 7.4 Physical hardware test: ARM64
- [ ] Hardware: Raspberry Pi 4 or generic ARM64 board with UEFI firmware
- [ ] Write USB image or SD card
- [ ] Boot from media
- [ ] Validation: Same as 7.3

**Dependencies**: Task 6.4
**Output**: ARM64 hardware compatibility report

#### 7.5 Mode testing
- [ ] Test each boot entry:
  - Internet Mode: Verify `phase.mode=internet` in `/proc/cmdline`
  - Local Mode: Verify `phase.mode=local`
  - Private Mode: Verify `phase.mode=private phase.nowrite=true`
- [ ] Ensure mode parameters passed correctly to init script

**Dependencies**: Tasks 7.1-7.4
**Output**: Mode testing report

---
