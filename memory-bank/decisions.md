# Architectural Decisions: Phase Open MVP + Phase Core + LUCID

**Last Updated**: 2026-05-29
**Version**: 0.5 (security hardening + multi-workload / sharded-verification architecture notes)

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

### 2026-05-20: Library Extraction Architecture and Dual Licensing

**Status**: Accepted

**Context**:
The November 2025 MVP shipped as a single `daemon/` crate. The LUCID
inference flagship needs the same libp2p substrate, the same Ed25519 identity,
the same signed-manifest/receipt envelopes — but with a completely different
workload (GPU inference instead of WASM). Continuing as one crate would have
forced LUCID into either monkey-patching daemon internals or duplicating ~5k
lines of working substrate. The Phase mission also requires that anyone — including
parties hostile to one another — can build on the protocol without legal
friction, while preventing the flagship application from being forked closed.

**Decision**:
Refactor `daemon/` in place into seven publishable Rust library crates under
a single Cargo workspace, with split licensing:

- **Apache-2.0** for the substrate (`phase-identity`, `phase-net`,
  `phase-manifest`, `phase-receipt`, `phase-protocol`,
  `phase-artifact-server`) and for the reference WASM node `plasm`.
  Permissive license so any organization — corporate, governmental, hostile-to-each-other —
  can build on the substrate.
- **AGPL-3.0-or-later** for the flagship inference daemon `lucidd`. Strong
  copyleft so the public-good inference network cannot be forked into a
  proprietary closed service.

Crate boundaries follow the dep graph:

```
phase-identity   ← leaf, no deps
phase-net        ← phase-identity
phase-manifest   ← phase-identity
phase-receipt    ← phase-identity
phase-protocol   ← phase-manifest, phase-receipt
phase-artifact-server ← phase-manifest, phase-net
plasm            ← phase-net, phase-identity, phase-manifest, phase-receipt, phase-protocol
lucidd           ← phase-protocol, phase-identity, phase-manifest, phase-receipt
```

No upward references. Build order is top to bottom.

**Alternatives Considered**:
1. **Single crate, internal modules** — keeps the monorepo shape but blocks
   LUCID from depending on stable interfaces without taking a dep on every
   plasm-specific type. Rejected.
2. **Two crates: `phase-core` mega-crate + `plasm` binary** — simpler dep
   tree but conflates concerns. A consumer who only wants signed manifests
   shouldn't have to pull libp2p. Rejected.
3. **All Apache-2.0** — maximally permissive but lets a well-funded actor
   fork LUCID into a closed inference product. Rejected as antithetical to
   the mission.
4. **All AGPL-3.0** — maximally protective but creates legal friction for
   protocol adopters, including governments and corporates whose legal
   review of AGPL is slow or hostile. Rejected for the substrate.

**Consequences**:

*Positive*:
- LUCID can depend on stable, versioned interfaces.
- Substrate crates are independently publishable on crates.io.
- License split matches the mission: substrate is adoptable everywhere; the
  flagship application is protected from closed forks.
- Future Phase nodes (phase-render, phase-science) just add another crate
  next to plasm and lucidd.

*Negative*:
- Cargo workspace plumbing across eight crates is more overhead than a single
  crate.
- Dual-license boundary requires every contributor to understand which crate
  they're modifying.
- Sequential crates.io publication required (phase-identity first, then
  fan out).

**References**:
- `memory-bank/releases/phase-core/index.yaml`
- `memory-bank/MISSION.md`
- M7 task: `memory-bank/tasks/2026-05/`

---

### 2026-05-22: Streaming Worker Trait with Commitment-Accumulator Signing

**Status**: Accepted

**Context**:
The original `daemon/` ExecutionHandler signed a single receipt at the end of
each job. That shape works for one-shot WASM execution, where the result is
a small `JobResult` blob produced atomically. It does not work for inference,
where output is a token stream that may run for minutes, where the client
needs to see tokens as they arrive (Ollama SSE), and where the receipt must
still cryptographically commit to the full output that was actually produced
— including the case where the stream was cancelled partway through.

**Decision**:

The `Worker` trait in `phase-protocol` returns a streaming handle, not a
finished result:

```rust
async fn execute(
    &self,
    job: SignedManifest<JobSpec>,
) -> Result<(JobHandle, JobStream), WorkerError>;
```

