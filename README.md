# Phase – Open, Verifiable Compute

**Phase** is an open protocol for distributed computation: workloads are discovered, executed, and verified across a global network of independent nodes.
**Plasm** is the local runtime engine that turns any computer into a node in the network.
**Phase Boot** enables any machine to boot from the network and join the compute mesh.

---

## What's Working Today

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         PHASE ECOSYSTEM                                  │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   ┌─────────────┐      DHT/mDNS       ┌─────────────┐                   │
│   │ Phase Boot  │ ◄──────────────────► │  Provider   │                   │
│   │  (Client)   │     Discovery        │  (plasmd)   │                   │
│   └──────┬──────┘                      └──────┬──────┘                   │
│          │                                    │                          │
│          │  Fetch kernel, initramfs           │  Serve boot artifacts    │
│          │  Verify signatures                 │  Sign manifests          │
│          │  kexec into target                 │  Advertise to DHT        │
│          ▼                                    ▼                          │
│   ┌─────────────┐                      ┌─────────────┐                   │
│   │   Running   │ ◄──────────────────► │    WASM     │                   │
│   │   System    │     Job Execution    │   Runtime   │                   │
│   └─────────────┘                      └─────────────┘                   │
│                                                                          │
│   Self-Hosting Loop: Boot → Serve → Others boot from you                │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### Releases Complete

| Release | Status | Description |
|---------|--------|-------------|
| **Phase Open MVP** | ✅ Complete | WASM execution, peer discovery, signed receipts |
| **Phase Boot** | ✅ Complete | Bootable USB/VM, DHT discovery, kexec handoff |
| **Netboot Provider** | ✅ Complete | HTTP artifact server, manifest signing, DHT advertisement |

---

## Quick Start

### Run a Boot Artifact Provider

```bash
# Build plasmd
cd daemon
cargo build --release

# Create artifacts directory
mkdir -p ~/boot-artifacts/stable/arm64
cp /path/to/kernel ~/boot-artifacts/stable/arm64/
cp /path/to/initramfs ~/boot-artifacts/stable/arm64/

# Start provider
./target/release/plasmd serve --artifacts ~/boot-artifacts

# Output:
# ╔══════════════════════════════════════════════╗
# ║           Phase Boot Provider                ║
# ╠══════════════════════════════════════════════╣
# ║ HTTP:     http://0.0.0.0:8080                ║
# ║ Channel:  stable                             ║
# ║ Arch:     arm64                              ║
# ║ DHT:      enabled                            ║
# ║ mDNS:     enabled                            ║
# ╚══════════════════════════════════════════════╝
```

### Check Provider Status

```bash
# View provider status
./target/release/plasmd provider status

# List available artifacts
./target/release/plasmd provider list

# Fetch manifest
curl http://localhost:8080/manifest.json | jq
```

### Execute a WASM Job

```bash
# Run a WASM workload locally
./target/release/plasmd run examples/hello.wasm

# Execute with signed receipt
./target/release/plasmd execute-job --wasm examples/hello.wasm
```

---

## Architecture

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
│       WASM Runtime (wasmtime)           │
└─────────────────────────────────────────┘
```

| Layer | Component | Description |
|-------|-----------|-------------|
| **Protocol** | Phase | Job manifests, receipts, peer discovery, encrypted transport |
| **Runtime** | Plasm | Rust daemon executing WASM workloads with resource limits |
| **Discovery** | libp2p | Kademlia DHT for internet-wide discovery, mDNS for LAN |
| **Security** | Ed25519 | Signed manifests and receipts, SHA256 artifact verification |
| **Boot** | Phase Boot | Network boot with kexec, overlayfs, mode-based behavior |

---

## Components

### Plasm Daemon (`daemon/`)

The core runtime engine:

- **WASM Execution**: Wasmtime runtime with memory/CPU limits
- **Peer Discovery**: Kademlia DHT + mDNS for node discovery
- **Job Protocol**: Offer/Accept handshake, signed receipts
- **Boot Provider**: HTTP server for boot artifacts

```bash
# CLI Commands
plasmd start              # Start P2P daemon
plasmd run <wasm>         # Execute WASM locally
plasmd serve              # Start boot artifact provider
plasmd provider status    # Query provider status
plasmd provider list      # List artifacts
```

### Phase Boot (`boot/`)

Network boot system:

- **Boot Stub**: Minimal initramfs with discovery tools
- **Discovery**: `phase-discover` binary for DHT/mDNS
- **Verification**: `phase-verify` for Ed25519 signatures
- **Fetch**: `phase-fetch` for content-addressed downloads
- **kexec**: Fast kernel switch without firmware

```bash
# Boot modes (via kernel cmdline)
phase.mode=internet    # Full network, DHT discovery
phase.mode=local       # LAN only, mDNS discovery
phase.mode=private     # Ephemeral identity, no writes
```

### Provider Module (`daemon/src/provider/`)

HTTP-based boot artifact serving:

| Endpoint | Description |
|----------|-------------|
| `GET /` | Provider info |
| `GET /health` | Health check (200/503) |
| `GET /status` | Detailed status with metrics |
| `GET /manifest.json` | Signed boot manifest |
| `GET /:channel/:arch/:artifact` | Download artifact (Range supported) |

### PHP Client SDK (`php-sdk/`)

```php
$plasm = new Plasm\Client();
$jobId = $plasm->submit('hello.wasm', ['cpu' => 1]);
$result = $plasm->result($jobId);
echo $result->output;  // "dlroW ,olleH"
```

---

## Installation

### From Source (Recommended)

```bash
# Clone repository
git clone https://github.com/phasebased/phase.git
cd phase

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build daemon
cd daemon
cargo build --release

