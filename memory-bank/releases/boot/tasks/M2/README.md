# Milestone M2 — Network Bring-up & Discovery

**Status**: 🔵 PLANNED
**Owner**: Networking Agent (primary), Tooling Agent (initramfs integration)
**Dependencies**: M1 complete (initramfs structure, network tools installed)
**Estimated Effort**: 4-5 weeks

## Intent Summary
Bring up network (wired and Wi-Fi) in initramfs environment and discover boot manifests via mDNS (LAN) and libp2p Kademlia DHT (WAN). Enable Internet, Local, and Private modes to select appropriate discovery mechanisms.

---

## Acceptance Highlights
1. **Wired network**: Automatically acquires DHCP lease on boot
2. **Wi-Fi network**: Optional manual configuration via prompt (Internet/Local modes)
3. **LAN discovery (mDNS)**: Finds local provider advertising `_phase-image._tcp` service
4. **WAN discovery (DHT)**: Queries libp2p Kademlia for `phase://stable/<arch>` manifest
5. **Manifest retrieval**: Successfully fetches manifest JSON from discovered provider
6. **Mode compliance**:
   - Internet Mode: DHT discovery, public identity
   - Local Mode: mDNS discovery only, no WAN access
   - Private Mode: DHT discovery, ephemeral identity, no persistent cache

## Tasks
1. [Network Bring-up Scripts](task-1-network-bring.md)
2. [mDNS Discovery (Local Mode)](task-2-mdns-discovery.md)
3. [libp2p DHT Discovery (Internet/Private Modes)](task-3-libp2p-dht.md)
4. [Manifest Schema & Retrieval](task-4-manifest-schema.md)
5. [Mode Policy Enforcement](task-5-mode-policy.md)
6. [Testing & Validation](task-6-testing-and.md)