`JobStream` is an `impl Stream<Item = JobChunk>`. The signed receipt is
produced at stream-close time by a **commitment accumulator**: each chunk is
hashed into a running SHA-256 state, the final state is the chunk-tree root,
and the `SignedReceipt<JobResult>` signs `(job_id, chunk_count, chunk_tree_root,
exit_code, wall_time_ms, completed_at)`. Cancellation produces a receipt
covering the chunks actually delivered.

`DynWorker` is the object-safe shim — it boxes the future so we can store
`Arc<dyn DynWorker>` in dispatch tables and the `phase-net` swarm.

**Alternatives Considered**:
1. **Sign the full output buffer at end** — works for short outputs but
   requires buffering an entire inference response. Memory-prohibitive for
   long generations and breaks the streaming contract clients expect.
   Rejected.
2. **Sign every chunk individually** — gives per-chunk verifiability but
   multiplies Ed25519 cost by chunk count (a 1k-token response would sign
   1k times) and produces a receipt graph instead of a single receipt.
   Rejected as overhead-prohibitive for v0.1.
3. **No receipt for streamed jobs** — simplest but breaks the protocol's
   core guarantee that every result is verifiable. Rejected.

**Consequences**:

*Positive*:
- One Ed25519 signature per job regardless of output length.
- Cryptographic commitment to actual delivered chunks, including the
  cancelled-stream case.
- Same trait shape works for WASM (single-chunk stream) and inference
  (many-chunk stream). Plasm's `WasmtimeWorker` emits one chunk and closes;
  LUCID's `LlamaCppWorker` emits chunks per token.
- Trait shape validated against a fake streaming worker and a real Ollama
  client before any extraction began — design wasn't speculative.

*Negative*:
- More moving parts than a one-shot worker. `JobHandle` carries cancellation
  state; `JobStream` is `Send + 'static`-bound.
- Verifiers must reconstruct the chunk tree to verify, requiring chunk
  ordering to be deterministic.
- `Pin<Box<dyn Future<Output = ...> + Send + '_>>` in `DynWorker` is the
  textbook "very complex type" pattern; suppressed with a targeted
  `#[allow(clippy::type_complexity)]` and a comment explaining the trade-off.

**References**:
- `crates/phase-protocol/src/worker.rs`
- `crates/phase-receipt/src/receipt.rs` (commitment accumulator)
- Pre-M4 validation: `tasks/Design streaming Worker trait` + `tasks/Validate Worker trait against fake streaming worker + real Ollama client`

---

### 2026-05-26: PHP SDK Dual-Format Signing Migration

**Status**: Accepted

**Context**:
The November 2025 MVP signed receipts with a legacy SHA-256 over
`version|module_hash|exit_code|wall_time_ms|timestamp`. M5 introduced
canonical signing — Ed25519 over `"phase-receipt:v1:" + canonical_json(...)` —
where the canonical JSON covers `{completed_at, job_id, result,
schema_version}`. Existing deployments using the November MVP receipts
must continue to verify. New clients (post-M7) emit the canonical format.

**Decision**:

The PHP SDK's `Crypto::verifyReceipt()` and `Receipt::canonicalBytes()`
auto-detect which format a receipt uses and verify accordingly:

1. If the receipt JSON contains a `schema_version` field, verify against
   `"phase-receipt:v1:" + canonical_json({completed_at, job_id, result,
   schema_version})`.
2. Otherwise, fall back to the legacy SHA-256 over
   `version|module_hash|exit_code|wall_time_ms|timestamp`.

Both code paths live in `php-sdk/src/Crypto.php` and `php-sdk/src/Receipt.php`.
No flag, no environment variable — the format is detected from the receipt
itself. New Plasm receipts and all LUCID receipts use the v1 format.

**Alternatives Considered**:
1. **Hard break, v1 only** — cleanest code but breaks every November MVP
   deployment that still trusts a node's pre-M7 receipts. Rejected.
2. **Configurable verifier mode** — explicit but pushes complexity onto
   integrators who shouldn't have to know about the protocol version.
   Rejected.
3. **Wrap legacy receipts in a v1 envelope on the node side** — moves the
   compatibility burden server-side but means the Rust daemon has to know
   about historic receipt shapes forever. Rejected; the PHP SDK is the
   right place for this since it owns the verifier.

**Consequences**:

*Positive*:
- Existing PHP integrations keep working without code changes.
- Test fixtures from the November MVP still verify.
- Future schema versions can be added by extending the detection branch.

