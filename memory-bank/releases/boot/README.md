# Release: Phase Boot (USB → Discover → Verify → Fetch → kexec)

**Scope:** Bootable USB for x86_64 + ARM64 (generic UEFI). Apple Silicon via VM only.  
**Outcome:** Minimal seed OS that discovers a signed manifest over DHT/mDNS, fetches kernel+initramfs+rootfs by content hash, verifies, then `kexec`s into the target image. Modes: Internet, Local, Private.

## Milestones
- M1 — Boot Stub & Media Layout
- M2 — Network Bring-up & Discovery (mDNS + DHT)
- M3 — Verification & Fetch Pipeline (TUF-like, CAS cache)
- M4 — kexec Handoff & Modes (Internet/Local/Private)
- M5 — Packaging & VM Images (x86_64/arm64 USB, QCOW2/Parallels)
- M6 — Phase/Plasma Hello Job Path (post-boot WASM)
- M7 — Docs, Threat Model, and Secure Boot Options

**Release Owner:** Michael S.  
**Contributors:** Runtime (Plasm), Networking, Security/Signing, Docs.

## Definition of Done
- USB boots on x86_64 & ARM64 UEFI hardware, and inside Parallels/UTM/QEMU (for Apple Silicon).  
- Internet Mode: discovers manifest via DHT, downloads artifacts (HTTP/IPFS), verifies signatures+hashes, boots via kexec.  
- Local Mode: uses cache or LAN peers (mDNS).  
- Private Mode: no persistent writes; optional Tor; ephemeral identity.  
- Minimal WASM “hello job” runs post-boot through Plasm.
