# M3 — Verification & Fetch Pipeline

**Objective**  
Verify signed manifest (TUF-like) and fetch kernel/initramfs/rootfs by content hash; CAS cache (disabled in Private mode).

**Deliverables**  
- Manifest schema: `{version, artifacts:{kernel,initramfs,rootfs}, signatures[], channel}`.  
- Ed25519 signature verification with offline root/targets keys.  
- Fetchers: HTTPS mirror first, IPFS gateway second (BitTorrent later).  
- CAS cache on cache partition (skipped in Private mode).

**Acceptance Criteria**  
- Tamper test: wrong hash/signature → boot aborts with clear error.  
- Cache hit/miss behavior correct across modes.

**Tasks**  
- [ ] `phase-verify` (manifest+hash checks).  
- [ ] `phase-fetch` (by CID/hash with retry/mirror fallback).  
- [ ] CAS layout + GC policy; disable in Private mode.  
- [ ] `policy.toml` (channels, min_version, cache toggle).

**Risks**  
- Key compromise → document rotation; store roots offline; optional transparency log in M7.