*Negative*:
- The SDK carries two verification code paths until legacy receipts are
  fully aged out (no fixed sunset date).
- New SDK adopters see both formats in the test suite and must understand
  the migration story.

**References**:
- `php-sdk/src/Crypto.php`
- `php-sdk/src/Receipt.php`
- `examples/php_compat_receipt_check.php`

---

### 2026-05-27: Peer-Relay is Batch (not Streaming) in v0.1

**Status**: Accepted (v0.1 scope decision; v0.2 will revisit)

**Context**:
LUCID's local-or-DHT router needs a way for Node B (no model loaded) to ask Node A (model loaded) to execute an inference job and stream results back. The Worker trait is natively streaming via `JobStream`. The question is whether the peer-to-peer transport over libp2p should also stream tokens, or batch them.

**Decision**:
For v0.1, the peer relay (`/phase/job-relay/1.0.0`) is **batch**: the serving peer drains its local JobStream to completion, then ships the whole `Vec<JobEvent>` as a single CBOR-encoded response. The requesting peer decodes the vector and synthesizes a JobStream that yields the events in order.

**Alternatives Considered**:
1. **Token streaming via libp2p substream** — natural fit but libp2p's `request_response` is request-then-response; achieving true streaming requires either a custom NetworkBehaviour or chained protocol messages. Doable but a multi-week design exercise.
2. **Batch (chosen)** — accepts that routed inference has worse first-token latency in exchange for shipping the protocol now. The protocol shape is correct; only the wire format changes when streaming lands in v0.2.
3. **Out-of-band streaming over a side channel (HTTP/QUIC direct between peers)** — defeats the libp2p NAT-traversal story.

**Consequences**:
- Positive: ships in a single session; protocol shape proven end-to-end; receipts and commitments work identically.
- Negative: peer-routed inference has multi-second first-token latency at minimum (full generation time + one round trip). Document expectation up front; users with TTFT-sensitive workloads use `X-Lucid-Local-Only`.
- v0.2 streaming is a wire-format change, not a protocol redesign — `JobEvent` framing stays the same, only the substream pattern changes.

**References**:
- `crates/lucidd/src/router.rs`
- `crates/phase-net/src/protocol.rs` (`JobRelayRequest`, `JobRelayResponse`)
- `crates/phase-net/src/discovery.rs` (`/phase/job-relay/1.0.0` behaviour wiring)

---

### 2026-05-27: Model Registry Advertisement Schema and TTL Refresh

**Status**: Accepted

**Context**:
LUCID's router needs a way to find which peers have a given model loaded. Phase has a DHT (Kademlia via phase-net); the question is how to schematize the records, how to sign them, and how long they live.

**Decision**:
**`SignedModelAdvertisement`** records, bincode-encoded, keyed at `b"phase/model/" || model_cid` (44 bytes total). Each record carries `(schema_version, ModelCapabilities, pubkey, signature)` where the signature is Ed25519 over a canonical bincode encoding of everything except the signature itself. `ADVERTISEMENT_TTL = 15 minutes`; `TTL_REFRESH_INTERVAL = 5 minutes` — a per-CID background task re-publishes the record every 5 minutes while the model stays loaded.

`ModelCapabilities` is intentionally workload-narrow: `model_id` (human-readable string), `model_cid` ([u8; 32] SHA-256 of GGUF), `quantization` (e.g. "Q4_K_M"), `context_length`, `max_concurrent`, `backend` ("llama.cpp" | "mlx"), `advertised_at`, `valid_until`. **Network-condition fields (latency, bandwidth) intentionally do NOT live here** — those live in `PeerCapabilities` (phase-net), workload-agnostic.

**Alternatives Considered**:
1. **JSON-encoded records** — human-debuggable but ~2x larger over the DHT.
2. **Single periodic broadcast instead of per-CID TTL refresh** — simpler but stale records linger longer when a peer goes offline.
3. **Bundling model advertisements with PeerCapabilities** — would couple workload to substrate; rejected per MISSION.md's "substrate first" principle.

**Consequences**:
- Positive: lookup is O(1) DHT query; tampering is detectable via Ed25519; cross-restart continuity via persistent identity from phase-identity.
- Negative: peers that drop offline ungracefully leave stale records for up to 15 minutes. Acceptable for v0.1; M5 router falls back to next peer if relay fails.
- Cross-peer `model_id → model_cid` index is NOT built — `find_peers_by_model_id` only resolves names this node has loaded locally. Cross-peer name registry is a v0.2 polish target.

