// SPDX-License-Identifier: Apache-2.0

//! # Plasm — Reference WASM Phase node
//!
//! Plasm is the reference [`phase_protocol::Worker`] implementation. It runs
//! `JobSpec::Wasm` jobs through Wasmtime and emits signed
//! `SignedReceipt<JobResult>` envelopes. As of phase-core M7 it lives at
//! `crates/plasm/` and is just one Phase node implementation among many —
//! equal-citizen to LUCID and any future implementation.
//!
//! ## Modules
//!
//! - [`worker`] — `WasmtimeWorker`, the `phase_protocol::Worker` impl.
//! - [`config`] — daemon configuration and execution limits.
//! - [`wasm`] — wasmtime runtime, legacy job manifests, and legacy receipts.
//! - [`network`] — re-exports of `phase-net` types under their historic
//!   `plasm::network::*` paths plus the `ExecutionHandler` bridge that
//!   `plasmd execute-job` and the libp2p job tests still use.
//! - [`provider`] — legacy plasm-specific `BootManifest` + signing
//!   (PHP-SDK-compat). NOT part of the Phase substrate.
//!
//! ## Example: drive the `Worker` trait directly
//!
//! ```no_run
//! use phase_identity::NodeIdentity;
//! use plasm::worker::WasmtimeWorker;
//!
//! let identity = NodeIdentity::generate();
//! let worker = WasmtimeWorker::new(identity);
//! // `worker` impls `phase_protocol::Worker`.
//! ```

// SEC-11 (L7): forbid `unsafe` in plasm's own code. This is per-crate — it
// does not affect wasmtime/libp2p, which use `unsafe` internally; it only
// prevents `unsafe` from regressing into plasm itself.
#![deny(unsafe_code)]

pub mod config;
pub mod network;
pub mod provider;
pub mod wasm;
pub mod worker;

// Re-export commonly used types — kept identical to the November 2025 daemon
// surface so downstream consumers (and the boundary tests) don't break.
pub use config::{Config, ExecutionLimits};
pub use network::{
    protocol::{JobOffer, JobRequest, JobRequirements, JobResponse, JobResult, RejectionReason},
    Discovery, DiscoveryConfig, ExecutionHandler, PeerCapabilities, PeerInfo,
};
pub use provider::{
    manifest::{ArtifactInfo, BootManifest, ManifestBuilder, ProviderInfo, Signature},
    ProviderConfig, ProviderServer,
};
pub use wasm::{
    manifest::JobManifest,
    receipt::Receipt,
    runtime::{ExecutionResult, Wasm3Runtime, WasmRuntime},
};
pub use worker::WasmtimeWorker;
