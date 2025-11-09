//! # Plasm - Phase Local WASM Execution Daemon Library
//!
//! This library provides the core functionality for the Phase distributed compute network.
//! It includes WASM execution, peer discovery, job protocols, and cryptographic signing.
//!
//! ## Modules
//!
//! - `config`: Daemon configuration and execution limits
//! - `wasm`: WASM runtime, manifests, and receipts
//! - `network`: Peer discovery, protocols, and job execution
//!
//! ## Example
//!
//! ```no_run
//! use plasm::wasm::runtime::{WasmRuntime, Wasm3Runtime};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let runtime = Wasm3Runtime::new();
//! let wasm_bytes = std::fs::read("hello.wasm")?;
//! let result = runtime.execute(&wasm_bytes, &["arg1"]).await?;
//! println!("Output: {}", result.stdout);
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod wasm;
pub mod network;

// Re-export commonly used types
pub use config::{Config, ExecutionLimits};
pub use wasm::{
    runtime::{WasmRuntime, Wasm3Runtime, ExecutionResult},
    manifest::JobManifest,
    receipt::Receipt,
};
pub use network::{
    Discovery,
    DiscoveryConfig,
    PeerInfo,
    PeerCapabilities,
    ExecutionHandler,
    protocol::{JobOffer, JobResponse, JobRequirements, RejectionReason, JobRequest, JobResult},
};