**References**:
- `crates/lucidd/src/registry.rs`
- `crates/lucidd/src/dht_transport.rs`

---

### 2026-05-28: Bootstrap Peers Wired via CLI / Config (Not DNS — Yet)

**Status**: Accepted (v0.2 substrate prep). DNS-based bootstrap deferred to a follow-up sprint.

**Context**:
mDNS handles peer discovery on a LAN. For WAN discovery (the actual "anyone can find anyone" story from MISSION.md) peers need *some* way to find their first hop. The `phase-net::DiscoveryConfig::bootstrap_peers` field existed since November 2025 but was a no-op — it parsed the multiaddr, logged it, and never actually dialed.

**Decision**:
Wire the existing field so each entry in `bootstrap_peers` actually (a) parses as a multiaddr with a `/p2p/<peer-id>` tail, (b) adds the address to Kademlia's routing table, (c) queues a `swarm.dial(...)`. Expose this through lucidd via repeatable `--bootstrap-peer <multiaddr>`. **No DNS lookup in v0.2 substrate prep.**

**Alternatives Considered**:
1. **DNS-based seeds at `bootstrap.phasebased.net`** (Bitcoin Core pattern) — the right long-term answer, but requires owning DNS records, picking a TXT format, and writing the DNS-resolution code path. Bigger scope; deferred.
2. **Hardcoded foundation peer-ids in the binary** — embed N peer-ids in the lucidd source. Operationally brittle (releases needed to add/remove relays) and centralizes trust in the build pipeline. Rejected.
3. **libp2p Rendezvous Protocol** — proper service discovery, but requires the server side (`rendezvous::server::Behaviour`) to be wired into phase-net's `NetworkBehaviour`. Real v0.2 engineering work. Will be done — but the simpler `--bootstrap-peer` path unblocks today.

**Consequences**:
- Positive: minimum viable WAN bootstrap, works today, simple to reason about.
- Negative: relay rotation requires consumers to update their config file or CLI flags. Acceptable for the v0.1.1 audience (technical contributors).
- DNS-based seeds remain a clear v0.2 milestone — the wire-up here is *additive*, not a replacement.

**References**:
- `crates/phase-net/src/discovery.rs` (bootstrap-peer wire-up)
- `crates/lucidd/src/main.rs` (CLI flag + `DiscoveryConfig` passthrough)
- First foundation relay coordinates in `activeContext.md`

---

### 2026-05-28: Persistent Identity by Default, User-Level systemd for Relays

**Status**: Accepted

**Context**:
Two operational sub-decisions landed in the same session:

(a) `lucidd/main.rs` was calling `NodeIdentity::generate()` every startup (fresh ephemeral keypair), so every restart produced a new peer-id. That's incompatible with "be a bootstrap peer" because the peer-id is encoded in the multiaddr other nodes use to dial you.

(b) Standing up lucidd as a long-running service required a process supervisor. `nohup` survives the launching shell but not reboot; raw cron-on-reboot lacks logging and restart-on-failure.

**Decision**:
(a) `NodeIdentity::load_or_create(path)` with `default_identity_path()` (platform-aware: `~/.config/phase/identity.key` on Linux). `--identity-path <path>` overrides. `phase-identity` already had `load_or_create`; main.rs just needed to use it.

(b) **User-level systemd service** (`~/.config/systemd/user/lucidd-relay.service`, `WantedBy=default.target`). Combined with `loginctl enable-linger <user>`, the service starts at boot and survives logout — without ever needing root privileges to install. The unit file ships in the repo at `crates/lucidd/systemd/lucidd-relay.service`.

**Alternatives Considered**:
- **System-level service at `/etc/systemd/system/`** — standard pattern, but requires sudo for installation, places lucidd in the root-owned filesystem, and bleeds permission concerns into the operator setup story. Rejected for v0.2 prep; will reconsider when running on dedicated VPSes where the user IS root.
- **Manual `nohup`** — adequate for one demo, brittle for any persistent role. Rejected.
- **Docker container** — adds a runtime dependency. Lucidd is a single static-ish ELF binary; the simplest deployment is `install -m 0755 lucidd ~/bin/lucidd` + a systemd unit. Future packaged release may use systemd-nspawn or similar; not now.

