# Phase Boot Provider Architecture

## Overview

Phase Boot Provider is a distributed network boot system that enables machines to discover and download boot artifacts (kernel, initramfs) over libp2p. The system uses a combination of HTTP for artifact distribution, DHT for global discovery, and mDNS for local network discovery.

### Key Concepts

- **Provider**: HTTP server that serves boot artifacts and manifests
- **Manifest**: JSON document describing available artifacts with cryptographic signatures
- **Discovery**: Multi-tier system for finding providers (local mDNS, global DHT)
- **Self-hosting Loop**: Providers can serve the artifacts needed to boot themselves

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Phase Boot Provider System                       │
└─────────────────────────────────────────────────────────────────────┘

  Boot Client                    Provider Server                 Network
  ┌──────────┐                  ┌────────────────┐           ┌──────────┐
  │          │                  │                │           │          │
  │  UEFI/   │                  │  HTTP Server   │◄──────────│  Client  │
  │  iPXE    │                  │  :8080         │   HTTP    │          │
  │          │                  │                │           └──────────┘
  │          │                  │  ┌──────────┐  │
  │          │                  │  │Manifest  │  │
  │  phase-  │◄─────DHT─────────┤  │Generator │  │
  │ discover │    Lookup        │  └──────────┘  │
  │          │                  │                │           ┌──────────┐
  │          │                  │  ┌──────────┐  │           │  mDNS    │
  └──────────┘                  │  │Artifact  │  │◄──────────│ Browser  │
                                │  │Store     │  │  Local    │          │
                                │  └──────────┘  │ Discovery └──────────┘
                                │                │
                                │  ┌──────────┐  │
                                │  │DHT/mDNS  │  │
                                │  │Advertiser│  │
                                │  └──────────┘  │
                                └────────────────┘
                                        │
                                        │ libp2p
                                        ▼
                                ┌────────────────┐
                                │   Kademlia     │
                                │      DHT       │
                                └────────────────┘
```

## Component Overview

### HTTP Provider Server

**Location**: `src/provider/server.rs`

The HTTP server provides REST endpoints for:
- Provider information and health checks
- Boot manifest retrieval (JSON)
- Artifact downloads (kernel, initramfs, etc.)
- Range request support for resumable downloads

**Key Features**:
- Streaming file downloads with HTTP Range support
- Artifact integrity via SHA256 hashes in headers
- Health monitoring and metrics collection
- Dynamic manifest generation

### Manifest System

**Location**: `src/provider/manifest.rs`

The manifest is a signed JSON document that describes available boot artifacts:

```json
{
  "manifest_version": 1,
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "created_at": "2025-01-01T00:00:00Z",
  "expires_at": "2025-01-31T00:00:00Z",
  "artifacts": {
    "kernel": {
      "filename": "vmlinuz",
      "size_bytes": 8388608,
      "hash": "sha256:abc123...",
      "download_url": "stable/x86_64/vmlinuz"
    },
    "initramfs": {
      "filename": "initramfs.img",
      "size_bytes": 67108864,
      "hash": "sha256:def456...",
      "download_url": "stable/x86_64/initramfs.img"
    }
  },
  "signatures": [
    {
      "algorithm": "ed25519",
      "key_id": "abc123...",
      "signature": "def456...",
      "signed_at": "2025-01-01T00:00:00Z"
    }
  ]
}
```

**Validation**:
- Required artifacts: `kernel` (minimum)
- Hash format: `algorithm:hexdigest`
- Timestamp format: ISO 8601
- Signature verification via Ed25519

### Artifact Store

**Location**: `src/provider/artifacts.rs`

Manages local artifact storage and retrieval:

```
artifacts/
└── stable/
    ├── x86_64/
    │   ├── vmlinuz
    │   ├── initramfs.img
    │   └── rootfs.squashfs
    └── arm64/
        ├── Image
        ├── initramfs.img
        └── bcm2711-rpi-4-b.dtb
