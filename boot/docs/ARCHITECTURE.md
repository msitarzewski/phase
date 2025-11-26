# Phase Boot Architecture

**Version:** 0.1.0-M2
**Last Updated:** 2025-11-26
**Status:** Milestone 2 - Discovery & Verification Implementation

---

## Table of Contents

1. [Overview](#overview)
2. [Boot Flow Diagram](#boot-flow-diagram)
3. [Component Interactions](#component-interactions)
4. [Trust Chain](#trust-chain)
5. [Boot Modes](#boot-modes)
6. [Network Discovery Flow](#network-discovery-flow)
7. [Directory Structure](#directory-structure)

---

## Overview

Phase Boot is a minimal, network-first boot system that discovers, verifies, and loads operating system images from the Phase distributed network. It provides:

- **Network Discovery**: Kademlia DHT-based manifest discovery via libp2p
- **Cryptographic Verification**: Ed25519 signature verification with rollback protection
- **Flexible Boot Modes**: Internet, Local (LAN), and Private (ephemeral) modes
- **Secure Artifact Fetching**: SHA256-verified downloads with multi-URL fallback
- **Kernel Chainloading**: kexec-based boot into verified kernels

Phase Boot is designed as a minimal initramfs that runs as PID 1, initializes networking, discovers the latest verified boot manifest, downloads artifacts, and kexecs into the target kernel.

---

## Boot Flow Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                    FIRMWARE (UEFI/BIOS)                         │
│  Loads: Phase Boot Kernel + Phase Boot Initramfs               │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                  PHASE BOOT INITRAMFS                           │
│                  /init (PID 1)                                  │
│                                                                 │
│  1. Mount essential filesystems (/proc, /sys, /dev, /run)      │
│  2. Parse kernel cmdline (phase.mode, phase.channel)           │
│  3. Initialize network (DHCP on available interface)           │
│  4. Route to mode handler based on phase.mode                  │
└────────────────────┬────────────────────────────────────────────┘
                     │
         ┌───────────┴───────────┬───────────────┐
         │                       │               │
         ▼                       ▼               ▼
    ┌─────────┐            ┌─────────┐     ┌──────────┐
    │ INTERNET│            │  LOCAL  │     │ PRIVATE  │
    │  MODE   │            │  MODE   │     │  MODE    │
    └────┬────┘            └────┬────┘     └────┬─────┘
         │                      │               │
         │                      │               │
         ▼                      ▼               ▼
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│ DHT Discovery    │  │ Local Cache      │  │ DHT Discovery    │
│                  │  │ or mDNS          │  │ (Ephemeral ID)   │
│ phase-discover   │  │                  │  │ phase-discover   │
│ --channel stable │  │ Find cached      │  │ --ephemeral      │
│ --arch x86_64    │  │ image locally    │  │ --channel stable │
└────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘
         │                     │                      │
         ▼                     ▼                      ▼
┌─────────────────────────────────────────────────────────────────┐
│               MANIFEST URL DISCOVERED                           │
│               /tmp/manifest_url                                 │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                  FETCH MANIFEST (M3+)                           │
│  Download manifest JSON from discovered URL                    │
│  (Currently: stub/placeholder)                                  │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│               VERIFY MANIFEST                                   │
│               phase-verify --manifest manifest.json             │
│                                                                 │
│  1. Load embedded root public key                               │
│  2. Verify Ed25519 signature on manifest                        │
│  3. Check rollback protection (manifest_version >= cached)      │
│  4. Exit 0 (verified) or exit 1 (failed)                        │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│               FETCH ARTIFACTS                                   │
│               phase-fetch --manifest manifest.json              │
│                           --output /boot --artifact all         │
│                                                                 │
│  For each artifact (kernel, initramfs, rootfs):                 │
│    1. Try each URL in manifest.artifacts[].urls[]              │
│    2. Download with streaming SHA256 verification              │
│    3. Verify size matches manifest                             │
│    4. Verify hash matches manifest                             │
│    5. Retry on failure (exponential backoff)                   │
│    6. Write to output directory                                │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│               OPTIONAL: SETUP OVERLAYFS                         │
│               overlayfs-setup.sh                                │
│                                                                 │
│  Create writable overlay for rootfs:                            │
│    Lower: /cache/rootfs (read-only verified image)             │
│    Upper: /tmp/overlay-upper (tmpfs for changes)               │
│    Work:  /tmp/overlay-work (tmpfs for overlay metadata)       │
│    Merged: /newroot (unified writable view)                     │
└────────────────────┬────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│               KEXEC INTO NEW KERNEL                             │
│               kexec-boot.sh --kernel vmlinuz                    │
│                             --initramfs initramfs.img           │
│                                                                 │
│  1. Load current kernel cmdline params (preserve phase.*)       │
│  2. Build final cmdline (preserve console, root, phase.*)       │
│  3. kexec -l (load new kernel + initramfs into memory)         │
│  4. kexec -e (execute - replaces current kernel)               │
│                                                                 │
│  *** POINT OF NO RETURN ***                                     │
│  System reboots into new kernel without BIOS/firmware          │
└─────────────────────────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                  NEW KERNEL BOOTS                               │
│  Target system's init takes over (systemd, OpenRC, etc.)        │
└─────────────────────────────────────────────────────────────────┘
```

---

## Component Interactions

### Discovery Phase

```
┌───────────┐         DHT Query          ┌──────────────┐
│   init    │──────────────────────────▶ │ phase-discover│
│ (PID 1)   │  /phase/stable/x86_64     │   (libp2p)   │
└─────┬─────┘                            └──────┬───────┘
      │                                         │
      │                                         │ Kademlia
      │                                         │ Network
      │                                         ▼
      │                              ┌──────────────────┐
      │                              │ Bootstrap Nodes  │
      │                              │ (DHT Peers)      │
      │                              └────────┬─────────┘
      │                                       │
      │  Manifest URL                         │ GetRecord
      │  (stdout)                             │ Response
      ◀────────────────────────────────────────┘
      │
      │ Write to /tmp/manifest_url
      ▼
```

### Verification Phase

```
┌───────────┐                              ┌──────────────┐
│   init    │────── manifest.json ────────▶│ phase-verify │
│           │                              │  (Ed25519)   │
└───────────┘                              └──────┬───────┘
                                                  │
                          ┌───────────────────────┤
                          │                       │
                          ▼                       ▼
                ┌──────────────────┐    ┌─────────────────┐
                │ Embedded Root Key│    │ manifest.signed │
                │  (compile-time)  │    │ manifest.sigs[] │
                └────────┬─────────┘    └────────┬────────┘
                         │                       │
                         │   Verify Signature    │
                         └──────────▶◆◀──────────┘
                                    │
                          ┌─────────┴─────────┐
                          ▼                   ▼
                      ✅ VALID           ❌ INVALID
                   (exit 0)              (exit 1)
                      │                       │
                      │                   ABORT BOOT
                      ▼
                  Continue
```

### Fetch Phase

```
┌──────────────┐                            ┌─────────────┐
│ phase-fetch  │──── Parse manifest ───────▶│ manifest    │
│              │                            │ .artifacts  │
└──────┬───────┘                            └─────────────┘
       │                                           │
       │  For each artifact:                       │
       │  kernel, initramfs, rootfs                │
       └───────────────────────────────────────────┘
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
   ┌─────────┐     ┌─────────┐     ┌─────────┐
   │  URL 1  │     │  URL 2  │     │  URL 3  │
   │ (HTTP)  │     │ (HTTPS) │     │ (IPFS)  │
   └────┬────┘     └────┬────┘     └────┬────┘
        │               │               │
        │ Download + Stream SHA256      │
        └───────────┬───┴───────────────┘
                    ▼
        ┌────────────────────────┐
        │ Verify:                │
        │  - Size matches        │
        │  - Hash matches        │
        │  - No corruption       │
        └───────────┬────────────┘
                    │
                    ▼
             Write to /boot/
```

### Kexec Phase

```
┌──────────────┐
│ kexec-boot.sh│
└──────┬───────┘
       │
       ├─── Read /proc/cmdline (preserve phase.* params)
       │
       ├─── Build final cmdline
       │
       ▼
┌──────────────────────┐
│ kexec -l             │  Load kernel into memory
│  --kernel vmlinuz    │  Does NOT execute yet
│  --initrd initrd.img │
│  --cmdline "..."     │
└──────┬───────────────┘
       │
       │ Kernel loaded successfully
       │
       ▼
┌──────────────────────┐
│ kexec -e             │  Execute loaded kernel
│                      │  *** REPLACES CURRENT KERNEL ***
└──────────────────────┘
       │
       │ (should never return)
       ▼
   New kernel boots
```

---

## Trust Chain

The Phase Boot trust chain ensures only cryptographically verified code executes:

```
┌─────────────────────────────────────────────────────────────────┐
│                    FIRMWARE TRUST ANCHOR                        │
│  UEFI Secure Boot (optional) / Coreboot verified boot          │
│  Verifies: Phase Boot Kernel signature                         │
└────────────────────┬────────────────────────────────────────────┘
                     │ Trusted
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                   PHASE BOOT INITRAMFS                          │
│  Contains:                                                      │
│    - Embedded Root Public Key (compile-time)                    │
│    - phase-verify binary                                        │
│    - phase-discover binary                                      │
│    - phase-fetch binary                                         │
└────────────────────┬────────────────────────────────────────────┘
                     │ Embedded Key
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                    ROOT PUBLIC KEY                              │
│  Location: daemon/src/bin/../../keys/root.pub.placeholder      │
│  Type: Ed25519 (32-byte public key)                            │
│  Used to verify: Manifest signatures                           │
└────────────────────┬────────────────────────────────────────────┘
                     │ Verifies
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                    BOOT MANIFEST                                │
│  File: manifest.json                                            │
│  Contains:                                                      │
│    - manifest_version (monotonic counter)                       │
│    - artifacts[] (kernel, initramfs, rootfs)                    │
│    - signatures[] (Ed25519 signatures)                          │
│    - signed.data (base64 canonical JSON)                        │
│                                                                 │
│  Verification (phase-verify):                                   │
│    1. Parse manifest                                            │
│    2. Decode signed.data (base64 → JSON)                        │
│    3. Hash signed data (SHA256)                                 │
│    4. Verify signature with root public key                     │
│    5. Check manifest_version >= cached version (rollback)       │
│    6. Update cached version on success                          │
└────────────────────┬────────────────────────────────────────────┘
                     │ Hash verification
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                    BOOT ARTIFACTS                               │
│  Files: vmlinuz, initramfs.img, rootfs.img                     │
│                                                                 │
│  Each artifact has:                                             │
│    - hash: "sha256:abcd1234..." (from manifest)                 │
│    - size: bytes (from manifest)                                │
│    - urls: [url1, url2, ...] (download locations)              │
│                                                                 │
│  Verification (phase-fetch):                                    │
│    1. Download from urls[] (try each until success)            │
│    2. Stream through SHA256 hasher during download             │
│    3. Verify size matches manifest.artifacts[].size            │
│    4. Verify hash matches manifest.artifacts[].hash            │
│    5. Fail entire boot if mismatch                             │
└────────────────────┬────────────────────────────────────────────┘
                     │ kexec
                     ▼
┌─────────────────────────────────────────────────────────────────┐
│                    TARGET KERNEL                                │
│  Verified kernel executes                                       │
│  Target system's trust chain takes over (dm-verity, IMA, etc.)  │
└─────────────────────────────────────────────────────────────────┘
```

**Key Properties:**

1. **Root of Trust**: Embedded public key in initramfs (compile-time)
2. **Manifest Signing**: Ed25519 signatures on canonical JSON representation
3. **Rollback Protection**: Monotonic `manifest_version` counter with cached verification
4. **Artifact Integrity**: SHA256 verification during download (streaming)
5. **No Network Trust**: Network only delivers data; cryptographic verification establishes trust

**File References:**

- Root key embedding: `/home/user/phase/daemon/src/bin/phase_verify.rs:113`
- Signature verification: `/home/user/phase/daemon/src/bin/phase_verify.rs:289-313`
- Rollback check: `/home/user/phase/daemon/src/bin/phase_verify.rs:141-163`
- Hash verification: `/home/user/phase/daemon/src/bin/phase_fetch.rs:246-348`

---

## Boot Modes

Phase Boot supports three boot modes controlled by the `phase.mode=` kernel parameter:

### 1. Internet Mode (`phase.mode=internet`)

**Description**: Full network access with DHT discovery
**Default**: Yes (if no `phase.mode` specified)

**Behavior:**

- Requires active network connection
- Uses Kademlia DHT for manifest discovery
- Persistent peer identity (generates session key)
- Downloads artifacts from internet sources
- Caches downloaded images (if `phase.cache=true`)
- Suitable for: Cloud instances, workstations, general use

**Discovery Flow:**

```
1. phase-discover --arch x86_64 --channel stable --timeout 60
2. Connects to bootstrap nodes
3. Performs DHT lookup: /phase/stable/x86_64/manifest
4. Returns manifest URL
5. Downloads manifest from returned URL
6. Verifies manifest signature
7. Downloads kernel + initramfs
8. kexec into downloaded kernel
```

**Implementation**: `/home/user/phase/boot/initramfs/scripts/mode-handler.sh:91-163`

### 2. Local Mode (`phase.mode=local`)

**Description**: LAN-only access with local cache preferred
**Use Case**: Air-gapped networks, enterprise LANs, local development

**Behavior:**

- Network optional (prefers LAN, can work offline)
- Uses mDNS/Avahi for local discovery (M3+ feature)
- Prefers local cache over network downloads
- Downloads from LAN-local mirrors if available
- Falls back to cached images if network unavailable
- Suitable for: Enterprise environments, offline systems, edge devices

**Cache Lookup:**

```
Cache directory: /cache/phase/
Naming pattern:  {manifest_version}-{channel}-{artifact}
Example:         1234-stable-vmlinuz
                 1234-stable-initramfs
```

**Discovery Flow:**

```
1. Check /cache/phase/ for matching channel
2. If found: Load from cache, verify hash
3. If network available: Try mDNS for local peers
4. Download from LAN-local mirror if available
5. Update cache with new version
6. kexec into cached/downloaded kernel
```

**Implementation**: `/home/user/phase/boot/initramfs/scripts/mode-handler.sh:167-245`

### 3. Private Mode (`phase.mode=private`)

**Description**: Ephemeral identity, no persistent writes
**Use Case**: Privacy-sensitive environments, public computers, untrusted hardware

**Behavior:**

- Requires network (no cache use - prevents identity leakage)
- Generates ephemeral libp2p identity (new peer ID each boot)
- Downloads to tmpfs only (no disk writes)
- Forces `phase.nowrite=true` (overlayfs on tmpfs)
- No artifact caching (prevents tracking)
- Suitable for: Public terminals, privacy-focused systems, temporary sessions

**Security Properties:**

- **No Persistent Identity**: New libp2p peer ID each boot
- **No Disk Writes**: All changes in tmpfs (lost on reboot)
- **No Cache Reuse**: Cannot correlate boots via cached artifacts
- **Ephemeral Networking**: No persistent network config stored

**Discovery Flow:**

```
1. phase-discover --arch x86_64 --channel stable --ephemeral --timeout 60
2. Generates one-time Ed25519 keypair for libp2p identity
3. Performs DHT lookup with ephemeral peer ID
4. Downloads manifest to /tmp (tmpfs)
5. Downloads kernel + initramfs to /tmp (tmpfs)
6. Sets up overlayfs with tmpfs upper layer
7. kexec with phase.nowrite=true in cmdline
```

**Implementation**: `/home/user/phase/boot/initramfs/scripts/mode-handler.sh:249-344`

### Mode Comparison Table

| Feature                  | Internet Mode | Local Mode | Private Mode |
|--------------------------|---------------|------------|--------------|
| **Network Required**     | Yes           | Optional   | Yes          |
| **Discovery Method**     | DHT (Kademlia)| mDNS/Cache | DHT (Ephemeral) |
| **Peer Identity**        | Session-based | Session-based | Ephemeral (one-time) |
| **Caching**              | Yes           | Preferred  | No (privacy) |
| **Disk Writes**          | Allowed       | Allowed    | Forbidden (tmpfs only) |
| **Artifact Source**      | Internet URLs | LAN/Cache  | Internet (tmpfs) |
| **Privacy Level**        | Standard      | Standard   | High         |
| **Offline Capable**      | No            | Yes (cache)| No           |
| **Rollback Protection**  | Yes           | Yes        | Yes (in-memory) |

**Kernel Cmdline Reference:**

```bash
# Internet mode (default)
phase.mode=internet phase.channel=stable

# Local mode with cache
phase.mode=local phase.channel=stable phase.cache=true

# Private mode (forces no-write)
phase.mode=private phase.channel=stable
# (phase.nowrite=true is automatically added)
```

---

## Network Discovery Flow

Phase Boot uses a **Kademlia DHT** (Distributed Hash Table) implemented via libp2p for network-based manifest discovery.

### DHT Key Format

```
/phase/{channel}/{arch}/manifest

Examples:
  /phase/stable/x86_64/manifest
  /phase/testing/arm64/manifest
  /phase/nightly/x86_64/manifest
```

**Implementation**: `/home/user/phase/daemon/src/bin/phase_discover.rs:109`

### Discovery Process

```
┌──────────────┐
│ phase-discover│  (Client Mode - no routing table participation)
└───────┬───────┘
        │
        │ 1. Generate Ed25519 keypair
        │    - Ephemeral: New key each run (private mode)
        │    - Session: New key per session (internet/local mode)
        │
        ▼
┌────────────────────┐
│ libp2p Swarm       │
│  - TCP transport   │
│  - Noise encryption│
│  - Yamux mux       │
└───────┬────────────┘
        │
        │ 2. Connect to bootstrap nodes
        │    (hardcoded or --bootstrap args)
        │
        ▼
┌────────────────────┐
│ Bootstrap Nodes    │
│ (Phase Network)    │
└───────┬────────────┘
        │
        │ 3. Bootstrap DHT
        │    (find peers, populate routing table)
        │
        ▼
┌────────────────────┐
│ Kademlia DHT       │
│  Mode: Client      │
└───────┬────────────┘
        │
        │ 4. GetRecord("/phase/stable/x86_64/manifest")
        │    (iterative closest-peer lookup)
        │
        ▼
┌────────────────────┐
│ DHT Record         │
│  Key: /phase/...   │
│  Value: manifest_url│
└───────┬────────────┘
        │
        │ 5. Return manifest URL
        │    (stdout or JSON format)
        │
        ▼
   MANIFEST_URL
```

### libp2p Configuration

**Transport Stack**:
- **TCP**: Base transport layer
- **Noise**: Encrypted connections (XX handshake pattern)
- **Yamux**: Stream multiplexing

**Kademlia Settings**:
- **Mode**: Client (does not participate in routing, only queries)
- **Store**: MemoryStore (ephemeral, not persisted)
- **Timeout**: 30 seconds default (configurable via `--timeout`)

**Implementation Reference**:
- Swarm setup: `/home/user/phase/daemon/src/bin/phase_discover.rs:115-130`
- Bootstrap: `/home/user/phase/daemon/src/bin/phase_discover.rs:136-158`
- DHT query: `/home/user/phase/daemon/src/bin/phase_discover.rs:161-206`

### Bootstrap Nodes

Bootstrap nodes are the initial entry points into the Phase DHT network.

**Default Bootstrap Nodes**:
- Currently empty (local testing uses `--bootstrap` flag)
- Production: Will use hardcoded Phase network bootstrap peers

**Format**: Multiaddr with embedded peer ID

```
/ip4/1.2.3.4/tcp/4001/p2p/12D3KooWABC...
/ip6/2001:db8::1/tcp/4001/p2p/12D3KooWXYZ...
```

**Implementation**: `/home/user/phase/daemon/src/bin/phase_discover.rs:255-262`

### Query Process

1. **Start DHT Query**: `swarm.behaviour_mut().kademlia.get_record(key)`
2. **Iterative Lookup**: Kademlia queries progressively closer peers
3. **Record Found**: `QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(record)))`
4. **Extract URL**: `record.record.value` contains manifest URL (UTF-8)
5. **Output**: Print URL to stdout (text or JSON format)

**Timeout Handling**:
- Query runs with configurable timeout (default: 30s)
- On timeout: Exit with code 1
- On success: Exit with code 0 and manifest URL on stdout

**Implementation**: `/home/user/phase/daemon/src/bin/phase_discover.rs:172-206`

### Discovery Modes

| Mode     | Identity Type | DHT Participation | Bootstrap Required | Caching |
|----------|---------------|-------------------|-------------------|---------|
| Internet | Session       | Client-only       | Yes               | Yes     |
| Local    | Session       | Client-only       | Optional (mDNS)   | Yes     |
| Private  | Ephemeral     | Client-only       | Yes               | No      |

**Privacy Note**: Client mode means Phase Boot nodes **do not** participate in DHT routing. They only query for records, do not store records, and do not route queries for other peers. This minimizes metadata leakage.

---

## Directory Structure

```
/home/user/phase/boot/
├── initramfs/                      # Initramfs contents
│   ├── init                        # PID 1 init script
│   └── scripts/                    # Boot scripts
│       ├── kexec-boot.sh          # Kernel chainloader
│       ├── mode-handler.sh        # Boot mode orchestrator
│       ├── net-init.sh            # Network initialization (M2+)
│       ├── overlayfs-setup.sh     # OverlayFS setup
│       └── plasm-init.sh          # WASM daemon initialization
│
├── schemas/                        # JSON schemas
│   └── manifest.schema.json       # Boot manifest schema
│
├── docs/                           # Documentation
│   ├── ARCHITECTURE.md            # This file
│   ├── COMPONENTS.md              # Component reference
│   ├── SECURITY.md                # Security architecture
│   ├── THREAT-MODEL.md            # Threat model analysis
│   ├── TROUBLESHOOTING.md         # Troubleshooting guide
│   ├── testing.md                 # Testing procedures
│   └── tested-hardware.md         # Hardware compatibility
│
└── build/                          # Build artifacts (generated)
    ├── initramfs.cpio.gz          # Compressed initramfs
    └── vmlinuz                     # Phase Boot kernel

/home/user/phase/daemon/src/bin/   # Boot binaries (installed to initramfs)
├── phase_discover.rs               # DHT discovery binary
├── phase_verify.rs                 # Signature verification binary
└── phase_fetch.rs                  # Artifact downloader binary
```

**Runtime Directory Structure** (in Phase Boot initramfs):

```
/ (initramfs root)
├── bin/
│   ├── phase-discover             # DHT discovery
│   ├── phase-verify               # Manifest verification
│   ├── phase-fetch                # Artifact downloader
│   ├── sh, ls, mount, etc.        # BusyBox symlinks
│   └── kexec                       # kexec-tools
│
├── scripts/                        # Boot orchestration
│   ├── kexec-boot.sh
│   ├── mode-handler.sh
│   ├── overlayfs-setup.sh
│   └── plasm-init.sh
│
├── proc/, sys/, dev/, run/         # Essential mounts (created by init)
│
├── tmp/                            # Temporary files (tmpfs)
│   ├── manifest_url               # Discovered manifest URL
│   ├── phase-boot.log             # Boot log
│   ├── network.status             # Network status ("up"/"down")
│   ├── network.interface          # Active interface name
│   └── network.ip                 # Assigned IP address
│
└── cache/                          # Cache mount point (local mode)
    └── phase/
        └── {version}-{channel}-{artifact}
```

---

## Implementation Status

**Milestone 2 (Current)**: Discovery & Verification
- ✅ phase-discover: DHT-based manifest discovery
- ✅ phase-verify: Ed25519 signature verification with rollback protection
- ✅ phase-fetch: SHA256-verified artifact downloads
- ✅ Mode-specific boot flows (internet/local/private)
- ✅ Network initialization
- ✅ OverlayFS setup

**Milestone 3 (Next)**: Integration & End-to-End Boot
- ⏳ Full manifest fetch (HTTP/HTTPS download)
- ⏳ Cache management (version tracking, pruning)
- ⏳ mDNS local discovery (Local Mode)
- ⏳ End-to-end boot test (discovery → verify → fetch → kexec)

**Milestone 4 (Future)**: Production Readiness
- ⏳ IPFS fallback support
- ⏳ Plasm daemon integration
- ⏳ Hardware compatibility testing
- ⏳ Performance optimization
- ⏳ Production bootstrap nodes

---

## See Also

- [COMPONENTS.md](./COMPONENTS.md) - Detailed component reference
- [SECURITY.md](./SECURITY.md) - Security architecture and threat model
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) - Common issues and solutions
- [manifest.schema.json](../schemas/manifest.schema.json) - Manifest format specification

---

**Last Updated**: 2025-11-26
**Maintained By**: Phase Boot Team
**License**: MIT