**Consequences**:
- Positive: relay node restarts cleanly, journals to systemd-journald, survives logout, fits the homelab-operator pattern that the early-adopter audience already knows.
- Negative: relay operators need to know that `loginctl enable-linger <user>` is a one-time sudo step. Documented in the unit file and the dist README.
- User-level services have weaker security-hardening options than system-level (no `User=`, no `PrivateNetwork=`, etc.). Acceptable v0.2 trade-off; revisited when the foundation operates on cloud VPSes.

**References**:
- `crates/lucidd/systemd/lucidd-relay.service`
- `crates/lucidd/src/main.rs` (identity path resolution)
- `crates/phase-identity/src/keypair.rs` (`load_or_create` was there since M3)

---

### 2026-05-27: Auto-Pause Policy is Declarative TOML + Pause-Not-Deprioritize

**Status**: Accepted

**Context**:
LUCID operators need granular control over *when* this node serves inference for peers — battery state, thermals, time-of-day, manual override. Two design axes mattered: (a) what's the policy surface (UI slider vs config file vs code) and (b) what's the behavior when conditions degrade (deprioritize via DHT signal vs auto-pause locally).

**Decision**:
**Declarative `lucid-policy.toml`** at the OS-standard config dir (`~/.config/lucidd/policy.toml` on Linux). Operator edits the file; lucidd reloads via `notify` filesystem watch + SIGHUP. No UI in v0.1.

**Pause, not deprioritize**: when conditions degrade (battery, thermal, manual), the node *refuses* to serve peer requests (503 + reason). It does not advertise itself as "low priority" — peers simply route to someone else. This avoids the Bittensor-shaped game where peers self-report degraded status to game ranking.

**Decision order** (first match wins): `Manual` → `OnBattery` → `ThermalLimit` → `OutsideTimeWindow` → `ConcurrencyLimit` → `ModelNotInAllowlist` → `Allow`.

**Alternatives Considered**:
1. **GUI slider in v0.1** — premature; TOML can ship today, UI can layer on later.
2. **DHT-broadcast load signal** ("I am at 80% capacity, please prefer others") — interesting but gameable; deferred to post-v0.1 reputation work.
3. **Single boolean "serve / don't serve"** — too coarse; users explicitly want time-of-day and battery as separate axes.

**Consequences**:
- Positive: operators understand the policy by reading a commented config file; reload is hot; clear refusal reasons give downstream routers actionable signal.
- Negative: TOML editing is unfriendly for non-technical operators (acceptable — v0.1 audience is technical contributors).
- Known UX wart: `PolicyEngine::should_serve` refuses *self*-traffic when on battery (operator sovereignty rule applies uniformly). v0.1.1 may add a "self-traffic always allowed" knob.

**References**:
- `crates/lucidd/src/policy.rs`
- MISSION.md (the auto-pause discussion in the early sprint)

---

### 2026-05-28: Hybrid signer-authorization policy (default-deny allowlist + PeerID-bind)

**Status**: Accepted (security hardening SEC-01; the v0.1 authorization model)

**Context**:
The 2026-05-28 audit's keystone finding (C1): `SignedManifest::verify()` checks a signature against the pubkey embedded *in the object*, proving "someone signed this" but not "an authorized party signed this." The lucidd relay handler didn't even call `verify()`, and plasm ran any self-signed WASM. With no authorization, any anonymous internet peer could spend a worker node's GPU / run arbitrary sandboxed code. A policy decision was required: how does a node decide which signed jobs to execute?

**Decision**:
**Hybrid.** A job is authorized if EITHER (a) its signer pubkey is in the operator's `authorized_submitters` allowlist, OR (b) the signer pubkey derives to the libp2p `PeerId` that delivered the request (PeerID-bind). Default-deny: empty allowlist + an `allow_unauthenticated_jobs = false` escape hatch (documented insecure, for local dev/testing). Server-side resource caps (`max_memory`/`max_duration`/`max_tokens`) clamp manifest-supplied values regardless of what the manifest claims.

**Alternatives Considered**:
1. **Pure allowlist** — simplest, matches the documented soft-launch plan, but requires out-of-band key exchange for every contributor.
2. **Pure open + rate-limit + reputation** — the eventual v0.2+ shape, but reputation infrastructure doesn't exist yet; open-without-it is the current vulnerability.
3. **Hybrid (chosen)** — allowlist for curated soft-launch peers, PeerID-bind so a connected peer is attributable even without prior allowlisting, escape hatch for local dev. Ships the safe default now with the v0.2 relaxation path already plumbed.

