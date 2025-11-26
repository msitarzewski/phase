# Milestone M6 — Phase/Plasma Hello Job Path

**Status**: 🔵 PLANNED
**Owner**: Runtime Agent (primary), Networking Agent (post-boot network)
**Dependencies**: M1-M5 complete (bootable images with kexec), existing Plasm daemon from Phase MVP
**Estimated Effort**: 2-3 weeks

## Intent Summary
Prove end-to-end Phase Boot → Plasm execution by running a WASM "hello job" after kexec into target OS. Integrate Plasm daemon from existing `daemon/` codebase into boot rootfs, start as post-boot service, fetch WASM job by CID from network, execute, and display receipt.

---

## Acceptance Highlights
1. **Plasm daemon integrated**: Binary installed in target rootfs, starts post-boot
2. **Systemd service functional**: `plasm.service` starts automatically, restartable
3. **WASM job published**: `hello.wasm` + manifest available via DHT/IPFS
4. **Job execution successful**: Hello job runs post-boot, outputs "dlroW ,olleH"
5. **Receipt generated**: Ed25519-signed receipt printed to console
6. **Mode compliance**:
   - Internet/Local: Receipt logged to `/var/log/plasm/receipts/`
   - Private Mode: Receipt printed to console only (no persistent log)
7. **Non-blocking boot**: Boot succeeds even if Plasm or job fails

## Tasks
1. [Integrate Plasm Daemon](task-1-integrate-plasm.md)
2. [Systemd Service](task-2-systemd-service.md)
3. [WASM Example Publication](task-3-wasm-example.md)
4. [Post-Boot Job Execution](task-4-post-boot.md)
5. [Mode Policy Enforcement](task-5-mode-policy.md)
6. [Testing & Validation](task-6-testing-and.md)