```

**Responsibilities**:
- Scan artifact directories
- Compute SHA256 hashes
- Metadata caching
- File serving integration

### Discovery System

#### 1. mDNS/DNS-SD (Local Network)

**Location**: `src/provider/mdns.rs`

Advertises the provider on the local network using DNS Service Discovery:

**Service Type**: `_phase-image._tcp.local.`

**TXT Records**:
- `channel=stable`
- `arch=x86_64`
- `version=0.1.0`
- `http_port=8080`

**Client Discovery**:
```bash
# Avahi (Linux)
avahi-browse _phase-image._tcp

# dns-sd (macOS)
dns-sd -B _phase-image._tcp
```

**Current Status**: Placeholder implementation (requires `mdns-sd` crate)

#### 2. Kademlia DHT (Global Network)

**Location**: `src/provider/dht.rs`, `src/bin/phase_discover.rs`

Advertises manifest URLs in the global libp2p DHT for discovery from anywhere:

**DHT Key Format**: `/phase/{channel}/{arch}/manifest`

**Record Structure**:
```json
{
  "channel": "stable",
  "arch": "x86_64",
  "manifest_url": "http://192.168.1.100:8080/stable/x86_64/manifest.json",
  "http_addr": "192.168.1.100:8080",
  "manifest_version": "0.1.0",
  "created_at": "2025-01-01T00:00:00Z",
  "ttl_secs": 3600
}
```

**Refresh**: Records are refreshed every 30 minutes (half of TTL)

## Discovery Flow

### Boot-time Discovery (phase-discover)

```
1. Client boots, needs artifacts for channel=stable, arch=x86_64

2. Generate ephemeral identity (Privacy Mode)
   └─> Keypair::generate_ed25519()

3. Connect to DHT bootstrap nodes
   └─> Kademlia::bootstrap()

4. Lookup manifest record in DHT
   └─> Key: /phase/stable/x86_64/manifest
   └─> Value: { manifest_url: "http://...", ... }

5. Fetch manifest from provider
   └─> HTTP GET http://192.168.1.100:8080/stable/x86_64/manifest.json

6. Verify manifest signatures
   └─> Ed25519 signature check

7. Download artifacts
   └─> HTTP GET http://192.168.1.100:8080/stable/x86_64/vmlinuz
   └─> HTTP GET http://192.168.1.100:8080/stable/x86_64/initramfs.img
   └─> Verify SHA256 hashes

8. Boot kernel with initramfs
```

### Local Network Discovery (mDNS)

```
1. Client queries for _phase-image._tcp.local.

2. Provider responds with:
   - Service name: plasmd-hostname
   - HTTP port: 8080
   - TXT records: channel=stable, arch=x86_64

3. Client filters by required channel/arch

4. Construct manifest URL
   └─> http://provider-ip:8080/stable/x86_64/manifest.json

5. Continue with steps 5-8 above
```

## Self-Hosting Loop

A Phase Boot Provider can serve the artifacts needed to boot itself:

```
┌──────────────────────────────────────────────────────────┐
│  1. Boot from USB/existing system                        │
│     └─> plasmd serve --artifacts /mnt/artifacts          │
└─────────────────────┬────────────────────────────────────┘
                      │
                      ▼
┌──────────────────────────────────────────────────────────┐
│  2. Provider advertises stable/x86_64 artifacts          │
│     └─> DHT: /phase/stable/x86_64/manifest → URL         │
└─────────────────────┬────────────────────────────────────┘
                      │
                      ▼
┌──────────────────────────────────────────────────────────┐
│  3. Other machines discover and boot from this provider  │
│     └─> Fetch kernel + initramfs                         │
└─────────────────────┬────────────────────────────────────┘
                      │
                      ▼
