# Phase – Open, Verifiable Compute

**Phase** is an open protocol for distributed computation: workloads are discovered, executed, and verified across a global network of independent nodes.  
**Plasm** is the local runtime engine that turns any computer into a node in the network.  
**PhaseBased** (commercial layer) will provide orchestration, SLAs, and billing later—**this repository focuses on the open protocol and free infrastructure.**

---

## Vision

The internet began as a network for sharing information.  
Phase is the next evolution—a network for sharing **computation**.

- **Decentralized:** no data centers, no single owner.  
- **Verifiable:** every result includes a signed receipt of work done.  
- **Private:** jobs execute in sandboxed WebAssembly environments.  
- **Resilient:** discovery and routing are handled peer‑to‑peer using DHT protocols.  

With Phase, you can run a job anywhere—across your LAN, a research cluster, or the public internet—and receive a provable result from an anonymous node.

---

## Architecture

| Layer | Name | Description |
|-------|------|-------------|
| **Protocol** | **Phase** | Defines job manifests, receipts, peer discovery, and encrypted transport. |
| **Runtime** | **Plasm** | Lightweight daemon that runs on any OS and executes WASM workloads. |
| **Client SDKs** | PHP, Swift, C++, React, etc. | Submit jobs, track progress, retrieve results. |
| **(Future)** | **PhaseBased** | Commercial orchestration and guarantees built on top of this open core. |

---

## MVP Scope

Demonstrate end‑to‑end remote execution using anonymous discovery:

1. **Anonymous peer discovery** using a modern DHT (libp2p / Kademlia).  
2. **Job submission** from a PHP client on macOS ARM.  
3. **Remote execution** on a Plasm node (.deb for Ubuntu x86_64).  
4. **Result and receipt return** through encrypted channels.  

**Goal:** a simple PHP script submits a WASM job that runs remotely and returns output + signed proof of execution.

---

## Components

### Plasm Daemon
- Written in **Rust** for safety and portability.
- Embeds a lightweight **WASM runtime** (`wasm3` initially; `wasmtime` next).
- Handles DHT participation, job queueing, encrypted transport, and receipt signing.
- Packaged as `.deb` for Intel/AMD Ubuntu systems.

### PHP Client SDK
- Library to discover peers, submit jobs, track progress, and retrieve results.
- Simple API:
  ```php
  $plasm = new Plasm\Client();
  $jobId = $plasm->submit('hello.wasm', ['cpu'=>1]);
  echo $plasm->result($jobId);
  ```

### Manifests & Receipts
- **Manifest** – declares resources, timeouts, and capabilities.
- **Receipt** – signed proof including module hash, wall time, CPU seconds.

---

## Phases of Development

1. **MVP** – Single‑job remote execution (this repo).  
2. **Networking Expansion** – Multi‑node coordination, redundancy, result recombination.  
3. **Security & Verification** – Deterministic receipts, reputation; explore zk‑proofs.  
4. **Ecosystem SDKs** – Swift, TypeScript, C++, PHP, Python libraries.  
5. **Federation Layer** – Self‑governing clusters, persistent identity, optional payments.  
6. **Commercial Layer** *(PhaseBased)* – SLAs, billing, and guaranteed performance tiers.

---

## Installation

### Prerequisites

**Ubuntu/Debian Server (x86_64):**
- Ubuntu 22.04 LTS or newer
- systemd (for service management)
- 1GB+ RAM, 1GB+ disk space

**macOS/Linux Client (any architecture):**
- PHP 8.1 or newer
- Composer (PHP package manager)
- `php-sodium` extension (for Ed25519 signature verification)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/phasebased/phase.git
cd phase

# Install Rust toolchain (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install cargo-deb for packaging
cargo install cargo-deb

# Build the Debian package
cd daemon
cargo deb

# Package will be created at: target/debian/plasm_0.1.0_amd64.deb
```

**For detailed build instructions, troubleshooting, and verification steps**, see the comprehensive build guide: **[docs/building-on-ubuntu.md](docs/building-on-ubuntu.md)**

### Installing on Ubuntu/Debian

```bash
# Install the package
sudo dpkg -i target/debian/plasm_0.1.0_amd64.deb

# Start the service
sudo systemctl start plasmd

# Enable to start on boot (optional)
sudo systemctl enable plasmd

# Check service status
sudo systemctl status plasmd

# View logs
sudo journalctl -u plasmd -f
```

### Installing PHP Client SDK

```bash
# Navigate to php-sdk directory
cd php-sdk

# Install dependencies
composer install

# The SDK is now ready to use in examples/
```

### Troubleshooting

**Service won't start:**
```bash
# Check detailed logs
sudo journalctl -u plasmd -n 50 --no-pager

# Verify binary is executable
ls -l /usr/bin/plasmd

# Test manual execution
/usr/bin/plasmd --version
```

**PHP client can't find daemon:**
- Ensure plasmd is running: `sudo systemctl status plasmd`
- Check network connectivity
- Verify firewall allows required ports

**Signature verification fails:**
- Ensure `php-sodium` extension is installed: `php -m | grep sodium`
- Install if missing: `sudo apt install php-sodium` (Ubuntu) or `brew install php` (macOS)

## Quick Start

### Local Execution Test

```bash
# From repository root
cd examples
php local_test.php
```

Expected output:
```
Submitting job: hello.wasm
Job ID: <uuid>
Waiting for result...
Output: dlroW ,olleH
Exit code: 0
Receipt verified ✓
```

### Remote Execution Test

```bash
# Ensure plasmd is running on a remote node
# From repository root
cd examples
php remote_test.php
```

Expected output:
```
Phase Remote Execution Test
===========================

Discovering nodes...
✓ Node discovered: <peer-id>
  Architecture: x86_64
  Runtime: wasmtime-27

Submitting job: hello.wasm
✓ Job submitted: <job-id>

Waiting for execution...
✓ Execution complete

Result:
  Output: dlroW ,olleH
  Exit code: 0
  Wall time: 235ms

Receipt Verification:
✓ Signature valid
✓ Module hash matches
✓ Receipt verified

Test complete!
```

---

## Roadmap Preview

- **Distributed Streams:** shard large jobs into parallel WASM streams and recombine results.  
- **Node Discovery Map:** visualize global capacity and latency.  
- **Receipts‑as‑Proof:** deterministic hashes for each compute unit.  
- **Anonymous Volunteer Mesh:** open pool for research and civic compute.  
- **PhaseBased Launch:** commercial orchestration and verified tiers (separate repo).

---

## License
Apache 2.0 © 2025 PhaseBased
