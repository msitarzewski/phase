// SPDX-License-Identifier: Apache-2.0

//! Thin re-export shim. As of phase-core M2, the libp2p / Kademlia / mDNS /
//! Noise+QUIC machinery and the JobOffer / JobResponse wire types live in
//! the `phase-net` crate. The execution-side `ExecutionHandler` stays here
//! for now; M7 will move it into `crates/plasm/` where it joins the
//! `WasmtimeWorker` impl of `phase-protocol::Worker`.

pub mod execution;

// Re-export the wire-level / discovery types from phase-net under the same
// `crate::network::` paths the November 2025 daemon used. This keeps daemon
// binaries, tests, and CLI helpers compiling while the deeper "lift Plasm
// out of daemon" refactor lands in M7.
pub use execution::ExecutionHandler;
pub use phase_net::{
    Discovery, DiscoveryConfig, JobOffer, JobRequest, JobRequirements, JobResponse, JobResult,
    PeerCapabilities, PeerInfo, RejectionReason,
};

/// Compatibility re-export of `phase_net::protocol` under the legacy
/// `plasm::network::protocol` path that `daemon/tests/boundary_libp2p_job.rs`
/// (and any external `plasm::` consumer) still references.
pub mod protocol {
    pub use phase_net::protocol::*;
}
