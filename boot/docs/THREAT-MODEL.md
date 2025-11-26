# Phase Boot Threat Model

**Version**: 1.0 (2025-11-26)
**Status**: Milestone 7 - Documentation
**Audience**: Technical users, security reviewers, system architects

---

## Table of Contents

1. [System Overview](#system-overview)
2. [Threat Actors](#threat-actors)
3. [Attack Vectors](#attack-vectors)
4. [Security Assumptions](#security-assumptions)
5. [Defense in Depth](#defense-in-depth)
6. [Known Limitations & Future Work](#known-limitations--future-work)
7. [Security Recommendations](#security-recommendations)

---

## System Overview

Phase Boot is a bootable USB/VM image designed to:

1. **Boot** from UEFI firmware into a minimal Linux environment
2. **Discover** peers via mDNS (local) and libp2p Kademlia DHT (internet)
3. **Fetch** signed manifests from the Phase network
4. **Verify** Ed25519 signatures and SHA256 artifact hashes
5. **Download** kernel/initramfs/rootfs artifacts
6. **Execute** kexec handoff to the verified target system
7. **Run** WASM workloads via the Plasm daemon

### Trust Boundary Diagram

```
┌─────────────────────────────────────────────────────────────┐
│ TRUSTED DOMAIN                                              │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ Embedded Root Key (daemon/src/bin/phase_verify.rs:113) │ │
│ │ UEFI Firmware (if Secure Boot enabled)                 │ │
│ └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────┐
│ VERIFICATION LAYER (CRITICAL SECURITY BOUNDARY)             │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ phase-verify: Ed25519 signature verification           │ │
│ │ phase-fetch: SHA256 hash verification                  │ │
│ │ Rollback protection (manifest_version)                 │ │
│ └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
                           ↓
┌─────────────────────────────────────────────────────────────┐
│ UNTRUSTED DOMAIN                                            │
│ - Network (DHT, mirrors, DNS)                               │
│ - Bootstrap nodes                                           │
│ - Artifact servers (HTTPS)                                  │
│ - Cache (Local Mode only)                                   │
│ - Physical media (after creation)                           │
└─────────────────────────────────────────────────────────────┘
```

**Critical Insight**: Security depends entirely on:
1. Integrity of the embedded root public key
2. Correctness of signature verification implementation
3. Monotonic manifest versioning

Everything beyond this boundary is considered hostile.

---

## Threat Actors

### TA-1: Passive Network Attacker

**Capabilities**:
- Monitor network traffic (DHT queries, HTTP downloads)
- Collect peer IDs, IP addresses, download patterns
- Analyze manifest metadata

**Goals**:
- Deanonymization
- Usage pattern analysis
- Infrastructure mapping

**Impact**: **PRIVACY** - User tracking, network analysis

**Mitigations**:
- Private Mode uses ephemeral libp2p identity (`daemon/src/bin/phase_discover.rs:89`)
- HTTPS hides artifact content (but not URLs/sizes)
- DHT queries reveal channel/arch preferences

**Residual Risk**: **MEDIUM** - Traffic analysis can still reveal:
- Boot timing patterns
- Geographic correlation (IP addresses)
- Artifact sizes (even over HTTPS)

---

### TA-2: Active Network Attacker (MITM)

**Capabilities**:
- DNS hijacking
- DHT poisoning (return malicious manifest URLs)
- MITM on HTTP connections
- Block legitimate bootstrap nodes

**Goals**:
- Serve malicious manifests
- Serve tampered artifacts
- Denial of service

**Impact**: **INTEGRITY/AVAILABILITY** - System compromise or boot failure

**Mitigations**:
- Ed25519 signature verification (`daemon/src/bin/phase_verify.rs:289-313`)
  - Manifest signatures must verify against embedded root key
  - Uses SHA256 pre-hashing before signature verification
- SHA256 hash verification on artifacts (`daemon/src/bin/phase_fetch.rs:246`)
  - Downloads fail if hash mismatch
- HTTPS for artifact downloads (certificate validation)

**Residual Risk**: **LOW** (for integrity), **MEDIUM** (for availability)
- **Cannot** serve malicious code (signature verification prevents this)
- **Can** cause denial of service (block all downloads)
- **Can** fingerprint users (serve unique URLs per victim)

---

### TA-3: Malicious Bootstrap Node Operator

**Capabilities**:
- DHT record poisoning
- Sybil attacks (multiple fake peer IDs)
- Eclipse attacks (isolate victim from legitimate network)
- Return malicious manifest URLs

**Goals**:
- Redirect victims to malicious manifests
- Surveillance of network participants
- Censorship (block specific manifest versions)

**Impact**: **AVAILABILITY/PRIVACY** - Boot failure or tracking

**Mitigations**:
- Signature verification prevents acceptance of unsigned manifests
- Multiple URL sources in manifest (`daemon/src/bin/phase_verify.rs:83-85`)
  - Artifacts have fallback URLs
- mDNS as fallback for local network discovery

**Residual Risk**: **MEDIUM**
- **Cannot** compromise integrity (signatures still required)
- **Can** cause DoS by eclipse attack (isolate from legitimate nodes)
- **Can** track which manifests are requested (privacy leak)

**Attack Scenario**:
```
1. Attacker controls bootstrap nodes in daemon/src/bin/phase_discover.rs:255
2. User's DHT query: /phase/stable/x86_64/manifest
3. Attacker returns: https://evil.com/fake-manifest.json
4. phase-verify downloads manifest
5. Signature verification FAILS (not signed by root key)
6. Boot process halts (DoS achieved)
```

---

### TA-4: Compromised Mirror Server

**Capabilities**:
- Serve malicious artifacts
- Selective targeting (based on IP/timing)
- Traffic analysis

**Goals**:
- Serve backdoored kernel/initramfs
- Fingerprint specific users

**Impact**: **INTEGRITY** - System compromise attempt

**Mitigations**:
- SHA256 hash verification (`daemon/src/bin/phase_fetch.rs:246-290`)
  - Hash is embedded in signed manifest
  - Mismatch causes immediate download failure
- HTTPS with certificate validation
- Multiple mirror URLs (fallback)

**Residual Risk**: **VERY LOW** (for integrity), **LOW** (for privacy)
- **Cannot** serve malicious artifacts (hash verification prevents)
- **Can** track download patterns
- **Can** cause DoS (refuse to serve)

---

### TA-5: Physical Access Attacker

**Capabilities**:
- Modify USB contents before first use
- Evil maid attack (modify USB while unattended)
- Cold boot attacks (if machine left running)
- USB firmware attacks (BadUSB)

**Goals**:
- Implant backdoors in initramfs/kernel
- Steal cached credentials
- Replace verification binaries

**Impact**: **INTEGRITY/CONFIDENTIALITY** - Full system compromise

**Mitigations**:
- **Before first boot**: Trust on first use (TOFU) model
  - User must create USB from trusted source
  - Verify checksums of downloaded Phase Boot image
- **After first boot**: Read-only ESP partition (future enhancement)
- **Private Mode**: No cache writes, ephemeral identity
  - Prevents persistence of compromises
- **Future: Secure Boot**: UEFI signature verification
  - Would detect modified bootloader/kernel

**Residual Risk**: **HIGH** (without Secure Boot), **MEDIUM** (with Secure Boot)
- **Can** modify USB contents (no tamper-evident mechanism)
- **Can** replace binaries (no Secure Boot verification yet)
- **Cannot** sign malicious manifests (no private key)
- **Private Mode mitigates** persistence but not initial compromise

**Attack Scenarios**:

1. **Pre-delivery tampering**:
   - Attacker intercepts USB shipment
   - Replaces `phase-verify` binary with backdoored version
   - User boots, backdoored verifier accepts malicious manifest
   - **Mitigation**: Verify USB image checksums, use Secure Boot

2. **Evil maid attack**:
   - User boots USB in hotel, leaves machine
   - Attacker gains physical access
   - Modifies `/boot/EFI/BOOT/BOOTX64.EFI` on ESP partition
   - User reboots, malicious bootloader runs
   - **Mitigation**: Secure Boot, read-only ESP, physical security

3. **BadUSB attack**:
   - USB controller firmware modified to emulate keyboard
   - Injects malicious commands at boot prompt
   - **Mitigation**: Disable USB HID in UEFI, zero boot timeout

---

### TA-6: Supply Chain Attacker

**Capabilities**:
- Compromise build infrastructure
- Sign malicious manifests with legitimate key
- Distribute backdoored Phase Boot images
- Compromise upstream dependencies (kernel, BusyBox)

**Goals**:
- Persistent infrastructure compromise
- Mass surveillance
- Targeted attacks

**Impact**: **INTEGRITY/CONFIDENTIALITY** - Widespread compromise

**Mitigations**:
- **Current**:
  - Reproducible builds (planned)
  - Open source (code review)
  - Dependency pinning (Cargo.lock)

- **Required for production**:
  - Hardware security module (HSM) for signing key
  - Multi-party signing (threshold signatures)
  - Build provenance attestation
  - Transparency log for signed manifests

**Residual Risk**: **HIGH** (single signing key)
- **If signing key compromised**: Game over
  - All signatures valid, no detection mechanism
  - Rollback protection ineffective (attacker can increment version)
- **If build infrastructure compromised**: Backdoored binaries
  - Reproducible builds would detect (but not deployed yet)

**Critical Vulnerability**: Single point of failure at signing key.

---

## Attack Vectors

### AV-1: DHT Poisoning

**Attack Description**:
Attacker registers malicious DHT records for manifest keys:
```
Key: /phase/stable/x86_64/manifest
Value: https://evil.com/backdoor-manifest.json
```

**Attack Flow**:
```
1. phase-discover queries DHT for manifest
2. Malicious bootstrap node returns evil.com URL
3. phase-verify downloads manifest from evil.com
4. [BLOCKED] Signature verification fails
5. Boot process halts
```

**Impact**: **Denial of Service** (cannot achieve code execution)

**Current Mitigations**:
- Ed25519 signature verification (`daemon/src/bin/phase_verify.rs:193-218`)
- Requires signature from embedded root key
- Multiple fallback URLs in manifest

**Residual Risk**: **MEDIUM**
- DoS possible (eclipse attack)
- Privacy leak (attacker learns query patterns)

**Detection**: None currently

**Future Improvements**:
- DHT record authentication (signed DHT values)
- Fallback to hardcoded manifest URLs
- Reputation system for bootstrap nodes

---

### AV-2: Manifest Replay Attack

**Attack Description**:
Attacker serves old, legitimately-signed manifest:
```json
{
  "manifest_version": 100,
  "version": "1.0.0",
  "signatures": [{"keyid": "...", "sig": "VALID_OLD_SIG"}]
}
```

**Attack Flow**:
```
1. Attacker intercepts/caches old signed manifest (v100)
2. Current manifest version is v150
3. User boots, phase-discover returns old manifest URL
4. [BLOCKED] Rollback protection detects v100 < v150
5. Boot process halts
```

**Impact**: **Downgrade Attack** (blocked), **DoS** (achievable)

**Current Mitigations**:
- Rollback protection (`daemon/src/bin/phase_verify.rs:140-164`)
  - Compares `manifest_version` against cached version
  - Rejects if `manifest.manifest_version < cached_version`
  - Cache location: `/cache/version` (Local Mode)

**Residual Risk**: **LOW** (Local Mode), **MEDIUM** (Internet/Private Mode)
- **Local Mode**: Effective protection (cached version persists)
- **Internet/Private Mode**: No persistent cache
  - Fresh boot accepts any version (no prior reference)
  - Vulnerable to "freeze time" attack (always serve v100)

**Detection**: None (indistinguishable from legitimate old manifest)

**Future Improvements**:
- Embed minimum version in Phase Boot image
- Expiration timestamps in manifests (`expires_at` field exists but not enforced)
- Online freshness oracle (signed timestamps)

---

### AV-3: Artifact Tampering

**Attack Description**:
Attacker modifies kernel artifact during download:

**Attack Flow**:
```
1. Manifest specifies:
   {
     "kernel": {
       "hash": "sha256:abc123...",
       "urls": ["https://mirror.com/kernel.img"]
     }
   }
2. Attacker MITM mirror.com connection
3. Serves modified kernel with backdoor
4. [BLOCKED] SHA256 verification fails
5. Download rejected
```

**Impact**: **Integrity Violation** (fully mitigated)

**Current Mitigations**:
- SHA256 hash verification (`daemon/src/bin/phase_fetch.rs:246-290`)
  - Computes hash during download
  - Rejects file if mismatch
- HTTPS transport (certificate validation)
- Hash embedded in signed manifest

**Residual Risk**: **NONE** (assuming hash function not broken)

**Attack Variants**:
- **Chosen-prefix collision**: Not feasible with SHA256 (2^128 operations)
- **Birthday attack**: Not applicable (hash is specified, not chosen)
- **Length extension**: Not applicable (hash covers entire file)

---

### AV-4: Signing Key Compromise

**Attack Description**:
Attacker obtains the Ed25519 private signing key.

**Attack Flow**:
```
1. Attacker compromises build server, extracts private key
2. Generates malicious manifest with backdoored kernel
3. Signs manifest with legitimate key
4. Publishes to DHT and mirrors
5. Users boot, verification succeeds
6. Backdoored kernel executes
```

**Impact**: **COMPLETE COMPROMISE** - No technical mitigation possible

**Current Mitigations**:
- **NONE** - Single key, no rotation, no revocation

**Residual Risk**: **CRITICAL**
- Single point of failure
- No detection mechanism
- No recovery mechanism

**Detection Strategies** (not implemented):
- Certificate Transparency-style log (signed manifest append-only log)
- Gossip protocol (compare manifests across peers)
- Canary values (monitor for unexpected changes)
- Multi-party signing (threshold signatures)

**Recovery** (not implemented):
- Key rotation protocol
- Emergency revocation mechanism
- Backup signing keys

**Recommended Mitigations**:
1. **Immediate**: Store key in HSM (hardware security module)
2. **Short-term**: Implement key rotation (every 90 days)
3. **Long-term**: Threshold signatures (3-of-5 quorum)
4. **Monitoring**: Manifest transparency log with gossip verification

---

### AV-5: DNS Hijacking

**Attack Description**:
Attacker hijacks DNS for artifact mirror domains.

**Attack Flow**:
```
1. Manifest URLs: ["https://cdn.phase.network/kernel.img"]
2. Attacker poisons DNS: cdn.phase.network → evil.com
3. phase-fetch downloads from evil.com
4. [BLOCKED] Hash verification fails
5. [FALLBACK] Tries next URL in manifest
```

**Impact**: **Availability** (if all mirrors hijacked)

**Current Mitigations**:
- SHA256 verification (see AV-3)
- HTTPS with certificate validation
  - DNS hijack to evil.com fails certificate check
- Multiple mirror URLs (fallback)

**Residual Risk**: **LOW**
- Cannot compromise integrity (hash verification)
- Can cause DoS (if all mirrors targeted)

---

### AV-6: Cache Poisoning (Local Mode)

**Attack Description**:
In Local Mode, attacker pre-populates cache with malicious artifacts.

**Attack Flow**:
```
1. User boots USB in Local Mode
2. phase-fetch checks /cache/ for artifacts
3. [CURRENT BEHAVIOR] Cache not yet implemented
4. [FUTURE] If cache hit, uses cached artifact
5. [QUESTION] Is cached artifact re-verified?
```

**Impact**: **TBD** (depends on cache verification design)

**Current Mitigations**:
- Cache not yet implemented (M4+)

**Residual Risk**: **MEDIUM** (future feature)

**Design Requirements** (for cache implementation):
1. **MUST** re-verify SHA256 hash on cache hits
2. **MUST** verify manifest version on cache hits
3. **SHOULD** cryptographically bind cache to USB device
4. **SHOULD** use authenticated encryption for cached artifacts

**Anti-Pattern to Avoid**:
```rust
// INSECURE - DO NOT DO THIS
if cache_hit {
    return cached_artifact;  // Assumes cache integrity
}
```

**Correct Pattern**:
```rust
if cache_hit {
    let artifact = read_cache(path);
    verify_hash(artifact, expected_hash)?;  // Re-verify
    return artifact;
}
```

---

### AV-7: Pre-Boot Attacks (Without Secure Boot)

**Attack Description**:
Attacker modifies bootloader or kernel before signature verification runs.

**Attack Flow**:
```
1. UEFI firmware loads BOOTX64.EFI from ESP
2. [WITHOUT SECURE BOOT] No signature verification
3. Attacker's modified bootloader executes
4. Bootloader loads attacker's kernel (bypasses phase-verify)
5. System compromised before any Phase Boot code runs
```

**Impact**: **COMPLETE BYPASS** of all security mechanisms

**Current Mitigations**:
- **NONE** - Secure Boot not yet implemented

**Residual Risk**: **CRITICAL** (without Secure Boot)

**Mitigation Roadmap** (M7 Secure Boot investigation):
1. Sign Phase Boot kernel with Microsoft UEFI CA key (for compatibility)
2. Include shim bootloader (for custom key enrollment)
3. Sign initramfs and verify in kernel
4. Document Secure Boot setup in UEFI

**User Responsibility**:
- Enable Secure Boot in UEFI settings
- Verify Phase Boot is signed by trusted CA
- Enroll custom keys if using community builds

---

## Security Assumptions

### Trusted Components

We **explicitly trust**:

1. **Embedded Root Public Key** (`daemon/src/bin/phase_verify.rs:113`)
   - Assumption: Key is correctly embedded at build time
   - Assumption: Key corresponds to legitimate Phase signing key
   - Risk: If key is wrong at build, all verification is useless
   - Verification: User must check Phase Boot image checksums

2. **UEFI Firmware** (when Secure Boot enabled)
   - Assumption: Firmware is not compromised
   - Assumption: Firmware correctly verifies bootloader signature
   - Risk: Firmware rootkits (extremely rare but possible)
   - Mitigation: Physical security, firmware updates, vendor trust

3. **Verification Implementation** (`phase-verify`, `phase-fetch`)
   - Assumption: ed25519-dalek library is correct
   - Assumption: SHA256 implementation is correct
   - Assumption: No implementation bugs (timing, parsing, etc.)
   - Risk: Cryptographic bugs, side channels
   - Mitigation: Code review, fuzzing, formal verification (future)

4. **Build Process**
   - Assumption: Official Phase Boot images are built securely
   - Assumption: No backdoors in build toolchain
   - Risk: Compromised build infrastructure
   - Mitigation: Reproducible builds, checksums, attestation

### Untrusted Components

We **explicitly distrust**:

1. **Network Infrastructure**
   - DHT (malicious nodes expected)
   - DNS (MITM assumed possible)
   - Bootstrap nodes (untrusted by default)
   - Mirror servers (verify, not trust)

2. **Downloaded Content**
   - Manifests (verify signatures)
   - Artifacts (verify hashes)
   - DHT records (verify after retrieval)

3. **Local Cache** (Local Mode)
   - Cache may be tampered
   - Re-verify on every use
   - Do not trust cache metadata

4. **Physical Media** (after creation)
   - USB may be modified
   - ESP partition may be tampered
   - Secure Boot is the only defense

### Security Model

**Trust Anchor**: Ed25519 root public key embedded in `phase-verify` binary.

**Threat Model**: Byzantine adversary
- Arbitrary network control
- Physical access to boot media (after creation)
- Cannot compromise: UEFI firmware, embedded root key

**Security Goals** (in priority order):
1. **Integrity**: Boot only signed, verified code
2. **Availability**: Graceful degradation (fail closed)
3. **Privacy**: Minimize tracking (Private Mode)
4. **Auditability**: Log verification events

**Non-Goals**:
- Protection against compromised UEFI firmware
- Protection against physical access before first boot
- Protection against signing key compromise (detection only, no prevention)

---

## Defense in Depth

Phase Boot employs multiple overlapping security layers:

### Layer 1: Signature Verification

**Location**: `daemon/src/bin/phase_verify.rs:289-313`

**Mechanism**: Ed25519 signature over SHA256(manifest)
```rust
// 1. Decode signed data (base64)
let data = BASE64.decode(data_b64)?;

// 2. Hash the data (pre-hash signing)
let mut hasher = Sha256::new();
hasher.update(&data);
let hash = hasher.finalize();

// 3. Verify signature
key.verify(&hash, &signature)?;
```

**Properties**:
- Authenticity: Only holder of private key can sign
- Integrity: Any modification invalidates signature
- Non-repudiation: Signature proves origin

**Weaknesses**:
- Single key (no redundancy)
- No key rotation (old signatures valid forever)
- No timestamp verification (replay possible if no rollback protection)

---

### Layer 2: Hash Verification

**Location**: `daemon/src/bin/phase_fetch.rs:246-290`

**Mechanism**: SHA256 hash comparison
```rust
// 1. Download file, compute hash on-the-fly
let mut hasher = Sha256::new();
hasher.update(&chunk);
let computed_hash = hex::encode(hasher.finalize());

// 2. Compare against manifest hash
if computed_hash != expected_hash {
    return Err("Hash mismatch");
}
```

**Properties**:
- Content integrity: Detects any modification
- Signed commitment: Hash is in signed manifest
- Independent verification: Each artifact verified separately

**Weaknesses**:
- No encryption (content visible over HTTPS)
- Assumes SHA256 not broken (safe until ~2030+)

---

### Layer 3: Rollback Protection

**Location**: `daemon/src/bin/phase_verify.rs:140-164`

**Mechanism**: Monotonic version counter
```rust
let cached_version: u64 = fs::read_to_string(version_path)?;
if manifest.manifest_version < cached_version {
    return Err("Rollback detected");
}
```

**Properties**:
- Forward progress: Cannot downgrade to old versions
- Replay prevention: Old manifests rejected
- Simple: No complex logic

**Weaknesses**:
- Stateful: Requires persistent cache (Local Mode only)
- Trust on first use: No protection on fresh boot
- No expiration: Old manifests valid if version not cached

**Improvement Opportunity**:
```rust
// Embed minimum version in Phase Boot image
const MIN_MANIFEST_VERSION: u64 = 100;

if manifest.manifest_version < MIN_MANIFEST_VERSION {
    return Err("Manifest too old");
}
```

---

### Layer 4: Transport Security (HTTPS)

**Location**: Implicit in `daemon/src/bin/phase_fetch.rs` HTTP client

**Mechanism**: TLS certificate validation

**Properties**:
- Confidentiality: Encrypted transport
- Integrity: Detects MITM (via cert validation)
- Authentication: Verifies server identity

**Weaknesses**:
- Depends on CA trust (system root certs)
- Vulnerable to CA compromise (rare but possible)
- Does not prevent malicious server (hence hash verification)

**Defense in Depth Note**: HTTPS is redundant (hash verification sufficient), but provides:
- Early failure (cert error before download completes)
- Content confidentiality (artifact metadata not visible)

---

### Layer 5: Network Diversity

**Location**: Manifest `urls` array

**Mechanism**: Multiple mirror URLs
```json
{
  "kernel": {
    "urls": [
      "https://cdn1.phase.network/kernel.img",
      "https://cdn2.phase.network/kernel.img",
      "ipfs://Qm..."
    ]
  }
}
```

**Properties**:
- Availability: Fallback if primary mirror down
- Censorship resistance: Hard to block all mirrors
- Geographic diversity: Reduces single jurisdiction risk

**Weaknesses**:
- All mirrors may serve same malicious content (mitigated by hash verification)
- IPFS URLs may be slow or unavailable

---

### Layer 6: Ephemeral Identity (Private Mode)

**Location**: `daemon/src/bin/phase_discover.rs:88-101`

**Mechanism**: Disposable libp2p keypair
```rust
let local_key = if args.ephemeral {
    libp2p::identity::Keypair::generate_ed25519()  // Fresh each boot
} else {
    // Load from persistent storage (not in Private Mode)
};
```

**Properties**:
- Unlinkability: Each boot has different peer ID
- Privacy: Cannot track across sessions
- No persistence: No cache writes

**Weaknesses**:
- No reputation: Cannot build trust over time
- DHT performance: Cold start every boot (no routing table)
- Fingerprinting: Traffic patterns may still identify user

---

### Layer 7: Secure Boot (Future)

**Status**: Not implemented (M7 investigation ongoing)

**Mechanism**: UEFI signature verification
```
UEFI Firmware
    ↓ verifies signature
BOOTX64.EFI (signed by Microsoft UEFI CA or custom key)
    ↓ verified, executes
Kernel (signed, verified by shim)
    ↓ verified, executes
Phase Boot init
```

**Properties** (when implemented):
- Pre-boot verification: Protects before Phase code runs
- Chain of trust: Each stage verifies next
- Hardware-rooted: Firmware is root of trust

**Blockers**:
- Requires kernel signing (Microsoft UEFI CA or custom key)
- Requires shim bootloader (for custom key enrollment)
- Requires user UEFI configuration (enable Secure Boot)

---

## Known Limitations & Future Work

### L-1: No Secure Boot Integration

**Impact**: CRITICAL

**Description**:
Phase Boot currently requires Secure Boot to be disabled. This allows an attacker with physical access to modify the bootloader or kernel before any Phase Boot code executes, completely bypassing all security mechanisms.

**Current State**:
- User must disable Secure Boot in UEFI
- BOOTX64.EFI is unsigned
- Kernel is unsigned

**Attack Scenario**:
1. Attacker gains physical access to running machine
2. Reboots, modifies `/boot/EFI/BOOT/BOOTX64.EFI` on ESP
3. User reboots, malicious bootloader runs
4. Bootloader loads malicious kernel (bypasses phase-verify)

**Workarounds**:
- Physical security (don't leave USB unattended)
- Private Mode (limits impact, no persistent cache)
- Verify USB checksums regularly (detect tampering)

**Roadmap**:
- **M7**: Investigate Secure Boot requirements (current)
- **M8**: Sign kernel with Microsoft UEFI CA key (for compatibility)
- **M9**: Include shim bootloader (for custom key enrollment)
- **M10**: Document Secure Boot setup process

**Blockers**:
- Kernel signing costs money (Microsoft UEFI CA submission)
- Custom key enrollment is user-hostile (complex UEFI steps)
- Some hardware has buggy Secure Boot implementations

---

### L-2: Single Signing Key (No Key Rotation)

**Impact**: HIGH

**Description**:
Phase Boot uses a single Ed25519 private key to sign all manifests. If this key is compromised, the entire ecosystem is compromised with no recovery mechanism.

**Current State**:
- One root key (hardcoded in `phase-verify`)
- No key rotation protocol
- No key revocation mechanism
- No key expiration

**Attack Scenario**:
1. Attacker compromises build server
2. Extracts signing key from memory/disk
3. Signs malicious manifests indefinitely
4. No detection, no recovery

**Workarounds**:
- Store key in HSM (hardware security module)
- Limit key access (only signing server, no network access)
- Monitor manifest signatures (manual comparison)

**Roadmap**:
- **M8**: HSM integration (AWS KMS or YubiHSM)
- **M9**: Key rotation protocol (dual-key overlap period)
- **M10**: Manifest transparency log (detect unauthorized signatures)
- **M11**: Threshold signatures (3-of-5 quorum for signing)

**Design Sketch** (key rotation):
```json
{
  "manifest_version": 200,
  "signing_keys": [
    {
      "keyid": "old-key-2024",
      "expires": "2025-12-31T23:59:59Z",
      "pubkey": "abc123..."
    },
    {
      "keyid": "new-key-2025",
      "valid_from": "2025-11-01T00:00:00Z",
      "pubkey": "def456..."
    }
  ],
  "signatures": [
    {"keyid": "old-key-2024", "sig": "..."},
    {"keyid": "new-key-2025", "sig": "..."}
  ]
}
```

---

### L-3: No Manifest Expiration Enforcement

**Impact**: MEDIUM

**Description**:
Manifests have an `expires_at` field, but it is not enforced. Old manifests remain valid indefinitely, enabling long-term replay attacks.

**Current State**:
```rust
// daemon/src/bin/phase_verify.rs:61
#[serde(default)]
expires_at: Option<String>,  // Present but not checked
```

**Attack Scenario**:
1. Attacker caches legitimate manifest from 2024
2. In 2027, serves cached manifest (replay attack)
3. [BLOCKED IF] User booted recently (rollback protection)
4. [SUCCEEDS IF] User's first boot, or Private Mode (no cache)

**Workarounds**:
- Use rollback protection (Local Mode only)
- Embed minimum version in Phase Boot image

**Roadmap**:
- **M8**: Enforce expiration (reject manifests older than 90 days)
- **M9**: Online freshness oracle (signed timestamp service)
- **M10**: Gossip protocol (compare manifests with peers)

**Implementation**:
```rust
if let Some(expires_str) = &manifest.expires_at {
    let expires = DateTime::parse_from_rfc3339(expires_str)?;
    if Utc::now() > expires {
        return Err("Manifest expired");
    }
}
```

**Blocker**: Requires accurate system clock (may not be available at boot).

---

### L-4: Cache Security (Local Mode)

**Impact**: MEDIUM (future feature)

**Description**:
Local Mode will cache artifacts to disk, but cache security design is not yet finalized. Risk of cache poisoning or tampering.

**Questions**:
- Is cache re-verified on read?
- Is cache authenticated (AEAD)?
- Is cache bound to specific USB device?
- How is cache invalidated?

**Attack Scenarios**:
1. **Cache poisoning**: Attacker pre-fills cache with malicious artifacts (different USB)
2. **Cache tampering**: Attacker modifies cached artifacts (evil maid)
3. **Cache confusion**: Cache from one channel used for another (stable vs testing)

**Required Mitigations**:
1. **MUST** re-verify SHA256 on cache reads (trust nothing)
2. **SHOULD** use authenticated encryption (AES-GCM, key derived from USB serial)
3. **SHOULD** version cache format (detect schema changes)
4. **SHOULD** namespace cache by channel/arch (prevent confusion)

**Design Principles**:
```
Cache is a performance optimization, not a trust boundary.
Everything from cache MUST be verified as if downloaded fresh.
```

---

### L-5: No Attestation or Transparency

**Impact**: MEDIUM

**Description**:
No mechanism to detect if Phase is serving different manifests to different users (targeted attacks), or if signing key is compromised.

**Attack Scenario**:
1. Attacker compromises signing key
2. Serves malicious manifest to targeted users (specific IPs)
3. Serves legitimate manifest to monitoring infrastructure
4. No way to detect the divergence

**Workarounds**:
- Manual manifest comparison (out-of-band)
- Gossip with trusted peers (compare what you got)

**Roadmap**:
- **M10**: Manifest transparency log (append-only, Merkle tree)
- **M11**: Gossip protocol (compare manifests with random peers)
- **M12**: Remote attestation (prove what manifest you booted)

**Inspiration**:
- Certificate Transparency (Google)
- Sigstore/Rekor (CNCF)
- Update Framework (TUF)

---

### L-6: Bootstrap Node Trust

**Impact**: MEDIUM

**Description**:
DHT discovery depends on bootstrap nodes (`daemon/src/bin/phase_discover.rs:255`). Malicious bootstrap nodes can eclipse users from the network.

**Current State**:
- Default bootstrap nodes: Empty (TODO comment)
- User provides via `--bootstrap` flag
- No authentication of bootstrap nodes

**Attack Scenario**:
1. User configured to use evil-bootstrap.com
2. Bootstrap node returns only attacker-controlled peers
3. DHT queries return malicious manifest URLs
4. [BLOCKED] Signature verification fails
5. [RESULT] Denial of service

**Workarounds**:
- Multiple independent bootstrap nodes
- mDNS fallback (local network)
- Hardcoded fallback manifest URLs

**Roadmap**:
- **M8**: Authenticated bootstrap (bootstrap nodes sign responses)
- **M9**: Bootstrap node diversity (multiple trust domains)
- **M10**: Fallback to HTTPS manifest URLs (no DHT)

---

### L-7: Minimal Audit Logging

**Impact**: LOW

**Description**:
Security events are logged to stdout but not persisted. No forensic evidence if attack occurs.

**Current State**:
- `tracing::info!()` calls in verification code
- Logs disappear on reboot (no persistent storage in Private Mode)
- No structured logging (JSON)

**Missing Events**:
- Signature verification failures (who, when, what manifest)
- Rollback detection (what version, when)
- Download failures (which mirrors, why)
- DHT query results (who returned what)

**Roadmap**:
- **M8**: Structured JSON logging
- **M9**: Optional remote syslog (Local Mode only)
- **M10**: Signed audit logs (tamper-evident)

---

## Security Recommendations

### For Users

1. **Enable Secure Boot** (when supported)
   - Verify UEFI settings before boot
   - Enroll Phase Boot signing key if using custom builds

2. **Verify USB Image Checksums**
   - Download checksums over HTTPS from phase.network
   - Verify SHA256 before writing USB: `sha256sum phase-boot.img`

3. **Use Private Mode** for sensitive environments
   - No persistent cache (ephemeral)
   - Fresh identity each boot
   - No write to USB (tamper evidence preserved)

4. **Physical Security**
   - Do not leave USB unattended (evil maid risk)
   - Use tamper-evident bags for transport
   - Regularly verify USB checksums (detect tampering)

5. **Network Security**
   - Prefer Local Mode on untrusted networks
   - Use VPN if available (defense in depth)
   - Monitor for unexpected manifest versions

### For Developers

1. **Signing Key Security**
   - Store in HSM (AWS KMS, YubiHSM, or dedicated hardware)
   - Never expose key on network-connected machines
   - Implement key rotation (every 90 days)
   - Plan for key compromise (incident response)

2. **Implement Missing Security Features**
   - Secure Boot support (priority 1)
   - Manifest expiration enforcement (priority 2)
   - Transparency log (priority 3)
   - Threshold signatures (priority 4)

3. **Security Testing**
   - Fuzz verification code (`phase-verify`, `phase-fetch`)
   - Penetration test DHT discovery (eclipse attacks)
   - Code review signature verification (formal methods)
   - Test rollback protection edge cases

4. **Incident Response Plan**
   - Key compromise: Emergency revocation mechanism
   - Build compromise: Reproducible build verification
   - Manifest poisoning: Transparency log audit

5. **Supply Chain Security**
   - Reproducible builds (bit-for-bit determinism)
   - Build provenance attestation (SLSA)
   - Dependency pinning and auditing
   - Insider threat controls (multi-party signing)

### For Operators (Mirror/Bootstrap Node)

1. **Mirror Operators**
   - Use HTTPS (valid certificates)
   - Log access patterns (detect abuse)
   - Report to transparency log (future)

2. **Bootstrap Node Operators**
   - Serve diverse DHT records (censorship resistance)
   - Do not log peer IDs (privacy)
   - Participate in gossip (detect poisoning)

---

## Conclusion

Phase Boot implements a robust chain-of-trust based on Ed25519 signatures and SHA256 hash verification. The core verification logic is sound and resistant to network-level attacks.

**Strong Security Properties**:
- ✅ Manifest authenticity (Ed25519 signatures)
- ✅ Artifact integrity (SHA256 hashes)
- ✅ Rollback protection (Local Mode)
- ✅ Network diversity (multiple mirrors)

**Critical Gaps** (require mitigation before production):
- ❌ No Secure Boot (physical attack vector)
- ❌ Single signing key (no recovery from compromise)
- ⚠️  No manifest expiration (long-term replay possible)
- ⚠️  No transparency (targeted attacks undetectable)

**Recommended Priority Order**:
1. **P0**: Secure Boot integration (blocks pre-boot attacks)
2. **P0**: HSM for signing key (reduces compromise risk)
3. **P1**: Key rotation protocol (enables recovery)
4. **P1**: Manifest expiration enforcement (prevents replay)
5. **P2**: Transparency log (detects targeted attacks)
6. **P2**: Cache security hardening (for Local Mode)
7. **P3**: Threshold signatures (eliminates single point of failure)

**Risk Acceptance**:
For development/testing, current security is adequate. For production deployment, P0 and P1 items are **required**.

**Contact**: Security issues should be reported privately to security@phase.network.

---

**Document Version**: 1.0
**Last Updated**: 2025-11-26
**Next Review**: After M8 (Secure Boot implementation)
