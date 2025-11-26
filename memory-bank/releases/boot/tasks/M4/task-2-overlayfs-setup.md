# Task 2 — OverlayFS Setup


**Agent**: Systems Agent
**Estimated**: 4 days

#### 2.1 OverlayFS design
- [ ] Layer structure:
  - **Lower layer** (read-only): SquashFS rootfs from `/tmp/rootfs`
  - **Upper layer** (read-write): tmpfs OR cache partition
  - **Work directory**: tmpfs (required by OverlayFS)
  - **Merged view**: `/mnt/target` (final root for target OS)
- [ ] Mode policies:
  - **Internet/Local**: Upper layer on cache partition (persistent)
  - **Private**: Upper layer on tmpfs (ephemeral, RAM-only)

**Dependencies**: None
**Output**: OverlayFS design documentation

#### 2.2 Mount SquashFS (lower layer)
- [ ] Script: `boot/initramfs/scripts/mount-overlay.sh`
- [ ] Steps:
  - Create mount point: `mkdir -p /mnt/lower`
  - Mount SquashFS: `mount -t squashfs -o loop /tmp/rootfs /mnt/lower`
  - Verify mount: Check `/mnt/lower` has expected files (e.g., `/bin`, `/etc`)
- [ ] Error handling: Mount failure → abort kexec

**Dependencies**: Task 2.1, M3 Task 5.1 (rootfs ready)
**Output**: SquashFS mount logic

#### 2.3 Prepare upper layer (mode-dependent)
- [ ] **Internet/Local Mode**:
  - Mount cache partition: `mount /dev/disk/by-label/PHASE-CACHE /cache` (if not already)
  - Create upper directory: `mkdir -p /cache/overlay/upper`
  - Create work directory: `mkdir -p /cache/overlay/work`
- [ ] **Private Mode**:
  - Create tmpfs upper: `mount -t tmpfs tmpfs /mnt/upper-tmp`
  - Create work directory: `mount -t tmpfs tmpfs /mnt/work-tmp`
- [ ] Verify: Directories exist and writable

**Dependencies**: Task 2.2, M2 Task 5.1 (mode parsing)
**Output**: Upper layer setup logic

#### 2.4 Mount OverlayFS (merged view)
- [ ] Steps:
  - Create merge point: `mkdir -p /mnt/target`
  - Mount OverlayFS:
    - Internet/Local: `mount -t overlay overlay -o lowerdir=/mnt/lower,upperdir=/cache/overlay/upper,workdir=/cache/overlay/work /mnt/target`
    - Private: `mount -t overlay overlay -o lowerdir=/mnt/lower,upperdir=/mnt/upper-tmp,workdir=/mnt/work-tmp /mnt/target`
  - Verify: `/mnt/target` has merged view (read-only + read-write)
- [ ] Error handling: Mount failure → abort kexec

**Dependencies**: Tasks 2.2, 2.3
**Output**: OverlayFS mount logic

#### 2.5 Test OverlayFS
- [ ] Internet/Local Mode test:
  - Write file to `/mnt/target/test`
  - Verify file persists in `/cache/overlay/upper/test`
  - Reboot, verify file still exists (persistent)
- [ ] Private Mode test:
  - Write file to `/mnt/target/test`
  - Verify file in tmpfs (RAM)
  - Reboot, verify file does NOT exist (ephemeral)
- [ ] Read-only test:
  - Verify files from lower layer (SquashFS) readable
  - Attempt to modify lower layer file → should write to upper layer

**Dependencies**: Task 2.4
**Output**: OverlayFS test results

---
