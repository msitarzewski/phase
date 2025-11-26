# Task 4 — Initramfs Construction


**Agent**: Tooling Agent
**Estimated**: 6 days

#### 4.1 Base initramfs structure
- [ ] Directory: `boot/initramfs/`
- [ ] Subdirectories:
  - `bin/` — Essential binaries
  - `sbin/` — System binaries
  - `etc/` — Configuration files
  - `dev/` — Device nodes (created by init)
  - `proc/`, `sys/`, `run/` — Mount points
  - `usr/` — Shared libraries, data
  - `scripts/` — Boot scripts

**Dependencies**: None
**Output**: Initramfs skeleton

#### 4.2 BusyBox integration
- [ ] Obtain BusyBox static binary (or build)
- [ ] Install to `boot/initramfs/bin/busybox`
- [ ] Create symlinks for common tools:
  - sh, ash, ls, cat, cp, mv, rm, mount, umount
  - ip, ping, route, ifconfig, dhcpcd
  - grep, awk, sed, tar, gzip
- [ ] Script: `boot/scripts/install-busybox.sh`

**Dependencies**: Task 4.1
**Output**: BusyBox installed with symlinks

#### 4.3 Network tools
- [ ] Install `iproute2` binaries: ip, ss
- [ ] Install `dhcpcd` (DHCP client)
- [ ] Install `wpa_supplicant` (Wi-Fi, optional for M1)
- [ ] Place in `boot/initramfs/sbin/`

**Dependencies**: Task 4.1
**Output**: Network binaries installed

#### 4.4 kexec-tools
- [ ] Build or obtain `kexec` static binary for both architectures
- [ ] Install to `boot/initramfs/sbin/kexec`
- [ ] Verify compatibility with kernel configs (Task 3.1, 3.2)

**Dependencies**: Task 4.1
**Output**: kexec binary installed

#### 4.5 Init script
- [ ] File: `boot/initramfs/init`
- [ ] Responsibilities:
  - Mount `/proc`, `/sys`, `/dev` (devtmpfs)
  - Parse kernel cmdline (`phase.mode=...`)
  - Bring up network (basic DHCP via dhcpcd)
  - Drop to shell for M1 (placeholder for M2 discovery)
  - Exec /bin/sh as PID 1 (busybox ash)
- [ ] Make executable: `chmod +x boot/initramfs/init`

**Dependencies**: Tasks 4.2, 4.3, 4.4
**Output**: Functional init script

#### 4.6 Build initramfs images
- [ ] Script: `boot/scripts/build-initramfs.sh`
- [ ] Arguments: `--arch [x86_64|arm64]`
- [ ] Steps:
  - Create CPIO archive: `find . | cpio -o -H newc`
  - Compress with gzip: `gzip -9`
  - Output to ESP: `boot/esp/initramfs-{arch}.img`
- [ ] Verify size: <50MB compressed

**Dependencies**: All Task 4 items
**Output**: Initramfs build script, image files

---
