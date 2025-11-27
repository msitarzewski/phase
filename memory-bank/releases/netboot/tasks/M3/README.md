# Milestone M3 — DHT/mDNS Advertisement

**Status**: PLANNED
**Owner**: Networking Agent (primary), Runtime Agent (integration)
**Dependencies**: M2 complete (manifest generation, signing)
**Estimated Effort**: 2 weeks

## Intent Summary

Advertise boot manifests in DHT and via mDNS so Phase Boot clients can discover providers. This is the discoverability layer - without it, clients need to know provider URLs in advance.

---

## Acceptance Criteria

1. **DHT advertisement**: Publish manifest URL at `/phase/{channel}/{arch}/manifest`
2. **mDNS advertisement**: Announce `_phase-image._tcp` service on LAN
3. **Multi-channel**: Support multiple channels (stable, testing) simultaneously
4. **Refresh**: Re-advertise periodically to maintain presence
5. **Discovery test**: phase-discover can find provider via DHT and mDNS

## DHT Key Scheme

```
Key:   /phase/{channel}/{arch}/manifest
Value: JSON with manifest URL and provider info

Example:
Key:   /phase/stable/arm64/manifest
Value: {
  "url": "http://192.168.1.100:8080/stable/arm64/manifest.json",
  "peer_id": "12D3KooW...",
  "provider_name": "my-mac",
  "last_updated": "2025-11-26T12:00:00Z"
}
```

## mDNS Service

```
Service: _phase-image._tcp.local
TXT Records:
  arch=arm64
  channel=stable
  manifest=http://192.168.1.100:8080/stable/arm64/manifest.json
  version=2025.11.26
```

---

## Tasks

1. [DHT Record Publishing](task-1-dht-publish.md) — Publish manifest to Kademlia DHT
2. [DHT Refresh & TTL](task-2-dht-refresh.md) — Maintain presence in DHT
3. [mDNS Advertisement](task-3-mdns-advertise.md) — Announce on local network
4. [Multi-Channel Support](task-4-multi-channel.md) — Advertise multiple channels
5. [Testing & Validation](task-5-testing.md) — Verify with phase-discover

---

## File Changes

### New Files
```
daemon/src/provider/
├── dht.rs              # DHT advertisement
└── mdns.rs             # mDNS advertisement
```

### Modified Files
```
daemon/src/provider/mod.rs      # Export dht, mdns modules
daemon/src/provider/server.rs   # Start advertisement on launch
daemon/src/network/discovery.rs # Add provider DHT methods
```

---

## Integration with Existing Discovery

plasmd already has libp2p/Kademlia for WASM job discovery. We'll extend it:

```rust
// Existing: advertise WASM capability
kademlia.start_providing(Key::new("/phase/capability/arm64/wasmtime"))?;

// New: advertise boot manifest
kademlia.put_record(
    Key::new("/phase/stable/arm64/manifest"),
    manifest_url_json.into_bytes(),
)?;
```

---

## Discovery Flow (Client Perspective)

```
1. Client boots Phase Boot USB
2. Network comes up (DHCP)
3. phase-discover runs:
   a. Try mDNS: query _phase-image._tcp.local
   b. If found: use LAN provider
   c. Else: query DHT for /phase/{channel}/{arch}/manifest
   d. Get manifest URL from DHT record
4. phase-fetch downloads manifest from URL
5. phase-verify checks signature
6. phase-fetch downloads artifacts
7. kexec-boot loads new kernel
```