┌──────────────────────────────────────────────────────────┐
│  4. Booted machines become providers themselves          │
│     └─> Self-replicating network boot infrastructure     │
└──────────────────────────────────────────────────────────┘
```

**Benefits**:
- No single point of failure
- Distributed artifact hosting
- Automatic redundancy
- Network scales with adoption

## Security Model

### Artifact Integrity

**Hash Verification**:
- All artifacts have SHA256 hashes in manifest
- Hashes verified before execution
- Prevents tampering during download

**Manifest Signing**:
- Ed25519 signatures over manifest content
- Multiple signatures supported (multi-sig)
- Key rotation via key_id field

### Network Security

**Trust Boundaries**:
- DHT provides **discovery** only (untrusted)
- Manifest signatures provide **authenticity**
- Hash verification provides **integrity**
- HTTPS optional for **confidentiality** (future)

**Threat Model**:
1. **Malicious DHT Records**: Mitigated by manifest signatures
2. **Man-in-the-Middle**: Mitigated by hash verification
3. **Compromised Provider**: Mitigated by signature verification
4. **Network Eavesdropping**: Not currently mitigated (use HTTPS/Noise for encryption)

### Privacy Considerations

**Ephemeral Identity Mode**:
- `phase-discover --ephemeral` generates temporary keypair
- No persistent peer identity
- Reduces tracking across boots

**Metadata Leakage**:
- DHT queries reveal: channel, architecture
- HTTP requests reveal: IP address, download patterns
- Future: Use Noise protocol for encrypted transport

## Performance Characteristics

### HTTP Range Support

Enables resumable downloads and efficient partial fetches:

```
HTTP/1.1 206 Partial Content
Content-Range: bytes 1024-2047/67108864
Content-Length: 1024
Accept-Ranges: bytes
X-Artifact-Hash: sha256:abc123...
```

**Benefits**:
- Resume interrupted downloads
- Fetch only needed portions
- Parallel range downloads (future)

### Caching Strategy

**Manifest Caching**:
- TTL: 30 days (default)
- Clients cache until expiration
- Version field enables invalidation

**Artifact Caching**:
- Immutable (hash-addressed)
- Infinite cache lifetime
- Deduplication across channels

### Scalability

**Provider Scaling**:
- Stateless HTTP server
- No database required
- Horizontal scaling via load balancers

**Network Scaling**:
- DHT scales to millions of nodes
- O(log n) lookup time
- Automatic replication (k=20 nodes)

## Deployment Scenarios

### 1. Home Lab / Small Network

```bash
# Single provider for local machines
plasmd serve --artifacts /srv/artifacts --channel stable --arch x86_64
```

**Discovery**: mDNS (local network only)

### 2. Enterprise / Data Center

```bash
# Multiple providers with DHT for redundancy
plasmd serve --artifacts /var/lib/plasm/artifacts --channel stable
```

**Discovery**: DHT (cross-subnet) + mDNS (same subnet)

### 3. Global Network

```bash
# Distributed providers across regions
plasmd serve --artifacts /data/artifacts --no-mdns
```

**Discovery**: DHT only (global distribution)

### 4. Offline / Air-Gapped

```bash
# No network discovery, direct HTTP access
plasmd serve --no-dht --no-mdns --bind 10.0.0.1 --port 8080
```

**Discovery**: Manual configuration of provider URL

## Future Enhancements

### Planned Features

1. **Noise Protocol**: Encrypted transport for confidentiality
2. **Relay Support**: NAT traversal via libp2p circuit relay
3. **IPFS Integration**: Content-addressed artifact storage
4. **BitTorrent**: Peer-to-peer artifact distribution
5. **Multi-signature Quorum**: Require N-of-M signatures for trust

### Under Consideration

- **Artifact Compression**: Transparent decompression
- **Delta Updates**: Binary diffs between versions
- **CDN Integration**: Hybrid cloud/P2P distribution
- **Metrics Dashboard**: Web UI for provider monitoring

## Related Documentation

- [API Reference](api-reference.md) - HTTP endpoints and CLI commands
- [Troubleshooting](troubleshooting.md) - Common issues and solutions
- [Security](security.md) - Detailed security guidelines