# Binary at: target/release/plasmd
```

### Debian Package

```bash
# Install cargo-deb
cargo install cargo-deb

# Build package
cd daemon
cargo deb

# Install
sudo dpkg -i target/debian/plasm_0.1.0_amd64.deb

# Start service
sudo systemctl start plasmd
sudo systemctl enable plasmd
```

### PHP SDK

```bash
cd php-sdk
composer install
```

---

## Configuration

### Provider Configuration

```bash
# Start with options
plasmd serve \
  --artifacts /var/lib/plasm/boot-artifacts \
  --channel stable \
  --arch arm64 \
  --port 8080 \
  --no-dht      # Disable DHT (LAN only)
```

### Artifact Directory Structure

```
/var/lib/plasm/boot-artifacts/
├── stable/
│   ├── arm64/
│   │   ├── kernel
│   │   ├── initramfs
│   │   └── rootfs (optional)
│   └── x86_64/
│       └── ...
└── testing/
    └── ...
```

---

## Security Model

- **Signed Manifests**: Ed25519 signatures over boot manifests
- **Content Verification**: SHA256 hashes for all artifacts
- **Sandboxed Execution**: WASM with no host access by default
- **Encrypted Transport**: Noise protocol over QUIC
- **Signed Receipts**: Cryptographic proof of execution

---

## Documentation

| Document | Location |
|----------|----------|
| Provider Quickstart | `daemon/docs/provider-quickstart.md` |
| Architecture | `daemon/docs/architecture.md` |
| API Reference | `daemon/docs/api-reference.md` |
| Troubleshooting | `daemon/docs/troubleshooting.md` |
| Security | `daemon/docs/security.md` |
| Boot Architecture | `boot/docs/ARCHITECTURE.md` |
| Boot Quickstart | `boot/docs/QUICKSTART-*.md` |

---

## Roadmap

### Completed

- [x] Local WASM execution with wasmtime
- [x] Peer discovery via Kademlia DHT
- [x] Remote execution with signed receipts
- [x] Debian packaging
- [x] Phase Boot (USB/VM boot system)
- [x] Netboot Provider (HTTP artifact server)
- [x] Self-hosting loop

### In Progress

- [ ] Full mDNS service advertisement
- [ ] Multi-provider load balancing
- [ ] Production key management

### Future

- [ ] Secure Boot signing chain
- [ ] Distributed job streams
- [ ] Zero-knowledge execution proofs
- [ ] Swift/TypeScript/Python SDKs
- [ ] PhaseBased commercial layer

---

## Project Structure

```
phase/
├── daemon/                 # Plasm daemon (Rust)
│   ├── src/
│   │   ├── main.rs         # CLI entry point
│   │   ├── lib.rs          # Library exports
│   │   ├── provider/       # Boot provider module
│   │   ├── network/        # P2P networking
│   │   └── wasm/           # WASM runtime
│   ├── docs/               # Provider documentation
│   └── Cargo.toml
├── boot/                   # Phase Boot system
│   ├── Makefile            # Build orchestration
│   ├── initramfs/          # Init scripts
│   └── docs/               # Boot documentation
├── php-sdk/                # PHP client SDK
├── examples/               # Example WASM jobs
├── memory-bank/            # Project documentation
│   ├── releases/           # Release plans
│   │   ├── boot/           # Phase Boot release
│   │   └── netboot/        # Netboot Provider release
│   └── tasks/              # Task documentation
└── README.md
```

---

## Contributing

1. Read `memory-bank/systemPatterns.md` for architectural patterns
2. Follow the library + binary pattern for Rust code
3. Run `cargo test` before submitting changes
4. Update documentation for new features

---

## License

Apache 2.0 - 2025 PhaseBased

---

## Stats

- **Tests**: 80 passing
- **Daemon Code**: ~5,000 lines Rust
- **Boot Code**: ~15,000 lines (scripts, configs, docs)
- **Provider Module**: 2,510 lines Rust
- **Documentation**: 3,000+ lines
