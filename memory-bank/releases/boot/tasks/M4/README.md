# Milestone M4 — kexec Handoff & Modes

**Status**: 🔵 PLANNED
**Owner**: Systems Agent (primary), Kernel Agent (kexec kernel params)
**Dependencies**: M1 (kexec-tools), M2 (discovery), M3 (verified artifacts)
**Estimated Effort**: 3-4 weeks

## Intent Summary
Load verified kernel, initramfs, and rootfs artifacts into memory and execute `kexec` to hand off boot process to target OS. Wire mode policies (Internet/Local/Private) to control overlay filesystem, cache writes, and ephemeral identity guarantees.

---

## Acceptance Highlights
1. **kexec successful**: Target kernel boots after hand off from seed initramfs
2. **Verified artifacts**: Only M3-verified kernel/initramfs/rootfs used
3. **OverlayFS functional**: Layered filesystem mounts correctly per mode
4. **Mode policies enforced**:
   - Internet Mode: OverlayFS with cache partition (persistent writes)
   - Local Mode: Same as Internet, but network restricted to LAN
   - Private Mode: OverlayFS with tmpfs (no persistent writes)
5. **Fallback on failure**: Seed shell with diagnostics if kexec fails
6. **Hardware compatibility**: kexec works on tested x86_64 and ARM64 devices

## Tasks
1. [kexec Orchestrator](task-1-kexec-orchestrator.md)
2. [OverlayFS Setup](task-2-overlayfs-setup.md)
3. [Kernel Cmdline Builder](task-3-kernel-cmdline.md)
4. [kexec Load & Execute](task-4-kexec-load.md)
5. [Mode Policy Enforcement](task-5-mode-policy.md)
6. [Testing & Validation](task-6-testing-and.md)
