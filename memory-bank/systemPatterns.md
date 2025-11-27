# System Patterns: Phase Architecture

**Last Updated**: 2025-11-26
**Version**: 1.1
**Status**: MVP Complete + Phase Boot Implemented

---

## Table of Contents

1. [Core Architecture](#core-architecture)
2. [Library + Binary Pattern](#library--binary-pattern)
3. [WASM Execution Pattern](#wasm-execution-pattern)
4. [Peer Discovery Pattern](#peer-discovery-pattern)
5. [Job Lifecycle Pattern](#job-lifecycle-pattern)
6. [Security & Sandboxing](#security--sandboxing)
7. [Data Flow Patterns](#data-flow-patterns)
8. [Error Handling](#error-handling)
9. [Testing Patterns](#testing-patterns)
10. [Phase Boot Patterns](#phase-boot-patterns)

---

## Core Architecture

### Layered Architecture

```
┌─────────────────────────────────────────┐
│         Client SDKs (PHP, etc.)         │
├─────────────────────────────────────────┤
│       Phase Protocol (Manifests)        │
├─────────────────────────────────────────┤
│      libp2p (DHT, QUIC, Noise)          │
├─────────────────────────────────────────┤
│      Plasm Runtime (Rust Daemon)        │
├─────────────────────────────────────────┤
│       WASM Runtime (wasm3/wasmtime)     │
└─────────────────────────────────────────┘
```

**Pattern**: Clear separation of concerns
- **Protocol layer**: Defines messages, schemas, handshakes
- **Transport layer**: Handles encrypted peer-to-peer communication
- **Runtime layer**: Executes jobs in sandboxed environment
- **Client layer**: Provides language-specific bindings

**When to Use**: Always maintain these boundaries. Don't mix protocol logic with transport or execution.

---

## Library + Binary Pattern

### Rust Crate Structure

**Pattern**: Structure Rust projects as reusable library with thin binary wrapper

```
daemon/
├── Cargo.toml          # Defines both [lib] and [[bin]]
├── src/
│   ├── lib.rs          # Library crate (public API)
│   ├── main.rs         # Binary crate (thin wrapper)
│   ├── config.rs       # Public module
│   ├── wasm/           # Public module
│   │   ├── mod.rs
│   │   ├── runtime.rs
│   │   ├── manifest.rs
│   │   └── receipt.rs
│   └── network/        # Public module
│       ├── mod.rs
│       ├── discovery.rs
│       ├── protocol.rs
│       └── execution.rs
└── tests/              # Integration tests
```

**Cargo.toml Configuration**:
```toml
[lib]
name = "plasm"
path = "src/lib.rs"

[[bin]]
name = "plasmd"
path = "src/main.rs"
```

**Library Definition (src/lib.rs)**:
```rust
//! # Plasm - Phase Local WASM Execution Daemon Library
//!
//! This library provides core functionality for the Phase distributed compute network.

// Public module exports
pub mod config;
pub mod wasm;
pub mod network;

// Convenience re-exports (flat namespace)
pub use config::{Config, ExecutionLimits};
pub use wasm::{
    runtime::{WasmRuntime, Wasm3Runtime, ExecutionResult},
    manifest::JobManifest,
    receipt::Receipt,
};
pub use network::{
    Discovery,
    DiscoveryConfig,
    PeerInfo,
    ExecutionHandler,
    protocol::{JobOffer, JobResponse, JobRequest, JobResult},
};
```

**Binary Usage (src/main.rs)**:
```rust
// Use library as external dependency
use plasm::{
    network::{Discovery, ExecutionHandler},
    wasm::runtime::Wasm3Runtime,
};

fn main() {
    // Binary is thin wrapper around library functionality
    let runtime = Wasm3Runtime::new().build().unwrap();
    // ...
}
```

**Key Principles**:
- **Library-first**: Core functionality in `lib.rs`, binary is thin CLI wrapper
- **Public API**: Expose clean interfaces via `pub mod` and re-exports
- **Flat namespace**: Common types available at crate root (e.g., `plasm::Config`)
- **Zero suppressions**: No `#[allow(dead_code)]` needed - code is public API
- **Reusability**: Other Rust projects can use as library

**Benefits**:

| Aspect | Without Pattern | With Pattern |
|--------|-----------------|--------------|
| **Warnings** | "unused" warnings everywhere | Zero warnings |
| **Suppressions** | `#[allow(dead_code)]` needed | None needed |
| **Reusability** | Binary-only, not reusable | Full library API |
| **Testing** | Integration tests awkward | Clean library tests |
| **Documentation** | None | rustdoc available |
| **API Clarity** | Internal modules only | Clear public API |

**When to Use**:
- ✅ **ALWAYS** - Apply from day 1, not as refactor
- ✅ Any Rust project with substantial functionality
- ✅ When code will be tested independently
- ✅ When other projects might reuse functionality
- ❌ Never for truly trivial single-file binaries

**Examples in the Wild**:
- **ripgrep**: Library (`grep` crate) + Binary (`rg`)
- **tokio**: Library (`tokio` crate) + Runtime
- **clap**: Library (`clap` crate) + Derive macros
- **serde**: Library (`serde` crate) + Serialization framework

**External Integration Example**:
```rust
// In another Rust project
[dependencies]
plasm = { path = "../phase/daemon" }

// Use library
use plasm::{JobManifest, WasmRuntime, Wasm3Runtime};

let runtime = Wasm3Runtime::new().build()?;
let manifest = JobManifest::from_file("job.json")?;
let result = runtime.execute(&wasm_bytes, &[])?;
```

**Historical Context**:
- Discovered during Milestone 4 (November 2025)
- Initially structured as binary-only, 27 "unused" warnings
- Refactored to library + binary pattern, warnings eliminated
- Zero performance overhead, zero build time increase
- Documented in `memory-bank/tasks/2025-11/091109_library_binary_refactor.md`

**Reference**: `memory-bank/quick-start.md#Architecture: Library + Binary Pattern`

---

## WASM Execution Pattern

### Sandboxed Execution Model

**Pattern**: WASM-only, sandboxed execution with explicit capabilities

```rust
// Pattern: Load WASM module
let wasm_bytes = fs::read(&wasm_path)?;
let module_hash = hash_module(&wasm_bytes);

// Pattern: Initialize runtime with constraints
let runtime = Wasm3Runtime::new()
    .with_memory_limit(manifest.memory_mb * 1024 * 1024)
    .with_cpu_quota(manifest.cpu_seconds)
    .with_timeout(manifest.timeout_seconds)
    .build()?;

// Pattern: Execute and capture output
let result = runtime.execute(&wasm_bytes, &args)?;
let stdout = result.stdout;
let stderr = result.stderr;
let exit_code = result.exit_code;

// Pattern: Generate receipt
let receipt = Receipt {
    module_hash,
    wall_time_ms: result.wall_time,
    cpu_time_ms: result.cpu_time,
    exit_code,
    signature: sign_receipt(&receipt_data, &node_key),
};
```

**Key Principles**:
- Default-deny: No host access unless explicitly granted
- Resource limits: Memory, CPU, timeout all constrained
- Deterministic hashing: Module hash for verification
- Signed receipts: Cryptographic proof of execution

**When to Use**: Every WASM job execution. Never bypass sandbox.

---

## Peer Discovery Pattern

### Kademlia DHT Discovery

**Pattern**: Anonymous peer discovery using libp2p Kademlia DHT

```rust
// Pattern: Initialize libp2p with Kademlia
let local_key = Keypair::generate_ed25519();
let local_peer_id = PeerId::from(local_key.public());

let transport = build_quic_transport(&local_key)?; // QUIC + Noise
let behaviour = Kademlia::new(local_peer_id.clone());

let swarm = Swarm::new(transport, behaviour, local_peer_id);

// Pattern: Bootstrap to DHT
for bootstrap_addr in config.bootstrap_nodes {
    swarm.behaviour_mut().add_address(&bootstrap_peer_id, bootstrap_addr);
}
swarm.behaviour_mut().bootstrap()?;

// Pattern: Advertise capabilities
let capability_key = format!("/phase/capability/{}", capability_id);
swarm.behaviour_mut().start_providing(capability_key.into_bytes())?;

// Pattern: Discover peers with capability
swarm.behaviour_mut().get_providers(capability_key.into_bytes());
```

**Key Principles**:
- Anonymous: No identity required, ephemeral peer IDs
- Decentralized: No central bootstrap (configurable bootstrap nodes)
- Capability-based: Discover peers by advertised capabilities
- Persistent peerstore: Cache discovered peers for faster reconnection

**When to Use**:
- Node startup: Bootstrap to DHT
- Job submission: Discover peers with required capabilities
- Periodic: Re-advertise capabilities (every 30min)

---

## Job Lifecycle Pattern

### End-to-End Job Flow

**Pattern**: Client → Discovery → Handshake → Execution → Receipt

```
1. CLIENT: Create manifest + WASM payload
   ├─ Manifest: {"cpu": 1, "memory_mb": 128, "timeout_sec": 30}
   └─ Payload: hello.wasm (module bytes)

2. DISCOVERY: Find capable peer via DHT
   ├─ Query: /phase/capability/wasm3-x86_64
   └─ Response: [peer_id_1, peer_id_2, ...]

3. HANDSHAKE: Negotiate job acceptance
   ├─ CLIENT → NODE: JobRequest {manifest, module_hash}
   ├─ NODE → CLIENT: JobAccepted {job_id, estimated_start}
   └─ Or: JobRejected {reason}

4. TRANSMISSION: Send WASM payload
   ├─ CLIENT → NODE: Stream WASM bytes over libp2p
   └─ NODE: Validate hash matches manifest

5. EXECUTION: Run job in sandbox
   ├─ Load WASM into runtime
   ├─ Apply resource limits
   ├─ Execute and capture stdout/stderr
   └─ Generate signed receipt

6. RESULT: Return output + receipt
   ├─ NODE → CLIENT: JobResult {stdout, stderr, exit_code, receipt}
   └─ CLIENT: Verify signature, validate receipt

7. CLEANUP: Ephemeral state disposal
   ├─ Delete WASM module
   ├─ Clear runtime memory
   └─ Log event for audit
```

**Key Principles**:
- Manifest-first: Always define resources before transmission
- Hash verification: Validate module integrity
- Signed receipts: Cryptographic proof of execution
- Stateless nodes: No persistent job storage (ephemeral only)

**When to Use**: Every job submission and execution cycle

---

## Security & Sandboxing

### Defense-in-Depth Model

**Pattern**: Multiple layers of isolation and verification

```
┌─────────────────────────────────────────┐
│  Layer 1: Network (Encrypted QUIC)     │  ← Noise protocol, TLS-like security
├─────────────────────────────────────────┤
│  Layer 2: Process (Daemon isolation)   │  ← systemd service, limited perms
├─────────────────────────────────────────┤
│  Layer 3: WASM Sandbox                 │  ← No syscalls, no file/network access
├─────────────────────────────────────────┤
│  Layer 4: Resource Limits              │  ← Memory, CPU, timeout enforcement
└─────────────────────────────────────────┘
```

**Security Checklist** (every execution):
- [ ] WASM module hash verified before execution
- [ ] Resource limits enforced (memory, CPU, timeout)
- [ ] No host filesystem access (sandboxed)
- [ ] No network access from WASM
- [ ] No syscall access from WASM
- [ ] Receipt signed with node's private key
- [ ] Client verifies receipt signature before trusting result

**When to Use**: Always. Never bypass any security layer.

---

## Data Flow Patterns

### Manifest Schema

**Pattern**: Declare job requirements upfront

```json
{
  "version": "0.1",
  "module_hash": "sha256:abc123...",
  "resources": {
    "cpu_cores": 1,
    "memory_mb": 128,
    "timeout_seconds": 30
  },
  "capabilities": ["wasm3", "x86_64"],
  "priority": "normal"
}
```

**Key Fields**:
- `module_hash`: Cryptographic hash for verification
- `resources`: Explicit resource requirements
- `capabilities`: Required node features (runtime, arch)
- `priority`: Scheduling hint (normal/low/high)

### Receipt Schema

**Pattern**: Cryptographic proof of execution

```json
{
  "version": "0.1",
  "job_id": "uuid-here",
  "module_hash": "sha256:abc123...",
  "node_peer_id": "12D3KooW...",
  "execution": {
    "wall_time_ms": 1234,
    "cpu_time_ms": 987,
    "memory_peak_mb": 45,
    "exit_code": 0
  },
  "timestamp": "2025-11-08T12:00:00Z",
  "signature": "base64-signature-here"
}
```

**Key Fields**:
- `module_hash`: Links execution to specific WASM module
- `node_peer_id`: Anonymous node identifier
- `execution`: Measured resource usage
- `signature`: Ed25519 signature over receipt fields

**Verification**:
```rust
// Pattern: Verify receipt signature
let receipt_bytes = serialize_receipt_for_signing(&receipt);
let signature = Signature::from_bytes(&receipt.signature)?;
let public_key = PublicKey::from_peer_id(&receipt.node_peer_id)?;
public_key.verify(&receipt_bytes, &signature)?;
```

---

## Error Handling

### Graceful Degradation Pattern

**Pattern**: Fail gracefully, provide actionable errors

```rust
// Pattern: Result-based error handling
pub enum ExecutionError {
    ModuleLoadFailed { path: String, reason: String },
    ResourceLimitExceeded { resource: String, limit: u64, actual: u64 },
    TimeoutExceeded { timeout_sec: u64 },
    RuntimeError { message: String },
    SignatureVerificationFailed { reason: String },
}

// Pattern: Error context
impl ExecutionError {
    pub fn context(&self) -> String {
        match self {
            Self::ModuleLoadFailed { path, reason } =>
                format!("Failed to load WASM module '{}': {}", path, reason),
            Self::ResourceLimitExceeded { resource, limit, actual } =>
                format!("{} limit exceeded: {} > {} allowed", resource, actual, limit),
            // ...
        }
    }
}

// Pattern: Logging at boundaries
log::error!("Execution failed: {}", error.context());
```

**Key Principles**:
- Typed errors: Use enums for different error cases
- Context-rich: Include actionable information
- Log at boundaries: Integration points, not internal logic
- Never panic: Use `Result<T, E>` everywhere

**When to Use**: All fallible operations. No unwraps in production code.

---

## Testing Patterns

### Test Structure

**Pattern**: Unit, integration, and cross-architecture tests

```rust
// Pattern: Unit test for WASM execution
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wasm_execution_success() {
        let runtime = Wasm3Runtime::new().build().unwrap();
        let wasm_bytes = include_bytes!("../fixtures/hello.wasm");
        let result = runtime.execute(wasm_bytes, &[]).unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("Hello"));
    }

    #[test]
    fn test_memory_limit_enforcement() {
        let runtime = Wasm3Runtime::new()
            .with_memory_limit(1024) // 1KB
            .build()
            .unwrap();
        let wasm_bytes = include_bytes!("../fixtures/memory_hog.wasm");
        let result = runtime.execute(wasm_bytes, &[]);

        assert!(matches!(result, Err(ExecutionError::ResourceLimitExceeded { .. })));
    }
}
```

**Pattern**: Integration test for peer discovery
```rust
#[tokio::test]
async fn test_peer_discovery() {
    let node1 = spawn_test_node().await;
    let node2 = spawn_test_node().await;

    // Node1 advertises capability
    node1.advertise("wasm3-x86_64").await.unwrap();

    // Node2 discovers Node1
    tokio::time::sleep(Duration::from_secs(2)).await;
    let peers = node2.discover("wasm3-x86_64").await.unwrap();

    assert!(peers.contains(&node1.peer_id()));
}
```

**Test Principles**:
- Deterministic: No flaky tests (fixed seeds, timeouts, retries)
- Independent: No shared state between tests
- Fast: Unit tests < 100ms, integration < 5s
- Clear names: `test_<scenario>_<expected_outcome>`

**When to Use**:
- Unit tests: All core logic (WASM runtime, manifest parsing, receipt signing)
- Integration tests: Peer discovery, job lifecycle, end-to-end flows
- Cross-arch tests: macOS ARM client → Ubuntu x86_64 node (manual demo)

---

## Phase Boot Patterns

### Boot Flow Pattern

**Pattern**: Multi-stage boot with mode-based behavior

```
UEFI Firmware
    ↓
systemd-boot / GRUB
    ↓ (selects boot entry by mode)
Kernel + Initramfs
    ↓ (phase.mode=internet|local|private)
/init (PID 1)
    ↓ mounts /proc, /sys, /dev
    ↓ parses phase.mode from cmdline
    ↓ brings up network (DHCP)
    ↓ discovers manifest (DHT/mDNS)
    ↓ verifies + fetches artifacts
    ↓ kexec into target kernel
Target System (plasm daemon)
```

**Key Principles**:
- Mode parsed from kernel cmdline (`phase.mode=`)
- Each mode has distinct behavior (internet, local, private)
- Discovery before fetch, verify before execute
- kexec for fast kernel switch without firmware

### Boot Mode Pattern

**Pattern**: Three modes with increasing privacy

| Mode | Network | Discovery | Persistence | Identity |
|------|---------|-----------|-------------|----------|
| Internet | Full | DHT | Write cache | Stable |
| Local | LAN only | mDNS | Use cache | Stable |
| Private | Optional Tor | DHT (ephemeral) | No writes | Ephemeral |

**Implementation**:
```bash
# boot/initramfs/scripts/mode-handler.sh
case "$mode" in
    internet)
        phase-discover --channel "$channel" --arch "$arch"
        ;;
    local)
        # Try cache first, fall back to mDNS
        if [ -f "$CACHE/manifest.json" ]; then
            use_cached_manifest
        else
            phase-discover --mdns-only
        fi
        ;;
    private)
        phase-discover --ephemeral --channel "$channel"
        ;;
esac
```

### Verification Pipeline Pattern

**Pattern**: Verify before trust

```
1. Discover manifest URL (DHT/mDNS)
      ↓
2. Fetch manifest JSON
      ↓
3. Verify Ed25519 signature (phase-verify)
   - Load embedded root public key
   - Verify signature over manifest
   - Check rollback protection (version >= cached)
      ↓
4. Fetch artifacts (phase-fetch)
   - Try each URL in manifest
   - Streaming SHA256 verification
   - Verify size matches manifest
      ↓
5. kexec into verified kernel
```

**Key Principles**:
- Root public key embedded in binary (daemon/keys/root.pub.placeholder)
- Manifest includes version for rollback protection
- Artifacts verified by content hash (SHA256)
- Multi-URL fallback for reliability

### kexec Handoff Pattern

**Pattern**: Replace running kernel without firmware

```bash
# boot/initramfs/scripts/kexec-boot.sh

# 1. Load current kernel params
CURRENT_CMDLINE=$(cat /proc/cmdline)

# 2. Build final cmdline (preserve phase.*)
FINAL_CMDLINE="root=/dev/ram0 phase.mode=$mode $CURRENT_CMDLINE"

# 3. Load new kernel
kexec -l "$KERNEL" --initrd="$INITRAMFS" --command-line="$FINAL_CMDLINE"

# 4. Execute (point of no return)
kexec -e
```

**Key Principles**:
- Preserve kernel cmdline parameters across kexec
- Include phase.mode in new cmdline
- kexec -l loads, kexec -e executes
- No BIOS/firmware involvement after initial boot

### OverlayFS Pattern

**Pattern**: Writable layer over read-only rootfs

```
┌─────────────────────────────────────┐
│     Merged View (/newroot)          │  ← Applications see this
├─────────────────────────────────────┤
│  Upper (tmpfs) - Changes written    │  ← Ephemeral writes
├─────────────────────────────────────┤
│  Lower (squashfs) - Verified rootfs │  ← Read-only, verified
└─────────────────────────────────────┘
```

**Implementation** (`overlayfs-setup.sh`):
```bash
# Mount verified rootfs as lower
mount -t squashfs "$ROOTFS" /lower -o ro

# Create tmpfs for upper/work
mount -t tmpfs tmpfs /overlay
mkdir /overlay/upper /overlay/work

# Create merged view
mount -t overlay overlay /newroot \
    -o lowerdir=/lower,upperdir=/overlay/upper,workdir=/overlay/work
```

**Key Principles**:
- Lower layer is cryptographically verified
- Upper layer is tmpfs (lost on reboot in private mode)
- Merged view provides normal filesystem semantics
- No writes to verified artifacts

### Build System Pattern

**Pattern**: Make-based cross-architecture build

```makefile
# boot/Makefile targets

# Architecture-specific
make ARCH=x86_64  # Default
make ARCH=arm64   # ARM64 build

# Components
make esp          # EFI System Partition
make initramfs    # Initramfs image
make rootfs       # SquashFS rootfs
make image        # Full USB image

# Testing
make test-qemu-x86  # QEMU test
make test-qemu-arm  # ARM64 QEMU test
```

**Key Principles**:
- Single Makefile orchestrates all builds
- ARCH variable selects target architecture
- Components buildable independently
- QEMU targets for easy testing

---

## Netboot Provider Patterns

### HTTP Artifact Server Pattern

**Pattern**: axum-based HTTP server for boot artifact serving

```rust
// daemon/src/provider/server.rs
use axum::{Router, routing::get, extract::State};
use std::sync::Arc;

struct AppState {
    config: ProviderConfig,
    artifacts: Arc<ArtifactStore>,
    metrics: Arc<ProviderMetrics>,
    start_time: Instant,
}

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(info_handler))
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/manifest.json", get(manifest_handler))
        .route("/:channel/:arch/manifest.json", get(channel_manifest_handler))
        .route("/:channel/:arch/:artifact", get(artifact_handler))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
```

**Key Principles**:
- Shared state via `Arc<AppState>`
- Health endpoint returns 200/503 for load balancers
- Artifact endpoints support HTTP Range requests
- Metrics tracking (requests, bytes served)

### Boot Manifest Schema Pattern

**Pattern**: Signed manifest describing boot artifacts

```rust
// daemon/src/provider/manifest.rs
#[derive(Serialize, Deserialize)]
pub struct BootManifest {
    pub manifest_version: u32,        // Always 1
    pub version: String,              // e.g., "0.1.0"
    pub channel: String,              // "stable", "testing"
    pub arch: String,                 // "aarch64", "x86_64"
    pub created_at: String,           // ISO 8601
    pub expires_at: String,           // ISO 8601
    pub artifacts: HashMap<String, ArtifactInfo>,
    pub signatures: Vec<Signature>,
    pub provider: Option<ProviderInfo>,
}

#[derive(Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub filename: String,
    pub size_bytes: u64,
    pub hash: String,                 // "sha256:hexdigest"
    pub download_url: Option<String>, // Relative path
}
```

**Key Principles**:
- Manifest version for forward compatibility
- Expiration for cache control
- Artifacts keyed by name (kernel, initramfs, rootfs)
- Ed25519 signatures for authenticity

### Manifest Signing Pattern

**Pattern**: Ed25519 signatures over manifest content

```rust
// daemon/src/provider/signing.rs
use ed25519_dalek::{SigningKey, Signer};
use sha2::{Sha256, Digest};

pub fn sign_manifest(manifest: &mut BootManifest, key: &SigningKey) -> Result<()> {
    // 1. Hash manifest without signatures
    let mut manifest_for_hash = manifest.clone();
    manifest_for_hash.signatures.clear();
    let json = serde_json::to_string(&manifest_for_hash)?;

    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let hash = hasher.finalize();

    // 2. Sign the hash
    let signature = key.sign(&hash);

    // 3. Add signature to manifest
    manifest.signatures.push(Signature {
        algorithm: "ed25519".to_string(),
        key_id: hex::encode(key.verifying_key().as_bytes()),
        signature: hex::encode(signature.to_bytes()),
        signed_at: chrono::Utc::now().to_rfc3339(),
    });

    Ok(())
}
```

**Key Principles**:
- Sign hash of manifest JSON (excluding signatures field)
- Include key_id for multi-key support
- Timestamp for audit trail
- Compatible with phase-verify binary

### DHT Advertisement Pattern

**Pattern**: Publish boot manifest location to Kademlia DHT

```rust
// daemon/src/provider/dht.rs
use libp2p::kad::RecordKey;

#[derive(Serialize, Deserialize)]
pub struct ManifestRecord {
    pub channel: String,
    pub arch: String,
    pub manifest_url: String,      // Full URL to manifest
    pub http_addr: String,         // Provider HTTP address
    pub manifest_version: String,
    pub created_at: String,
    pub ttl_secs: u64,             // Default: 3600 (1 hour)
}

impl ManifestRecord {
    /// DHT key format: /phase/{channel}/{arch}/manifest
    pub fn dht_key(channel: &str, arch: &str) -> RecordKey {
        let key_str = format!("/phase/{}/{}/manifest", channel, arch);
        RecordKey::new(&key_str.into_bytes())
    }
}

// In Discovery::publish_manifest_record()
let key = record.key();
let value = record.to_bytes()?;
swarm.behaviour_mut().kademlia.put_record(
    Record::new(key, value),
    Quorum::One,
)?;
```

**Key Principles**:
- DHT key includes channel and arch for targeted discovery
- Record contains HTTP URL, not full manifest
- TTL for automatic expiration
- Refresh at half-TTL interval

### Architecture Aliasing Pattern

**Pattern**: Handle arm64/aarch64 and amd64/x86_64 naming variants

```rust
// daemon/src/provider/artifacts.rs
impl ArtifactStore {
    fn arch_aliases(arch: &str) -> Vec<&str> {
        match arch {
            "aarch64" => vec!["aarch64", "arm64"],
            "arm64" => vec!["arm64", "aarch64"],
            "x86_64" => vec!["x86_64", "amd64"],
            "amd64" => vec!["amd64", "x86_64"],
            other => vec![other],
        }
    }

    pub fn get_artifact_path(&self, channel: &str, arch: &str, name: &str) -> Option<PathBuf> {
        for arch_variant in Self::arch_aliases(arch) {
            let path = self.base_dir.join(channel).join(arch_variant).join(name);
            if path.exists() {
                return Some(path);
            }
        }
        None
    }
}
```

**Key Principles**:
- Server auto-detects as "aarch64" or "x86_64" (Rust convention)
- Artifacts may be stored as "arm64" or "amd64" (Linux convention)
- Try all aliases transparently
- No user configuration required

### HTTP Range Request Pattern

**Pattern**: Support partial content downloads for large artifacts

```rust
// daemon/src/provider/server.rs
async fn artifact_handler(
    headers: HeaderMap,
    // ...
) -> impl IntoResponse {
    if let Some(range_header) = headers.get(header::RANGE) {
        // Parse "bytes=start-end" or "bytes=start-"
        if let Some((start, end)) = parse_range(range_header, file_size) {
            // Seek and read partial content
            let mut file = File::open(&path).await?;
            file.seek(SeekFrom::Start(start)).await?;
            let content = file.take(end - start + 1);

            return (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_RANGE, format!("bytes {}-{}/{}", start, end, file_size)),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_LENGTH, (end - start + 1).to_string()),
                ],
                body
            ).into_response();
        }
        // Invalid range: 416 Range Not Satisfiable
    }
    // No range: 200 OK with full content
}
```

**Key Principles**:
- Enables resumable downloads
- Required for large boot artifacts (kernels, rootfs)
- Standard HTTP/1.1 Range semantics (RFC 7233)
- Always include `Accept-Ranges: bytes` header

---

## Anti-Patterns (Avoid These)

### ❌ Centralized Discovery
**Bad**: Single bootstrap node or central registry
**Good**: Configurable bootstrap nodes, Kademlia DHT

### ❌ Unsandboxed Execution
**Bad**: Running arbitrary code with host access
**Good**: WASM-only, default-deny sandbox

### ❌ Unsigned Receipts
**Bad**: Trusting execution results without verification
**Good**: Cryptographic signatures on all receipts

### ❌ Blocking I/O in Event Loop
**Bad**: Synchronous file/network I/O in async runtime
**Good**: Async I/O everywhere, spawn_blocking for CPU work

### ❌ Hardcoded Configuration
**Bad**: Hardcoded bootstrap nodes, timeouts, limits
**Good**: Configuration file, environment variables, CLI flags

---

## Pattern Evolution

As the codebase grows, document new patterns here:

**When to Add a Pattern**:
- Used in 3+ places across codebase
- Solves a recurring architectural problem
- Establishes a convention (naming, structure, error handling)

**Format**:
```markdown
### Pattern Name

**Pattern**: One-sentence summary

**Code Example**: Concrete implementation

**Key Principles**: 3-5 bullet points

**When to Use**: Specific scenarios
```

---

**Remember**: Patterns are discovered, not invented. Extract patterns from working code, don't impose patterns prematurely.