**Consequences**:
- Positive: closes anonymous-execution; default-deny is safe-by-default; PeerID-bind makes every accepted job attributable to a libp2p identity; caps bound resource abuse.
- Negative: contributors must be allowlisted (or rely on PeerID-bind semantics) — friction matching the intended soft launch.
- The decision was delegated to the agent during the "get'r done" hardening run; recommended option taken and flagged for Michael's review. Reversible via config.

**Revisit trigger** (Michael, 2026-05-29): default-deny is right *for now*. Flip the default toward open once (a) the network serves sliced/distributed models (v0.2 `ExoProxyWorker` / multi-node sharding) AND (b) the inference infra can handle open load — gated by the rate-limit + reputation end-state above. Capability-gated, not date-gated; treat the flip as an in-scope deliverable of the v0.2 sharding milestone. Until then, use `allow_unauthenticated_jobs=true` for local dev rather than flipping the production default early.

**References**: `crates/lucidd/src/policy.rs`, `crates/lucidd/src/router.rs` (`make_inbound_relay_handler`), `crates/plasm/src/worker.rs`, SEC-01 task file.

---

### 2026-05-28: Document-and-justify unreachable advisories; cargo-deny scoped to first-party

**Status**: Accepted (security hardening SEC-02/SEC-00)

**Context**:
After upgrading wasmtime (27→36) and hickory (0.24→0.26) cleared 17 of 20 advisories, three remained that cannot be fixed without forking upstream: two hickory-proto 0.25.2 advisories reachable only via `libp2p-mdns 0.48` (which hard-pins `^0.25.2`), and one `nix 0.19.1` advisory pulled by `battery` only under `cfg(freebsd/dragonfly)`. Forcing a `[patch]` to a breaking hickory line would not compile; the nix code never builds on our targets. Question: fail the gate forever, or suppress?

**Decision**:
**Document-and-justify, never blanket-suppress.** Each accepted advisory is listed in `.cargo/audit.toml` AND `deny.toml` with a per-ID reachability justification and a "clear when…" condition. For `cargo deny`, unmaintained-crate checking is scoped `unmaintained = "workspace"` (encoded in config, not a CLI flag) so the gate fails only when a *first-party/direct* dependency goes unmaintained (actionable — this is the signal SEC-12 acted on for bincode), while transitive unmaintained crates we don't control stay visible in `cargo audit` output without blocking.

**Alternatives Considered**:
1. **Force `[patch]` to patched upstream lines** — doesn't compile (breaking API in a transitively-pinned crate); rejected.
2. **Blanket-disable advisory checking** — defeats the purpose; rejected.
3. **Document-and-justify with reachability analysis (chosen)** — honest, keeps `cargo audit`/`cargo deny` green and meaningful, with explicit revisit triggers tied to upstream releases.

**Consequences**:
- Positive: the CI advisory gate is both green and honest; a genuinely-new vulnerability still fails; revisit conditions are written down.
- Negative: the ignore list must be kept in sync between `.cargo/audit.toml` and `deny.toml` (noted in both files); accepted advisories require periodic re-review as upstreams release.

**References**: `.cargo/audit.toml`, `deny.toml`, `.github/workflows/security.yml`, SEC-00 / SEC-02 task files.

---

### 2026-05-28: bincode → postcard for signed DHT records (schema v2 clean break)

**Status**: Accepted (security hardening SEC-12)

**Context**:
`bincode 1.x` (unmaintained, RUSTSEC-2025-0141) was used directly in `registry.rs` both for the DHT wire envelope and for the canonical bytes that get Ed25519-signed in `SignedModelAdvertisement`. Because the encoding is part of the signed canonical bytes, swapping it changes what verifies.

**Decision**:
Migrate to **postcard** (maintained, deterministic, compact — good for signed payloads). Bump `ADVERTISEMENT_SCHEMA_VERSION` 1→2 and take a deliberate clean break: a v2 reader rejects v1 records on the schema-version check before signature verification. Acceptable because the v0.1 network is tiny. A single private helper produces both the signed canonical bytes and the wire encoding, so they cannot drift.

