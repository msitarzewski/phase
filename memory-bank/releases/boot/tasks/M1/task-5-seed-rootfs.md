# Task 5 — Seed Rootfs (SquashFS)


**Agent**: Tooling Agent
**Estimated**: 4 days

#### 5.1 Minimal userspace structure
- [ ] Directory: `boot/rootfs/`
- [ ] Standard FHS directories:
  - `/bin`, `/sbin`, `/lib`, `/lib64`, `/usr`, `/etc`
  - `/var`, `/tmp`, `/home`, `/root`
  - `/proc`, `/sys`, `/dev` (mount points)

**Dependencies**: None
**Output**: Rootfs skeleton

#### 5.2 Install base binaries
- [ ] BusyBox (same as initramfs)
- [ ] Essential tools: mount, umount, modprobe, insmod
- [ ] Network tools: ip, dhcpcd
- [ ] Place in `boot/rootfs/bin/` and `boot/rootfs/sbin/`

**Dependencies**: Task 5.1
**Output**: Binaries installed in rootfs

#### 5.3 Configuration files
- [ ] `/etc/passwd`, `/etc/group` (minimal root user)
- [ ] `/etc/hostname` — "phase-boot"
- [ ] `/etc/resolv.conf` — Placeholder (DHCP will overwrite)
- [ ] `/etc/network/interfaces` — Minimal network config

**Dependencies**: Task 5.1
**Output**: Configuration files

#### 5.4 Build SquashFS
- [ ] Script: `boot/scripts/build-rootfs.sh`
- [ ] Command: `mksquashfs boot/rootfs/ boot/rootfs.sqfs -comp xz -b 1M`
- [ ] Verify size: <300MB
- [ ] Verify read-only mount: `mount -t squashfs -o loop rootfs.sqfs /mnt`

**Dependencies**: Tasks 5.1, 5.2, 5.3
**Output**: SquashFS image, build script

---
