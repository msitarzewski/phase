# M4 — kexec Handoff & Modes

**Objective**  
Load verified artifacts and `kexec` into target kernel; wire mode policies (Internet/Local/Private).

**Deliverables**  
- `fetch-verify-kexec` orchestrator.  
- OverlayFS mounting rules per mode.  
- Private mode: no persistent writes; ephemeral identity; optional Tor toggle.

**Acceptance Criteria**  
- Successful kexec on at least 1 x86_64 NUC/server and 1 ARM64 board.  
- Private mode leaves no writes on cache partition.

**Tasks**  
- [ ] Orchestrator tying discovery → verify → fetch → kexec.  
- [ ] Kernel cmdline builder (rootfs + overlay options).  
- [ ] Mode policy guardrails (disallow writes in Private).  
- [ ] Failure fallback to seed shell with diagnostics.

**Risks**  
- kexec blocked by firmware/kernel params → provide bootloader chainload fallback entries.