**Alternatives Considered**:
1. **bincode 2.x** — maintained successor but a different API and still less deterministic-by-design than postcard.
2. **Reuse canonical-JSON** (as manifests/receipts do) — consistent, but ~2× larger over the DHT.
3. **postcard (chosen)** — compact, stable wire format, deterministic, actively maintained.

**Consequences**:
- Positive: off the unmaintained crate; smaller records; one code path for sign + wire so no drift.
- Negative: v1↔v2 advertisements don't interoperate (clean break) — fine at v0.1 scale; postcard transitively pulls `heapless`→`atomic-polyfill` (itself unmaintained, transitive — accepted per the cargo-deny scoping decision above).

**References**: `crates/lucidd/src/registry.rs`, SEC-12 task file.

---

### 2026-05-29: Per-workload node implementations (diffusion is a separate node, not a LUCID mode)

**Status**: Accepted (architectural direction; informs present-day LUCID + substrate discipline, implementation post-v0.1)

**Context**:
Prompted by reviewing ComfyUI PR #7063 (single-host multi-GPU work-unit splitting for diffusion sampling) and asking what it implies for LUCID. The substrate (`phase-net`/`phase-identity`/`phase-manifest`/`phase-receipt`/`phase-protocol`/`phase-artifact-server`) is workload-agnostic by design; `JobSpec` is `#[non_exhaustive]` and the `Worker` trait was built so new workload types slot in (MISSION.md lists "phase-render" / image-gen / science as future). Question: should diffusion be a LUCID feature/mode, or its own node?

**Decision**:
**Diffusion (and other non-streaming, blob-output, non-autoregressive workloads) is a SEPARATE Phase node implementation** — its own daemon + its own `JobSpec` variant(s) + its own `Worker` impl + its own native API surface — sharing the same Phase substrate crates. Not a mode inside `lucidd`. The shape differences are fundamental, not cosmetic:

| Axis | LUCID (LLM inference) | Diffusion node |
|---|---|---|
| Client API (the compat wedge) | Ollama `/api/chat`, token streaming | ComfyUI graph / `diffusers` / A1111 `/sdapi` — different ecosystem |
| Result shape | stream of tokens; commitment = SHA-256 chain over chunks | large image/video blob (+ optional denoise-step previews) |
| Execution profile | autoregressive, KV-cache, latency-per-token, context length | fixed-step denoise loop, no KV-cache, batch/conditioning-parallel, VRAM = model+latents+VAE |
| Backend wrapped | llama.cpp / MLX | ComfyUI / diffusers / sd.cpp |
| Model format | GGUF | safetensors/checkpoints + LoRA/VAE/embedding ecosystem |
| Substrate stress | compute protocol; barely touches artifact-server | **hammers `phase-artifact-server`** — checkpoints are GBs, outputs are large blobs |

