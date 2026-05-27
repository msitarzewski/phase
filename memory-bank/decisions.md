# Architectural Decisions: Phase Open MVP + Phase Core + LUCID

**Last Updated**: 2026-05-27
**Version**: 0.3 (phase-core + LUCID software released May 2026)

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
