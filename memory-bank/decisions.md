# Architectural Decisions: Phase Open MVP

**Last Updated**: 2025-11-08
**Version**: 0.1

---

## Decision Log Format

Each decision follows this template:

```markdown
### YYYY-MM-DD: Decision Title
**Status**: Proposed | Accepted | Deprecated | Superseded
**Context**: Why was this decision needed?
**Decision**: What was decided?
**Alternatives Considered**: What other options were evaluated?
**Consequences**: What are the positive and negative outcomes?
**References**: Links to related tasks, discussions, docs
```

---

## Decisions

### 2025-11-08: Use wasm3 for MVP, wasmtime for Production

**Status**: Accepted

**Context**:
Need a WASM runtime for executing user-submitted jobs. Requirements:
- Fast startup time (minimize overhead)
- Sandboxed execution (no host access)
- Rust bindings available
- Production-ready or clear migration path

**Decision**:
Use **wasm3** for MVP development, plan migration to **wasmtime** for production.

**Alternatives Considered**:
1. **wasmtime** (Bytecode Alliance)
   - ✅ Production-grade, JIT-compiled, fast execution
   - ✅ Full WASI support
   - ❌ Slower startup (10-100ms JIT compilation)
   - ❌ Higher memory overhead

2. **wasmer** (Wasmer Inc.)
   - ✅ JIT-compiled, fast execution
   - ✅ Good Rust bindings
   - ❌ Commercial backing (potential licensing concerns)
   - ❌ Similar startup overhead to wasmtime

3. **wasm3** (Interpreted)
   - ✅ Ultra-fast startup (<1ms)
   - ✅ Low memory overhead
   - ✅ Simple C API with Rust bindings
   - ❌ Slower execution (10-100x vs JIT)
   - ❌ Limited WASI support
   - ❌ Not actively maintained

**Consequences**:

*Positive*:
- Fast iteration during MVP (instant startup)
- Simple integration (minimal dependencies)
- Clear migration path (runtime abstraction)

*Negative*:
- Execution performance not representative of production
- Must plan wasmtime migration early
- May encounter limitations with complex WASM modules

**Migration Strategy**:
```rust
// Define trait for runtime abstraction
trait WasmRuntime {
    fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult>;
}

// MVP: wasm3 implementation
struct Wasm3Runtime { }
impl WasmRuntime for Wasm3Runtime { }

// Future: wasmtime implementation (same interface)
struct WasmtimeRuntime { }
impl WasmRuntime for WasmtimeRuntime { }
```

**References**: `memory-bank/techContext.md#WASM Runtime`

---

### 2025-11-08: Use libp2p for Peer-to-Peer Networking

**Status**: Accepted

**Context**:
Need decentralized peer discovery and encrypted communication. Requirements:
- Anonymous peer discovery (no central registry)
- NAT traversal (home routers, firewalls)
- Encrypted transport (privacy, security)
- Rust ecosystem compatibility

**Decision**:
Use **rust-libp2p** with Kademlia DHT for peer discovery and QUIC + Noise for transport.

**Alternatives Considered**:
1. **Custom TCP/UDP + DHT**
   - ✅ Full control, minimal dependencies
   - ❌ Enormous engineering effort
   - ❌ High risk of bugs (security, NAT traversal)
   - ❌ No community support

2. **ZeroMQ**
   - ✅ Simple API, good performance
   - ❌ No built-in DHT or peer discovery
   - ❌ Requires central broker (defeats decentralization)

3. **gRPC**
   - ✅ Well-supported, good tooling
   - ❌ Centralized (requires known server addresses)
   - ❌ Not designed for P2P

4. **libp2p** (IPFS, Filecoin)
   - ✅ Battle-tested in production (IPFS, Filecoin, Polkadot)
   - ✅ Built-in DHT (Kademlia), NAT traversal, encryption
   - ✅ Modular design (swap transports, protocols)
   - ✅ Active development (Protocol Labs)
   - ❌ Larger dependency footprint
   - ❌ Steeper learning curve

**Consequences**:

*Positive*:
- Proven technology (IPFS runs on millions of nodes)
- Handles NAT traversal automatically (relay, autonat)
- Encrypted by default (Noise protocol)
- Modular (can swap Kademlia for other discovery later)

*Negative*:
- Heavier dependency tree (increases binary size)
- Complexity (may be overkill for simple use cases)
- Learning curve (libp2p abstractions)

**References**: `memory-bank/systemPatterns.md#Peer Discovery Pattern`

---

### 2025-11-08: JSON for Manifests and Receipts

