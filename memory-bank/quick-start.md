# Quick Start: Phase Development Guide

**Last Updated**: 2025-11-09
**Version**: 0.1
**Audience**: Developers working on Phase Open MVP

---

## Table of Contents

1. [Architecture: Library + Binary Pattern](#architecture-library--binary-pattern)
2. [Session Startup](#session-startup)
3. [Common Patterns](#common-patterns)
4. [File Locations](#file-locations)
5. [Code Snippets](#code-snippets)
6. [Troubleshooting](#troubleshooting)
7. [Quick Commands](#quick-commands)

---

## Architecture: Library + Binary Pattern

**IMPORTANT**: Phase uses the standard Rust pattern of **library + binary crate** instead of a standalone binary.

### Why This Matters

✅ **Do this from the start**, not as a refactor:
- Zero `#[allow(dead_code)]` suppressions needed
- All "unused" code is actually public library API
- Clean compiler warnings
- Reusable for other Rust projects
- Standard pattern (ripgrep, tokio, clap all use this)

❌ **Don't do this**:
- Build everything as binary modules (`mod config; mod wasm;`)
- Add `#[allow(dead_code)]` to silence warnings
- Refactor to library later

### Structure

```
daemon/
├── Cargo.toml          # Defines both [lib] and [[bin]]
└── src/
    ├── lib.rs          # Library public API (re-exports everything)
    ├── main.rs         # Binary (uses `plasm::` library)
    ├── config.rs       # Internal module
    ├── wasm/           # Internal modules
    └── network/        # Internal modules
```

### Cargo.toml Configuration

```toml
[package]
name = "plasm"
version = "0.1.0"
edition = "2021"

[lib]
name = "plasm"
path = "src/lib.rs"

[[bin]]
name = "plasmd"
path = "src/main.rs"
```

### lib.rs Pattern

**Purpose**: Expose clean public API

```rust
//! # Plasm - Phase Local WASM Execution Library
//!
//! Core functionality for the Phase distributed compute network.

pub mod config;
pub mod wasm;
pub mod network;

// Re-export commonly used types for convenience
pub use config::{Config, ExecutionLimits};
pub use wasm::{
    runtime::{WasmRuntime, Wasm3Runtime, ExecutionResult},
    manifest::JobManifest,
    receipt::Receipt,
};
pub use network::{
    Discovery,
    DiscoveryConfig,
    ExecutionHandler,
    // ... protocol types
};
```

### main.rs Pattern

**Purpose**: Thin CLI wrapper using library

```rust
// Use the library crate, not internal modules
use plasm::{
    network::{Discovery, DiscoveryConfig, ExecutionHandler},
    wasm::runtime::{WasmRuntime, Wasm3Runtime},
};

#[tokio::main]
async fn main() -> Result<()> {
    // CLI implementation uses library API
    let discovery = Discovery::new(DiscoveryConfig::default())?;
    // ...
}
```

### Benefits

| Aspect | Library + Binary | Binary Only |
|--------|-----------------|-------------|
| **Warnings** | Zero (all code is pub API) | Many (need `#[allow(dead_code)]`) |
| **Reusability** | Other Rust projects can depend on it | Not reusable |
| **Testing** | Integration tests use public API | Internal access only |
| **Standard** | How all major Rust projects work | Non-standard |
| **Maintenance** | Clear API boundary | Everything exposed to binary |

### When to Use Library Pattern

✅ **Always for projects with**:
- Public API that others might use
- Multiple binaries sharing code
- Complex functionality (not just a script)
- Long-term maintenance

❌ **Skip for**:
- One-off scripts
- Throwaway prototypes
- Truly private/internal-only tools

### Lesson Learned (Milestone 4)

**Problem**: After implementing Milestones 1-3, had 27 compiler warnings about unused code. All the code was tested and working, just not used by the `plasmd` binary.

**Wrong approach**: Add `#[allow(dead_code)]` everywhere (suppresses warnings, doesn't fix root cause)

**Right approach**: Refactor to library + binary pattern:
- Created `lib.rs` with public API exports
- Updated `main.rs` to use `plasm::` instead of `mod`
- Removed ALL `#[allow(dead_code)]` attributes
- Result: 0 warnings, clean public API, standard Rust pattern

**Time cost**: ~30 minutes to refactor

**Value**: Permanent clean architecture, no technical debt

### Quick Reference

```bash
# Build library + binary
cargo build

# Test library (unit tests)
cargo test --lib

# Test binary
cargo test --bin plasmd

# Build just the library
cargo build --lib

# Build just the binary
cargo build --bin plasmd

# Check library API docs
cargo doc --lib --open
```

---

## Session Startup

### First-Time Setup

```bash
# Clone repository
cd /Users/michael/Software/phase

# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install cargo-deb for packaging
cargo install cargo-deb

# Install PHP (macOS)
brew install php@8.1

# Install Composer (PHP package manager)
brew install composer
```

### Development Workflow

```bash
# Build daemon (debug)
cargo build

# Run tests
cargo test

# Run linter
cargo clippy -- -D warnings

# Format code
cargo fmt

# Build release binary
cargo build --release

# Create .deb package
cargo deb
```

---

## Common Patterns

### Adding a New WASM Execution Feature

**Steps**:
1. Update `WasmRuntime` trait in `daemon/src/wasm/runtime.rs`
2. Implement feature in `Wasm3Runtime` (MVP)
3. Add tests in `daemon/src/wasm/runtime_test.rs`
4. Document in `memory-bank/systemPatterns.md#WASM Execution Pattern`

**Example**:
```rust
// 1. Update trait
pub trait WasmRuntime {
    fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult>;
    fn execute_with_timeout(&self, wasm_bytes: &[u8], timeout: Duration) -> Result<ExecutionResult>; // NEW
}

// 2. Implement in Wasm3Runtime
impl WasmRuntime for Wasm3Runtime {
    fn execute_with_timeout(&self, wasm_bytes: &[u8], timeout: Duration) -> Result<ExecutionResult> {
        // Implementation
    }
}

// 3. Add test
#[test]
fn test_execute_with_timeout_succeeds() {
    let runtime = Wasm3Runtime::new().build().unwrap();
    let wasm_bytes = include_bytes!("../fixtures/hello.wasm");
    let result = runtime.execute_with_timeout(wasm_bytes, Duration::from_secs(5)).unwrap();
    assert_eq!(result.exit_code, 0);
}
```

---

### Adding a New Peer Discovery Feature

**Steps**:
1. Update libp2p behaviour in `daemon/src/network/discovery.rs`
2. Add event handler in network event loop
3. Add tests in `daemon/src/network/discovery_test.rs`
4. Document in `memory-bank/systemPatterns.md#Peer Discovery Pattern`

**Example**:
```rust
// 1. Update behaviour
use libp2p::kad::{Kademlia, KademliaEvent};

pub struct DiscoveryBehaviour {
    kademlia: Kademlia<MemoryStore>,
}

// 2. Handle events
match event {
    KademliaEvent::OutboundQueryProgressed { result, .. } => {
        match result {
            QueryResult::GetProviders(Ok(providers)) => {
                // Handle discovered peers
            }
            _ => {}
        }
    }
}

// 3. Test
#[tokio::test]
async fn test_peer_discovery() {
    let node1 = spawn_node().await;
    let node2 = spawn_node().await;
    node1.advertise("wasm3-x86_64").await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let peers = node2.discover("wasm3-x86_64").await.unwrap();
    assert!(peers.contains(&node1.peer_id()));
}
```

---

### Adding Error Handling

**Pattern**: Use `thiserror` for typed errors

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MyError {
    #[error("Failed to load file '{path}': {reason}")]
    FileLoadError { path: String, reason: String },

    #[error("Network timeout after {seconds}s")]
    TimeoutError { seconds: u64 },

    #[error(transparent)]
    IoError(#[from] std::io::Error),  // Auto-convert from io::Error
}

// Usage
fn load_file(path: &Path) -> Result<Vec<u8>, MyError> {
    std::fs::read(path)
        .map_err(|e| MyError::FileLoadError {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
}
```

---

## File Locations

### Quick Reference Map

```
phase/
├── memory-bank/                  # Documentation & planning
│   ├── toc.md                    # Index (start here)
│   ├── projectbrief.md           # Vision & goals
│   ├── systemPatterns.md         # Architecture patterns
│   ├── techContext.md            # Tech stack decisions
│   ├── activeContext.md          # Current sprint
│   ├── progress.md               # Status tracking
│   ├── projectRules.md           # Coding standards
│   ├── decisions.md              # ADRs
│   ├── quick-start.md            # This file
│   └── tasks/2025-11/            # Task documentation
│
├── daemon/                       # Rust daemon (NOT YET CREATED)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs
│   │   ├── wasm/
│   │   │   ├── runtime.rs        # WASM execution
│   │   │   ├── manifest.rs       # Job manifests
│   │   │   └── receipt.rs        # Execution receipts
│   │   ├── network/
│   │   │   ├── discovery.rs      # Peer discovery
│   │   │   ├── transport.rs      # libp2p transport
│   │   │   └── handshake.rs      # Job handshake
│   │   └── config.rs             # Configuration
│   └── tests/
│       └── fixtures/             # Test WASM modules
│
├── php-sdk/                      # PHP client (NOT YET CREATED)
│   ├── composer.json
│   ├── src/
│   │   ├── Client.php            # Main API
│   │   ├── Manifest.php          # Job manifest builder
│   │   ├── Receipt.php           # Receipt verification
│   │   └── Transport/
│   │       ├── LocalTransport.php
│   │       └── RemoteTransport.php
│   └── tests/
│
├── examples/                     # Demo applications
│   ├── hello.wasm                # Example WASM module (NOT YET CREATED)
│   └── remote_test.php           # End-to-end demo
│
├── docs/                         # External documentation (NOT YET CREATED)
│   ├── install.md
│   ├── architecture.md
│   └── api-reference.md
│
├── release_plan.yaml             # Milestone planning
├── README.md                     # Project overview
└── CLAUDE.md                     # AGENTS.md workflow
```

---

## Code Snippets

### WASM Execution (Rust)

```rust
use wasm3::Runtime as Wasm3Runtime;

// Create runtime with limits
let runtime = Wasm3Runtime::new()
    .with_memory_limit(128 * 1024 * 1024)  // 128 MB
    .with_stack_size(64 * 1024)            // 64 KB
    .build()?;

// Load and execute WASM
let wasm_bytes = std::fs::read("hello.wasm")?;
let result = runtime.execute(&wasm_bytes, &["arg1", "arg2"])?;

println!("Exit code: {}", result.exit_code);
println!("Stdout: {}", result.stdout);
println!("Wall time: {}ms", result.wall_time_ms);
```

### Manifest Creation (Rust)

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct JobManifest {
    pub version: String,
    pub module_hash: String,
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub timeout_seconds: u64,
}

// Create manifest
let manifest = JobManifest {
    version: "0.1".to_string(),
    module_hash: format!("sha256:{}", hex::encode(hash)),
    cpu_cores: 1,
    memory_mb: 128,
    timeout_seconds: 30,
};

// Serialize to JSON
let json = serde_json::to_string_pretty(&manifest)?;
std::fs::write("manifest.json", json)?;
```

### Receipt Verification (Rust)

```rust
use ed25519_dalek::{PublicKey, Signature, Verifier};

fn verify_receipt(receipt: &Receipt, signature: &[u8], public_key_bytes: &[u8]) -> Result<bool, Error> {
    // Parse public key
    let public_key = PublicKey::from_bytes(public_key_bytes)?;

    // Parse signature
    let signature = Signature::from_bytes(signature)?;

    // Serialize receipt for verification (must match signing format)
    let receipt_bytes = serde_json::to_vec(receipt)?;

    // Verify
    Ok(public_key.verify(&receipt_bytes, &signature).is_ok())
}
```

### PHP Client Usage

```php
<?php
require 'vendor/autoload.php';

use Plasm\Client;

// Create client (local mode for MVP)
$client = new Client(['mode' => 'local']);

// Create and submit job
$job = $client->createJob('hello.wasm')
    ->withCpu(1)
    ->withMemory(128)
    ->withTimeout(30)
    ->submit();

// Wait for result
$result = $job->wait();

// Display output
echo "Output: " . $result->stdout() . "\n";
echo "Exit code: " . $result->exitCode() . "\n";

// Verify receipt
if ($result->receipt()->verify()) {
    echo "Receipt verified ✓\n";
} else {
    echo "Receipt verification failed ✗\n";
}
```

---

## Troubleshooting

### Common Issues

#### Cargo build fails with "linker not found"

```bash
# macOS: Install Xcode Command Line Tools
xcode-select --install

# Linux: Install build-essential
sudo apt install build-essential
```

#### Clippy warnings about unused imports

```bash
# Auto-fix simple issues
cargo clippy --fix

# Or manually remove unused imports
# rustfmt will help organize
cargo fmt
```

#### WASM module won't load

```bash
# Check WASM validity
wasm-objdump -h hello.wasm

# Inspect exports
wasm-objdump -x hello.wasm | grep export
```

#### libp2p peer discovery not working

```rust
// Enable debug logging
RUST_LOG=libp2p=debug cargo run

// Check bootstrap nodes are reachable
// Verify firewall allows UDP (QUIC)
```

---

## Quick Commands

### Development

```bash
# Watch and rebuild on changes
cargo watch -x build

# Run specific test
cargo test test_execute_wasm -- --nocapture

# Run only unit tests (not integration)
cargo test --lib

# Run only integration tests
cargo test --test '*'

# Check code coverage
cargo tarpaulin --out Html
open tarpaulin-report.html
```

### Debugging

```bash
# Enable all logging
RUST_LOG=debug cargo run

# Enable specific module logging
RUST_LOG=plasm::wasm=trace cargo run

# Run with backtrace
RUST_BACKTRACE=1 cargo run

# Run with full backtrace
RUST_BACKTRACE=full cargo run
```

### Packaging

```bash
# Build .deb package
cargo deb

# Test .deb installation (requires Ubuntu VM)
dpkg -i target/debian/plasm_*.deb
systemctl status plasmd

# Uninstall
sudo dpkg -r plasm
```

### Git Workflow

```bash
# Create feature branch
git checkout -b feature/wasm-execution

# Commit with conventional format
git commit -m "feat: implement wasm3 runtime with stdout capture"

# Push and create PR
git push -u origin feature/wasm-execution
```

---

## Session Data

### Current State (2025-11-08)

**Active Milestone**: Milestone 1 - Local WASM Execution
**Status**: Planning complete, implementation not started
**Next Actions**:
1. Create daemon/ Rust workspace
2. Add wasm3 dependency
3. Implement basic runtime

**Recent Activity**:
- Created Memory Bank files (projectbrief, systemPatterns, techContext, etc.)
- Documented all 23 MVP tasks
- Established coding standards and patterns

**No Active Blockers**

---

## Memory Bank Quick Lookup

| Need | File | Section |
|------|------|---------|
| Project goals | projectbrief.md | Vision, MVP Scope |
| Architecture patterns | systemPatterns.md | Core Architecture, WASM Execution |
| Tech decisions | techContext.md | Technology Stack Overview |
| Current focus | activeContext.md | Current Sprint |
| Task status | progress.md | Release Milestones |
| Coding rules | projectRules.md | Rust/PHP Standards |
| Why decisions made | decisions.md | Decision Log |
| This guide | quick-start.md | All sections |
| Task history | tasks/2025-11/README.md | Monthly summary |

---

## Links & Resources

### Documentation
- **Rust Book**: https://doc.rust-lang.org/book/
- **libp2p Docs**: https://docs.libp2p.io
- **WASM Spec**: https://webassembly.github.io/spec/
- **Tokio Guide**: https://tokio.rs/tokio/tutorial

### Tools
- **cargo-deb**: https://github.com/kornelski/cargo-deb
- **wasm3**: https://github.com/wasm3/wasm3
- **wasmtime**: https://wasmtime.dev

### Community
- **Phase Discussions**: (internal/private for now)
- **Issues**: Track in GitHub Issues

---

**This file is your go-to reference for daily development. Update as patterns emerge.**
