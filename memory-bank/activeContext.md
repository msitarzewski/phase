# Active Context: Current Sprint

**Last Updated**: 2025-11-08
**Sprint**: MVP Foundation (Nov 2025)
**Status**: Planning & Architecture

---

## Current Focus

Building the foundational components for Phase Open MVP (v0.1) - anonymous peer-to-peer distributed WASM execution.

### Active Milestone: Local WASM Execution

**Goal**: Run WASM workloads locally via plasm daemon

**Status**: Planning complete, ready for implementation

**Remaining Tasks**:
1. Initialize repo structure (daemon/, php-sdk/, examples/) - **PLANNED**
2. Implement wasm3 runner: load .wasm, run, capture stdout - **PLANNED**
3. Define manifest.json & receipt.json schemas - **PLANNED**
4. Provide example job hello.wasm (reverse string) - **PLANNED**
5. Create PHP client library and local demo script - **PLANNED**

---

## Current Sprint Backlog

### High Priority (This Week)
- [ ] Set up Rust workspace structure (daemon/, libs/)
- [ ] Integrate wasm3 with basic load/execute/capture
- [ ] Define and implement manifest.json schema
- [ ] Define and implement receipt.json schema
- [ ] Create hello.wasm example (reverse string in Rust → WASM)

### Medium Priority (Next Week)
- [ ] Build PHP client SDK skeleton (Composer package)
- [ ] Implement local transport (in-process execution)
- [ ] Create examples/local_test.php demo
- [ ] Write unit tests for WASM execution
- [ ] Set up CI/CD pipeline (GitHub Actions)

### Low Priority (Future)
- [ ] Documentation: architecture diagram
- [ ] Documentation: developer setup guide
- [ ] Benchmarking: execution overhead measurement

---

## Upcoming Milestones (Roadmap)

### Milestone 2: Peer Discovery (Dec 2025 target)
**Goal**: Enable anonymous node discovery and messaging over DHT

**Tasks**:
- Integrate rust-libp2p with Kademlia DHT
- Advertise node capability manifest
- Implement job announcement and acceptance handshake
- Encrypt communication using Noise + QUIC
- Implement NAT traversal (UPnP + relay)
- Add structured logging of peer discovery events

### Milestone 3: Remote Execution (Jan 2026 target)
**Goal**: Execute job on discovered node and return result

**Tasks**:
- Serialize job payload + manifest
- Transmit via libp2p stream
- Execute job on remote node in WASM sandbox
- Return stdout and signed receipt
- PHP client verifies signature
- Client retry/timeout logic

### Milestone 4: Packaging & Demo (Feb 2026 target)
**Goal**: Deliver runnable .deb package and example

**Tasks**:
- Create Debian package using cargo-deb
- Add systemd service for plasmd
- Write install instructions
- Cross-arch demo: macOS ARM client → Ubuntu x86_64 node
- examples/remote_test.php with clear output
- docs/architecture-diagram.png (optional)

---

## Key Decisions This Week

### Decided
- **WASM Runtime**: Start with wasm3, migrate to wasmtime post-MVP
- **Networking**: rust-libp2p with Kademlia DHT
- **Serialization**: JSON for manifests/receipts (human-readable)
- **Client SDK**: PHP first, Swift/TypeScript later
- **Packaging**: cargo-deb for Debian/Ubuntu targets

### Pending
- Bootstrap node strategy (public nodes vs. configurable)
- Receipt signature algorithm (Ed25519 vs. secp256k1)
- Log format and verbosity levels
- Configuration file format (TOML vs. YAML)

---

## Blockers & Risks

### Current Blockers
None

### Risks
- **wasm3 maintenance**: Project not actively maintained
  - **Mitigation**: Plan wasmtime migration early
- **NAT traversal complexity**: Home routers may block P2P
  - **Mitigation**: Implement relay nodes in Milestone 2
- **Cross-architecture testing**: Limited access to ARM/x86_64 machines
  - **Mitigation**: Use GitHub Actions runners for CI

---

## Active Experiments

### wasm3 Integration Proof-of-Concept
**Status**: Researching Rust bindings
**Goal**: Validate wasm3 can execute WASM and capture stdout
**Timeline**: This week
**Success Criteria**: Hello world WASM executes, stdout captured

### libp2p DHT Bootstrap
**Status**: Not started
**Goal**: Validate Kademlia discovery works without central server
**Timeline**: Next sprint (Milestone 2)
**Success Criteria**: Two local nodes discover each other via DHT

---

## Recent Achievements

### 2025-11-08: Memory Bank Initialization
- Created projectbrief.md
- Created systemPatterns.md
- Created techContext.md
- Created 23 task planning documents for all release plan tasks
- Established AGENTS.md workflow

---

## Next Actions (Priority Order)

1. **Create Rust workspace**: Set up daemon/, libs/, examples/ structure
2. **Integrate wasm3**: Add dependency, create WasmRuntime trait
3. **Define schemas**: Manifest and Receipt JSON schemas with validation
4. **Build hello.wasm**: Rust program that reverses stdin → stdout
5. **Test local execution**: Unit tests for WASM load/execute/capture

---

## Team Context (Solo Developer MVP)

**Role**: Full-stack developer (Rust + PHP)
**Availability**: Part-time (evenings/weekends)
**Timeline**: No hard deadlines, quality over speed

**Knowledge Gaps**:
- libp2p internals (learning as we go)
- WASM module introspection
- systemd service hardening

**Learning Resources**:
- libp2p documentation: https://docs.libp2p.io
- WASM spec: https://webassembly.github.io/spec/
- systemd hardening: https://www.freedesktop.org/software/systemd/man/

---

## Communication & Updates

**Weekly Updates**: Friday end-of-week summary
**Decision Log**: Update decisions.md for architectural choices
**Task Documentation**: Create tasks/YYYY-MM/DDMMDD_*.md after completion

---

## Success Metrics (Milestone 1)

- [ ] `plasmd` binary compiles without errors
- [ ] `hello.wasm` executes and outputs "dlroW ,olleH"
- [ ] PHP client can submit job locally and retrieve result
- [ ] Receipt includes module hash, wall time, exit code
- [ ] Unit tests pass (>80% coverage)
- [ ] Documentation sufficient for third-party reproduction

---

**This document is updated weekly. Last review: 2025-11-08**