**Status**: Accepted

**Context**:
Need serialization format for job manifests and execution receipts. Requirements:
- Human-readable (easy debugging)
- Schema validation (prevent malformed input)
- Cross-language support (Rust, PHP, future SDKs)
- Compact enough for network transmission

**Decision**:
Use **JSON** with JSON Schema validation for both manifests and receipts.

**Alternatives Considered**:
1. **Protocol Buffers (protobuf)**
   - ✅ Compact binary format
   - ✅ Schema enforcement
   - ✅ Cross-language support
   - ❌ Not human-readable (harder debugging)
   - ❌ Requires code generation

2. **MessagePack**
   - ✅ Compact binary format
   - ✅ JSON-compatible
   - ❌ Less tooling support
   - ❌ Not human-readable

3. **CBOR**
   - ✅ Compact, standardized (RFC 8949)
   - ✅ JSON-compatible
   - ❌ Less common (smaller ecosystem)
   - ❌ Not human-readable

4. **JSON**
   - ✅ Human-readable (easy debugging)
   - ✅ Universal support (all languages, tools)
   - ✅ JSON Schema for validation
   - ❌ Larger size (vs binary formats)
   - ❌ No native date/binary types

**Consequences**:

*Positive*:
- Easy to debug (cat manifest.json, curl receipts)
- Wide tooling (jq, JSON Schema validators)
- Cross-language compatibility (PHP, Swift, TypeScript)
- No code generation required

*Negative*:
- Slightly larger payloads (vs protobuf/msgpack)
- No built-in binary support (must base64 encode)

**Future Migration Path**:
If network efficiency becomes critical, add binary encoding as optional transport format while keeping JSON as canonical representation.

**References**: `memory-bank/systemPatterns.md#Data Flow Patterns`

---

### 2025-11-08: Ed25519 for Receipt Signatures

**Status**: Accepted

**Context**:
Need cryptographic signatures for execution receipts. Requirements:
- Fast signing and verification
- Small signature size (network efficiency)
- Widely supported libraries
- No patent/licensing issues

**Decision**:
Use **Ed25519** (edwards25519 curve) for all receipt signatures.

**Alternatives Considered**:
1. **RSA-2048**
   - ✅ Widely supported, mature
   - ❌ Large signatures (256 bytes)
   - ❌ Slow signing/verification
   - ❌ Vulnerable to timing attacks

2. **ECDSA (secp256k1 / secp256r1)**
   - ✅ Smaller signatures than RSA (64 bytes)
   - ✅ Widely used (Bitcoin, Ethereum)
   - ❌ Slower than Ed25519
   - ❌ More complex implementation
   - ❌ Vulnerable to nonce reuse

3. **Ed25519**
   - ✅ Fast signing/verification
   - ✅ Small signatures (64 bytes)
   - ✅ No nonce reuse vulnerability
   - ✅ Excellent Rust support (ed25519-dalek)
   - ✅ Single-party signing (no aggregation needed for MVP)

**Consequences**:

*Positive*:
- Fast verification on client side (PHP, Swift)
- Small signature overhead (64 bytes)
- Deterministic signing (no RNG required)
- Well-supported (libsodium, ed25519-dalek)

*Negative*:
- Not compatible with existing RSA/ECDSA infrastructure
- Single-party only (no threshold signatures without extensions)

**Implementation**:
```rust
use ed25519_dalek::{Keypair, Signature, Signer, Verifier};

// Node signs receipt
let signature: Signature = node_keypair.sign(&receipt_bytes);

// Client verifies receipt
node_public_key.verify(&receipt_bytes, &signature)?;
```

**References**: `memory-bank/projectRules.md#Cryptography`

---

### 2025-11-08: PHP for Initial Client SDK

**Status**: Accepted

**Context**:
Need client SDK for job submission. Requirements:
- Target audience familiarity (web developers)
- Rapid prototyping capability
- Broad adoption (server-side scripting)
- Cross-platform compatibility

**Decision**:
Build **PHP SDK** first (PHP 8.1+), followed by Swift, TypeScript, Python in later phases.

**Alternatives Considered**:
1. **Python**
   - ✅ Popular for data science, scripting
   - ✅ Excellent async support (asyncio)
   - ❌ Less common for web backends (vs PHP)

2. **TypeScript/JavaScript**
   - ✅ Universal (browser + Node.js)
   - ✅ Great async support (promises, async/await)
   - ❌ Fragmented ecosystem (npm, browser, Deno)

