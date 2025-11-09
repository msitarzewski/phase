# Project Brief: Phase Open MVP

**Last Updated**: 2025-11-08
**Version**: 0.1
**Status**: Active Development - MVP Phase

---

## Vision

The internet began as a network for sharing information. Phase is the next evolution—a network for sharing **computation**.

Build an open protocol for distributed computation where workloads are discovered, executed, and verified across a global network of independent nodes.

### Core Principles

- **Decentralized**: No data centers, no single owner
- **Verifiable**: Every result includes a signed receipt of work done
- **Private**: Jobs execute in sandboxed WebAssembly environments
- **Resilient**: Discovery and routing handled peer-to-peer using DHT protocols

---

## Architecture Overview

| Layer | Name | Description |
|-------|------|-------------|
| **Protocol** | **Phase** | Defines job manifests, receipts, peer discovery, and encrypted transport |
| **Runtime** | **Plasm** | Lightweight daemon that runs on any OS and executes WASM workloads |
| **Client SDKs** | PHP, Swift, C++, React, etc. | Submit jobs, track progress, retrieve results |
| **(Future)** | **PhaseBased** | Commercial orchestration and guarantees built on top of this open core |

---

## MVP Scope (v0.1)

**Goal**: Demonstrate end-to-end remote execution using anonymous discovery

### Deliverables

1. **Anonymous peer discovery** using modern DHT (libp2p / Kademlia)
2. **Job submission** from PHP client on macOS ARM
3. **Remote execution** on Plasm node (.deb for Ubuntu x86_64)
4. **Result and receipt return** through encrypted channels

### Success Criteria

A simple PHP script submits a WASM job that runs remotely and returns output + signed proof of execution.

**Example Flow**:
```php
$plasm = new Plasm\Client();
$jobId = $plasm->submit('hello.wasm', ['cpu'=>1]);
echo $plasm->result($jobId);
// Output: "dlroW ,olleH" (reversed string)
// Receipt verified ✓
```

---

## Core Components

### 1. Plasm Daemon
- Written in **Rust** for safety and portability
- Embeds lightweight **WASM runtime** (wasm3 initially; wasmtime next)
- Handles DHT participation, job queueing, encrypted transport, receipt signing
- Packaged as `.deb` for Intel/AMD Ubuntu systems

### 2. PHP Client SDK
- Library to discover peers, submit jobs, track progress, retrieve results
- Simple, minimal API surface
- Local mode for testing, remote mode for production

### 3. Manifests & Receipts
- **Manifest**: Declares resources, timeouts, capabilities
- **Receipt**: Signed proof including module hash, wall time, CPU seconds

---

## Release Plan Milestones

### Milestone 1: Local WASM Execution
**Goal**: Run WASM workloads locally via plasm daemon

**Tasks**:
- Initialize repo structure (daemon/, php-sdk/, examples/)
- Implement wasm3 runner: load .wasm, run, capture stdout
- Define manifest.json & receipt.json schemas
- Provide example job hello.wasm (reverse string)
- Create PHP client library and local demo script

### Milestone 2: Peer Discovery
**Goal**: Enable anonymous node discovery and messaging over DHT

**Tasks**:
- Integrate rust-libp2p with Kademlia DHT
- Advertise node capability manifest (CPU, arch, port)
- Implement job announcement and acceptance handshake
- Encrypt communication using Noise + QUIC
- Implement NAT traversal (UPnP + relay)
- Add structured logging of peer discovery events

### Milestone 3: Remote Execution
**Goal**: Execute job on discovered node and return result

**Tasks**:
- Serialize job payload + manifest
- Transmit via libp2p stream
- Execute job on remote node in WASM sandbox
- Return stdout and signed receipt
- PHP client verifies signature
- Client retry/timeout logic

### Milestone 4: Packaging & Demo
**Goal**: Deliver runnable .deb package and example

**Tasks**:
- Create Debian package using cargo-deb
- Add systemd service for plasmd
- Write install instructions
- Cross-arch demo: macOS ARM client → Ubuntu x86_64 node
- examples/remote_test.php with clear output
- docs/architecture-diagram.png (optional)

---

## Constraints & Non-Negotiables

### Technical Constraints
- **WASM-only execution**: All jobs must be compiled to WebAssembly
- **Default-deny policy**: Sandboxed execution with no host access by default
- **Encrypted transport**: All peer communication encrypted (Noise + QUIC)
- **Anonymous discovery**: No centralized registry or identity required

### Architectural Constraints
- **Open-core only**: No commercial billing code in this repository
- **No timelines**: Quality over deadlines
- **Public repo**: All code Apache 2.0 licensed
- **Minimal dependencies**: Lean, auditable codebase

### Security Requirements
- Sandboxed WASM execution (no file system, network, or syscall access)
- Cryptographic signatures on all receipts
- Encrypted peer-to-peer communication
- No PII collection or storage

---

## Future Phases (Post-MVP)

1. **Networking Expansion**: Multi-node coordination, redundancy, result recombination
2. **Security & Verification**: Deterministic receipts, reputation; explore zk-proofs
3. **Ecosystem SDKs**: Swift, TypeScript, C++, PHP, Python libraries
4. **Federation Layer**: Self-governing clusters, persistent identity, optional payments
5. **Commercial Layer (PhaseBased)**: SLAs, billing, guaranteed performance tiers

---

## Target Audience

### Primary Users (MVP)
- Developers experimenting with distributed compute
- Researchers needing reproducible computation
- Hobbyists contributing spare compute capacity

### Future Users
- Enterprise teams needing verified off-site compute
- Research institutions requiring distributed simulation
- Privacy-conscious applications requiring anonymous execution

---

## Success Metrics (MVP)

- [ ] Plasm daemon runs on Ubuntu x86_64
- [ ] PHP client discovers remote peer via DHT
- [ ] WASM job executes remotely and returns result
- [ ] Receipt signature validates successfully
- [ ] Cross-architecture demo works (macOS ARM → Ubuntu x86_64)
- [ ] Documentation enables third-party reproduction

---

## License & Governance

**License**: Apache 2.0
**Copyright**: © 2025 PhaseBased
**Governance**: Open development, community-driven roadmap

This is the open protocol layer. Commercial services (PhaseBased) will be separate repositories with separate licensing.
