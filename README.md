# Phase

Phase is the open verifiable compute protocol — a credibly-neutral, vendor-agnostic substrate for advertising, discovering, and verifying compute across machines that do not trust each other, without payments, without KYC, without lock-in. The protocol is workload-neutral: WASM today, GPU inference next (LUCID), anything that benefits from verifiable distributed compute after that. For everyone: smart computing, on the public's hardware, that nobody can turn off.

See [memory-bank/MISSION.md](memory-bank/MISSION.md) for the long-form rationale.

---

## Status

- **Phase Core** — complete (Nov 2025 → May 2026). The libp2p + Ed25519 substrate has been extracted from the legacy `daemon/` tree into seven publishable Rust library crates plus the repositioned Plasm reference node.
- **LUCID** — in progress. The inference flagship daemon (`crates/lucidd`) is scaffolded; M2–M8 build out the LlamaCpp/MLX workers, the Ollama API surface, the local-or-DHT router, and the two-node demo.

Workspace state: 152 tests passing, `cargo build --workspace` clean, zero clippy warnings across phase-core crates, legacy `daemon/` removed (history preserved in git).

---

## Repository Structure

```
phase/
├── Cargo.toml                 # Workspace root, resolver = 2
├── crates/
│   ├── phase-identity/        # Persistent Ed25519 node identity
│   ├── phase-net/             # libp2p / Kademlia / mDNS / Noise+QUIC
│   ├── phase-manifest/        # SignedManifest<T>, generic over payload
│   ├── phase-receipt/         # SignedReceipt<T>, generic over JobResult
│   ├── phase-protocol/        # JobSpec enum + Worker trait + JobStream
│   ├── phase-artifact-server/ # Content-addressed HTTP server
│   ├── plasm/                 # Reference WASM Phase node (plasmd binary)
│   └── lucidd/                # LUCID inference Phase node (in progress)
├── php-sdk/                   # PHP client SDK (legacy + phase-receipt:v1: signing)
├── wasm-examples/             # Source for hello.wasm
├── examples/                  # hello.wasm artifact + PHP demos
├── boot/                      # Phase Boot (USB/netboot initramfs)
├── memory-bank/               # Project documentation
│   ├── MISSION.md
│   ├── releases/
│   │   ├── phase-core/        # Library extraction release plan
│   │   └── lucid/             # Inference flagship release plan
│   └── tasks/                 # Per-month task records
└── CLAUDE.md / AGENTS.md      # AI-assist development workflow
```

### Crate dependency graph

```
phase-identity   ← leaf
  ├── phase-net
  ├── phase-manifest
  └── phase-receipt
          └── phase-protocol
                  ├── phase-artifact-server  (also depends on phase-net)
                  ├── plasm                  (WASM reference node)
                  └── lucidd                 (inference flagship, in progress)
```

No upward references. Build order is top to bottom.

---

## Quick Start

Build everything:

```bash
cargo build --workspace
```

Run the Plasm reference node:

```bash
cargo run -p plasm --bin plasmd -- start
```

Execute the hello.wasm sample (reverses `Hello, World` to `dlroW ,olleH`) and emit a signed receipt:

```bash
cargo run -p plasm --bin plasmd -- run examples/hello.wasm
```

Serve boot artifacts (Phase Boot provider role):

```bash
mkdir -p /tmp/boot-artifacts/stable/x86_64
cargo run -p plasm --bin plasmd -- serve --artifacts /tmp/boot-artifacts
```

Run the test suite:

```bash
cargo test --workspace
```

---

## Documentation

- [memory-bank/MISSION.md](memory-bank/MISSION.md) — what we are building and why
- [memory-bank/releases/phase-core/](memory-bank/releases/phase-core/) — substrate extraction release plan and milestone records
- [memory-bank/releases/lucid/](memory-bank/releases/lucid/) — inference flagship release plan
- [memory-bank/decisions.md](memory-bank/decisions.md) — architectural decision record
- [memory-bank/projectRules.md](memory-bank/projectRules.md) — coding and contribution conventions
- [CLAUDE.md](CLAUDE.md) — AI-assist development workflow (AGENTS.md-compatible)

---

## License

- `phase-identity`, `phase-net`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`, `plasm` — Apache-2.0. The substrate must be adoptable without legal friction by anyone, including parties hostile to one another.
- `lucidd` — AGPL-3.0-or-later. The flagship application is copyleft so no party can fork it closed.

See per-crate `Cargo.toml` for canonical license declarations and the top-level [LICENSE](LICENSE) file for the Apache-2.0 text.