3. **Swift**
   - ✅ Target platform (macOS, iOS)
   - ✅ Native performance
   - ❌ Smaller audience than PHP
   - ❌ Server-side Swift less mature

4. **PHP**
   - ✅ Massive adoption (WordPress, Laravel, Symfony)
   - ✅ Simple syntax (low barrier to entry)
   - ✅ Server-side scripting (common use case)
   - ✅ Good crypto support (libsodium extension)
   - ❌ Less async support (until fibers/Swoole)

**Consequences**:

*Positive*:
- Wide potential user base (WordPress, Laravel devs)
- Simple API design (synchronous by default)
- Easy cross-platform testing (macOS, Linux, Windows)

*Negative*:
- Async support requires Swoole or ReactPHP (added complexity)
- Less "cool factor" than Rust/Go/TypeScript

**Roadmap**:
1. **Phase 1 (MVP)**: PHP SDK (local + remote execution)
2. **Phase 2**: Swift SDK (iOS/macOS native)
3. **Phase 3**: TypeScript SDK (web + Node.js)
4. **Phase 4**: Python SDK (data science workflows)

**References**: `memory-bank/techContext.md#PHP Client SDK`

---

### 2025-11-08: cargo-deb for Debian Packaging

**Status**: Accepted

**Context**:
Need distribution format for plasm daemon on Ubuntu targets. Requirements:
- Native package manager integration (apt)
- systemd service lifecycle
- Clean installation and removal
- Easy cross-compilation

**Decision**:
Use **cargo-deb** to generate `.deb` packages for Ubuntu x86_64.

**Alternatives Considered**:
1. **Static binary tarball**
   - ✅ Simple, universal
   - ❌ No package manager integration
   - ❌ Manual systemd setup
   - ❌ No automatic dependency resolution

2. **Docker container**
   - ✅ Portable, self-contained
   - ❌ Heavier runtime (Docker daemon required)
   - ❌ Not native systemd integration
   - ❌ Complicates peer discovery (networking)

3. **Snap package**
   - ✅ Modern Ubuntu packaging
   - ✅ Auto-updates
   - ❌ Sandboxing complications (network, filesystem)
   - ❌ Slower startup

4. **cargo-deb**
   - ✅ Native `.deb` format (apt integration)
   - ✅ systemd service included
   - ✅ Dependency management
   - ✅ Simple configuration (Cargo.toml)
   - ❌ Debian/Ubuntu only (no RPM)

**Consequences**:

*Positive*:
- Clean installation: `sudo dpkg -i plasm.deb`
- Automatic systemd integration
- Dependency resolution (libc, systemd)
- Familiar to Ubuntu/Debian users

*Negative*:
- Limited to Debian-based distros (no RHEL/Fedora)
- Requires separate packaging for other distros

**Future Expansion**:
- **RPM** (RHEL/Fedora): cargo-generate-rpm
- **Homebrew** (macOS): formula for `brew install plasm`
- **AUR** (Arch Linux): PKGBUILD script

**References**: `memory-bank/techContext.md#Build & Deployment`

---

### 2025-11-08: No Commercial Code in Open Repository

**Status**: Accepted

**Context**:
Phase has two layers:
1. **Phase Open**: Free, open-source protocol and runtime
2. **PhaseBased**: Commercial orchestration, SLAs, billing (future)

**Decision**:
This repository (`phase`) contains **only** the open protocol and runtime. Commercial features (SLAs, billing, orchestration) will live in separate repositories.

**Consequences**:

*Positive*:
- Clear separation of concerns
- Open-source community can contribute without commercial concerns
- License clarity (Apache 2.0 for open layer)
- No "bait and switch" (open core stays open)

*Negative*:
- Duplication possible (shared code must be extracted to libraries)
- Integration testing across repos more complex

**Governance**:
- Open layer: Community-driven, Apache 2.0
- Commercial layer: PhaseBased-owned, proprietary (separate repo, license)

**References**: `memory-bank/projectbrief.md#Constraints`

---

## Superseded Decisions

*(None yet)*

---

## Future Decisions to Make

### Pending
- **Bootstrap node strategy**: Public bootstrap nodes vs. configurable list
- **Configuration format**: TOML vs YAML vs JSON
- **Log format**: JSON logs vs structured text
- **Peer reputation system**: Design and storage mechanism

### Deferred
- **zk-SNARK proofs**: Wait for production workloads (Phase 3)
- **GPU passthrough**: Wait for WGPU maturity
- **Multi-threaded WASM**: Wait for WASM threads standardization

---

**Decision log format adapted from Architecture Decision Records (ADR) pattern.**
