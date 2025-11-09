# Technical Context: Phase Technology Stack

**Last Updated**: 2025-11-08
**Version**: 0.1
**Status**: MVP Technology Selection

---

## Table of Contents

1. [Technology Stack Overview](#technology-stack-overview)
2. [Plasm Daemon (Rust)](#plasm-daemon-rust)
3. [WASM Runtime](#wasm-runtime)
4. [Networking & Transport](#networking--transport)
5. [PHP Client SDK](#php-client-sdk)
6. [Build & Deployment](#build--deployment)
7. [Development Tools](#development-tools)
8. [Future Technology Roadmap](#future-technology-roadmap)

---

## Technology Stack Overview

| Component | Technology | Version | Rationale |
|-----------|-----------|---------|-----------|
| **Runtime Daemon** | Rust | 1.70+ | Memory safety, performance, async/await, ecosystem |
| **WASM Engine** | wasm3 → wasmtime | wasm3 (MVP), wasmtime (future) | Fast startup (wasm3), production-ready (wasmtime) |
| **P2P Networking** | rust-libp2p | 0.53+ | Industry-standard DHT, modular transports |
| **Encryption** | Noise + QUIC | libp2p built-in | Zero-RTT, forward secrecy, UDP performance |
| **Serialization** | serde + JSON | serde 1.0+ | Human-readable manifests, wide compatibility |
| **Client SDK** | PHP | 8.1+ | Target audience familiarity, broad adoption |
| **Packaging** | cargo-deb | Latest | Native Debian packaging for Ubuntu targets |
| **Service Management** | systemd | - | Standard Linux service lifecycle |

---

## Plasm Daemon (Rust)

### Core Dependencies

```toml
[dependencies]
# Async runtime
tokio = { version = "1.35", features = ["full"] }

# Networking
libp2p = { version = "0.53", features = ["kad", "quic", "noise", "yamux"] }

# WASM runtime (MVP)
wasm3 = "0.3"
# WASM runtime (future)
wasmtime = "16.0"  # Commented out for MVP

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Cryptography
ed25519-dalek = "2.0"  # Receipt signing
sha2 = "0.10"          # Module hashing

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# CLI
clap = { version = "4.4", features = ["derive"] }

# Configuration
config = "0.13"
toml = "0.8"

# Error handling
anyhow = "1.0"
thiserror = "1.0"
```

### Why Rust?

**Advantages**:
- **Memory safety**: No segfaults, buffer overflows, or data races
- **Performance**: Native speed, zero-cost abstractions
- **Async/await**: Built-in async runtime (Tokio) for networking
- **Ecosystem**: Excellent libraries for networking (libp2p), WASM (wasmtime), crypto

**Considerations**:
- Steeper learning curve than Go/Python
- Slower compilation times
- Smaller talent pool

**Decision**: Rust's safety guarantees and performance profile are critical for a distributed compute runtime handling untrusted code.

---

## WASM Runtime

### MVP: wasm3

**Characteristics**:
- Interpreted WASM engine
- Fast startup (< 1ms)
- Low memory overhead
- Simple C API with Rust bindings

**Use Case**: MVP proof-of-concept, lightweight jobs

**Limitations**:
- Slower execution (10-100x vs. JIT)
- Limited WASI support
- Not actively maintained

### Future: wasmtime

**Characteristics**:
- JIT-compiled WASM engine
- Full WASI support
- Production-grade (used by Fastly, Shopify, etc.)
- Active development (Bytecode Alliance)

**Migration Path**:
```rust
// MVP: wasm3
let runtime = Wasm3Runtime::new().build()?;
let result = runtime.execute(wasm_bytes, args)?;

// Future: wasmtime (same interface)
let runtime = WasmtimeRuntime::new().build()?;
let result = runtime.execute(wasm_bytes, args)?;
```

**Decision**: Start with wasm3 for rapid MVP, migrate to wasmtime for production performance.

---

## Networking & Transport

### rust-libp2p

**Features Used**:
- **Kademlia DHT**: Peer discovery and content routing
- **QUIC transport**: UDP-based, low-latency transport
- **Noise protocol**: Encrypted handshake (like TLS)
- **Yamux**: Stream multiplexing over single connection

**Architecture**:
```rust
// libp2p stack
Transport: QUIC (UDP)
    ↓
Security: Noise protocol (encryption)
    ↓
Multiplexing: Yamux (multiple streams per connection)
    ↓
Behaviour: Kademlia (DHT for discovery)
```

**Why libp2p?**
- Industry-standard (IPFS, Filecoin, Polkadot)
- Modular design (swap transports, protocols)
- Built-in encryption, NAT traversal
- Active development and community

**Alternatives Considered**:
- ~~ZeroMQ~~: No built-in DHT or peer discovery
- ~~gRPC~~: Centralized, not peer-to-peer
- ~~Raw TCP/UDP~~: Reinventing the wheel

### NAT Traversal

**Techniques**:
1. **UPnP/NAT-PMP**: Automatic port forwarding (home routers)
2. **Relay nodes**: TURN-like relaying for restricted NATs
3. **Hole punching**: Simultaneous open for symmetric NATs

**Implementation**: libp2p's built-in relay and autonat protocols

---

## PHP Client SDK

### Target Version: PHP 8.1+

**Why PHP?**
- Target audience: Web developers, rapid prototyping
- Widespread adoption (WordPress, Laravel, Symfony)
- Simple syntax, low barrier to entry

**Architecture**:
```
plasm-php/
├── src/
│   ├── Client.php          # Main API
│   ├── Manifest.php        # Job manifest builder
│   ├── Receipt.php         # Receipt verification
│   └── Transport/
│       ├── LocalTransport.php   # For local testing
│       └── RemoteTransport.php  # libp2p over HTTP bridge
├── tests/
└── composer.json
```

**Dependencies**:
```json
{
  "require": {
    "php": "^8.1",
    "guzzlehttp/guzzle": "^7.8",  // HTTP client for bridge
    "ext-sodium": "*"              // Ed25519 signature verification
  },
  "require-dev": {
    "phpunit/phpunit": "^10.0"
  }
}
```

**API Design**:
```php
// Simple, fluent interface
$client = new Plasm\Client([
    'mode' => 'local',  // or 'remote'
    'daemon_url' => 'http://localhost:8080',
]);

$job = $client->createJob('hello.wasm')
    ->withCpu(1)
    ->withMemory(128)  // MB
    ->withTimeout(30)  // seconds
    ->submit();

$result = $job->wait();  // Blocks until complete
echo $result->stdout();
echo $result->receipt()->verify() ? "✓" : "✗";
```

**Future SDKs**: Swift (iOS/macOS), TypeScript (web), Python, C++

---

## Build & Deployment

### Rust Build

**Development**:
```bash
cargo build              # Debug build
cargo test               # Run tests
cargo clippy             # Linting
cargo fmt                # Code formatting
```

**Release**:
```bash
cargo build --release    # Optimized binary
cargo deb                # Create .deb package
```

### Debian Packaging (cargo-deb)

**Configuration** (`Cargo.toml`):
```toml
[package.metadata.deb]
maintainer = "PhaseBased <hello@phasebased.com>"
copyright = "2025, PhaseBased"
license-file = ["LICENSE", "0"]
extended-description = "Plasm daemon for distributed WASM execution"
depends = "$auto, systemd"
section = "net"
priority = "optional"
assets = [
    ["target/release/plasmd", "/usr/bin/", "755"],
    ["config/plasmd.service", "/lib/systemd/system/", "644"],
    ["config/plasmd.toml", "/etc/plasm/", "644"],
]
```

**Output**: `plasm_0.1.0_amd64.deb`

### systemd Service

**Unit File** (`plasmd.service`):
```ini
[Unit]
Description=Plasm Distributed Compute Daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/plasmd --config /etc/plasm/plasmd.toml
Restart=on-failure
RestartSec=5s
User=plasm
Group=plasm
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/plasm

[Install]
WantedBy=multi-user.target
```

**Lifecycle**:
```bash
sudo systemctl start plasmd
sudo systemctl enable plasmd
sudo systemctl status plasmd
journalctl -u plasmd -f
```

---

## Development Tools

### Recommended Setup

**Editor**: VS Code with rust-analyzer
**Linting**: `cargo clippy` (strict mode)
**Formatting**: `rustfmt` (auto-format on save)
**Testing**: `cargo test` + `cargo tarpaulin` (coverage)

### CI/CD (Future)

**GitHub Actions**:
```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rs/toolchain@v1
      - run: cargo test --all-features
      - run: cargo clippy -- -D warnings

  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release
      - run: cargo deb
      - uses: actions/upload-artifact@v4
        with:
          name: plasm.deb
          path: target/debian/*.deb
```

**Coverage**: Codecov or Coveralls

---

## Future Technology Roadmap

### Phase 2: Advanced Networking
- **WebRTC data channels**: Browser-based nodes
- **Gossipsub**: Efficient pub/sub for job announcements
- **Circuit relay v2**: Better NAT traversal

### Phase 3: Security & Verification
- **zk-SNARK proofs**: Cryptographic execution verification
- **SGX/SEV**: Hardware-based trusted execution
- **Reputation system**: DHT-based peer scoring

### Phase 4: Ecosystem Expansion
- **Swift SDK**: Native iOS/macOS client
- **TypeScript SDK**: Web-based job submission
- **Python SDK**: Data science workflows
- **C++ SDK**: High-performance clients

### Phase 5: Performance Optimization
- **WASM SIMD**: Vectorized computation
- **GPU passthrough**: WGPU for compute shaders
- **Multi-threaded WASM**: Shared memory parallelism

---

## Technology Decision Log

### Why wasm3 for MVP instead of wasmtime?

**Date**: 2025-11-08
**Decision**: Use wasm3 for initial MVP
**Rationale**:
- Faster startup (< 1ms vs. 10-100ms JIT compilation)
- Simpler integration (fewer dependencies)
- Sufficient for proof-of-concept workloads
- Easy migration path (runtime abstraction)

**Trade-offs**:
- Slower execution (acceptable for demo)
- Limited WASI support (not needed for MVP)

**Revisit**: After MVP validation, migrate to wasmtime

### Why libp2p instead of custom P2P?

**Date**: 2025-11-08
**Decision**: Use rust-libp2p
**Rationale**:
- Battle-tested (IPFS, Filecoin)
- Built-in DHT, NAT traversal, encryption
- Modular design (easy to swap components)
- Active community

**Trade-offs**:
- Larger dependency footprint
- Steeper learning curve

**Alternatives Rejected**:
- Custom TCP/UDP: Too much work, error-prone
- ZeroMQ: No DHT, requires central broker

---

## Dependency Audit Policy

**Security**:
- Run `cargo audit` before every release
- Pin dependencies in `Cargo.lock`
- Review security advisories monthly

**Licensing**:
- Only use Apache 2.0 / MIT compatible licenses
- Document all dependencies in `NOTICE` file

**Maintenance**:
- Prefer actively maintained crates (commits in last 6 months)
- Avoid abandoned or single-maintainer crates for critical components

---

**Technology choices are driven by MVP requirements and future scalability. Re-evaluate quarterly.**
