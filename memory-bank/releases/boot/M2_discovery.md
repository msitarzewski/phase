# M2 — Network Bring-up & Discovery (mDNS + DHT)

**Objective**  
Bring up network (wired/Wi-Fi) in initramfs and discover manifests via mDNS (LAN) and DHT (WAN).

**Deliverables**  
- Network bring-up scripts with retry/backoff.  
- mDNS service query: `_phase-image._tcp` TXT includes manifest pointer (CID/URL).  
- libp2p (Kademlia) client to query latest channel manifest (`phase://stable/<arch>`).

**Acceptance Criteria**  
- LAN-only test: finds a local provider and retrieves manifest JSON.  
- WAN test: finds providers via DHT and retrieves same manifest.

**Tasks**  
- [ ] Wi-Fi config helper (optional) + CLI prompts.  
- [ ] Avahi/mDNS discovery shim.  
- [ ] libp2p/Kademlia static client build (initramfs-friendly).  
- [ ] Channel mapping: `stable`, `testing` → manifest IDs.

**Risks**  
- Static linking size → trim binaries; use busybox where possible.  
- Captive portals → document manual fallback URL input.