**Key observation**: diffusion is the workload that exercises the *content-addressed-blob half* of the substrate that LUCID v0.1 leaves mostly idle (LUCID's weights are out-of-band, outputs are small token streams). The substrate was designed with both halves; LUCID validates the compute half, a diffusion node validates the artifact half. This is evidence the workload-agnostic bet is sound — but only if we hold the line below.

**Present-day constraint (the actionable part)**: keep `phase-protocol` (`JobSpec`, `Worker`, `JobEvent`, the commitment model) free of inference-specific assumptions, so a diffusion node slots in **without substrate changes**. Any inference-ism leaking into the protocol layer (token-shaped events as the only event type, KV-cache concepts in the trait, Ollama assumptions in `phase-*`) is a substrate-first violation to be caught in review. This is "substrate first, then product" (MISSION.md principle 1) made concrete.

**Alternatives Considered**:
1. **Diffusion as a LUCID mode** — rejected; would force two unrelated API surfaces + result shapes + scheduling models into one daemon, and bleed inference assumptions into shared code.
2. **A generic "media" node covering inference + diffusion** — rejected; the only thing they share is the substrate, which is already the shared layer. A node's job is to be native to its ecosystem's clients.

**Consequences**:
- Positive: each node speaks its ecosystem's native protocol (compatibility-as-wedge, per-domain); the substrate stays clean; proves workload-agnosticism with a second real consumer.
- Naming: substrate crates stay `phase-*`; node flagships get product brands. The diffusion flagship is named **LUMEN** (decided 2026-05-29). Rationale: it extends LUCID's light metaphor (Phase → LUCID → LUMEN reads as one deliberate optics/wave-physics family), the word carries an unambiguously positive meaning (luminous flux; "light" in Latin; illumination/making-visible) independent of any backronym, and it dodges two traps that killed the alternatives — **PRISM** (the name of the NSA mass-surveillance program; brand-radioactive for a credibly-neutral anti-surveillance project) and **MIRAGE** (connotes illusion/"not real" — exactly the deepfake/synthetic-misinformation anxiety a generative-image tool should not lean into). Node roster: `plasm` (WASM), LUCID (LLM inference), LUMEN (diffusion). **Trademark caveat to verify at launch**: Unreal Engine's global-illumination system is also called "Lumen" — different category (game-engine feature vs. distributed-compute product), but check before public launch. Substrate-side working name remains `phase-render` until the node exists.
- Timing: post-LUCID-v0.1, likely parallel to / after LUCID v0.2. Not now. Recording it now is to (a) confirm the substrate bet and (b) constrain what does NOT leak into `phase-protocol` during LUCID development.

**References**: MISSION.md (layering + "Future" nodes), `crates/phase-protocol/` (`JobSpec` `#[non_exhaustive]`, `Worker`), ComfyUI PR #7063 (the prompt).

---

### 2026-05-29: Verifying sharded / partial computation is an unsolved open problem (recorded risk)

**Status**: Open problem — recorded, not solved. Blocks the trust story for v0.2 `ExoProxyWorker` (LUCID sharding) and for any distributed diffusion.

**Context**:
v0.1's verifiable-compute guarantee rests on the commitment accumulator (`phase-protocol/commitment.rs`): a worker folds emitted output chunks into a SHA-256 chain, signs the terminal state, and a verifier **replays the received chunks** to confirm the worker produced exactly what it signed. This works because the verifier *has* the output and can cheaply recompute the commitment over it. SEC-05 wired this into the consumer side (receipt verify + bind).

That model **breaks for sharded / partial computation**:
- **Sharded LLM (v0.2 ExoProxyWorker)**: a peer computes an intermediate hidden-state tensor for layers N..M and hands it to the next peer. There is no cheap way for the requester to verify that tensor is correct without re-running those layers (which defeats the point of offloading them). The commitment-replay trick doesn't apply — you can't replay what you didn't run.
- **Distributed diffusion**: same shape — a peer denoises some steps / some conditioning; verifying it did so honestly means re-doing the work. Worse, cross-hardware float nondeterminism means two *honest* GPUs produce slightly different outputs, so bitwise reproduction fails and you're into fuzzy/perceptual comparison.

This is a genuinely hard, partly-open research area (verifiable/attested computation, redundant execution + voting, ZK proofs of inference — all expensive or immature). It is distinct from, and harder than, the token-commitment v0.1 ships.

**Candidate directions (none chosen)**:
1. **Redundant execution + cross-check** — run the same shard on K peers, compare (with a tolerance for float nondeterminism), trust the majority. Costs Kx compute; tolerance tuning is fiddly.
2. **Reputation + random spot-checking** — occasionally re-run a shard yourself and ding peers that diverge. Probabilistic, cheap, pairs with the reputation system already planned for v0.2.
3. **Trusted execution (TEE/attestation)** — strong but hardware-gated and against the "anyone's consumer GPU" ethos.
4. **ZK proofs of inference** — cryptographically ideal, currently far too expensive for real model sizes.

**Consequences / why recorded now**:
- The v0.2 sharding milestone must treat "how do we verify a peer's partial result" as a **first-class design problem with its own decision**, not an afterthought — the v0.1 receipt/commitment machinery does not cover it.
- This is *why* default-deny authorization (the 2026-05-28 hybrid-authz ADR) matters **more**, not less, once peers compute partial results for each other: until partial-compute verification exists, you want jobs flowing only among attributable/curated peers. Ties directly to the "flip the authz default once infra can handle it" trigger.
- Likely landing spot: redundant-execution + reputation spot-checking (2 + 1 combined) as the pragmatic v0.2 answer, with TEE/ZK as later research. Not decided here.

**References**: `crates/phase-protocol/commitment.rs`, `crates/lucidd/src/router.rs` (SEC-05 receipt verify+bind), the 2026-05-28 hybrid-authz ADR, releases/lucid (v0.2 `ExoProxyWorker`).

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
