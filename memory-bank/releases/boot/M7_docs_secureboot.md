# M7 — Docs, Threat Model, and Secure Boot

**Objective**  
Ship clear docs, a pragmatic threat model, and two Secure Boot options.

**Deliverables**  
- Quickstarts: bare-metal x86_64/ARM64; Apple Silicon via VM.  
- Threat model: tamper, rollback, targeted poisoning, privacy limits.  
- Secure Boot: shim path (MS-signed) and owner-enrolled keys.  
- Troubleshooting guide.

**Acceptance Criteria**  
- A new user can build/write the USB, boot, and run the hello job without assistance.  
- Security doc explains key rotation and (optional) transparency log.

**Tasks**  
- [ ] Quickstart + diagrams for boot flow.  
- [ ] Secure Boot key paths and scripts.  
- [ ] Troubleshooting (kexec, drivers, Wi-Fi, captive portals).  
- [ ] Transparency log notes (planned).

**Risks**  
- User confusion on Secure Boot → include explicit detection and instructions to disable or enroll keys.
