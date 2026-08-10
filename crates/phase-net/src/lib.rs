// SPDX-License-Identifier: Apache-2.0

#![deny(unsafe_code)]

//! libp2p, Kademlia DHT, mDNS, Noise+QUIC transport. Decentralized peer
//! discovery and capability advertisement for the Phase substrate.
//!
//! This crate carries the network code that lived inside
//! `daemon/src/network/` prior to phase-core M2. Job execution itself stays
//! out of phase-net — `Discovery` is purely about peer discovery, capability
//! gossip, manifest publication, and the JobOffer/JobResponse wire surface.
//! The execution-side `Worker` trait lives in `phase-protocol`; the wasmtime
//! implementation lives in `crates/plasm/` (after M7).

pub mod discovery;
pub mod peer;
pub mod protocol;

pub use discovery::{
    BlobStream, BlobStreamHandler, Discovery, DiscoveryConfig, JobRelayHandler, JobRelayLiveStream,
    JobRelayStreamHandler, NatReachability, ReachabilityConfig, ReachabilityConnection,
    ReachabilityPath, ReachabilityRole, ReachabilitySnapshot, RelayServerLimits, RendezvousPeer,
    RendezvousServerLimits,
};
pub use peer::{BandwidthBucket, LatencyBucket, PeerCapabilities, PeerInfo};
pub use protocol::{
    BlobStreamError, BlobStreamFrame, BlobStreamFrameKind, BlobStreamRequest, BlobStreamValidator,
    JobOffer, JobRelayRequest, JobRelayResponse, JobRelayStreamControl, JobRelayStreamControlKind,
    JobRelayStreamError, JobRelayStreamFrame, JobRelayStreamFrameKind, JobRelayStreamOpen,
    JobRelayStreamValidator, JobRequest, JobRequirements, JobResponse, JobResult, RejectionReason,
    BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS, BLOB_STREAM_MAX_CHUNK_BYTES,
    BLOB_STREAM_MAX_DEADLINE_AHEAD_MS, BLOB_STREAM_MAX_IDLE_TIMEOUT_MS,
    BLOB_STREAM_MAX_METADATA_BYTES, BLOB_STREAM_MAX_REASON_BYTES, BLOB_STREAM_MIN_IDLE_TIMEOUT_MS,
    BLOB_STREAM_PROTOCOL, BLOB_STREAM_SCHEMA_VERSION, JOB_RELAY_STREAM_MAX_EVENT_BYTES,
    JOB_RELAY_STREAM_MAX_OPEN_BYTES, JOB_RELAY_STREAM_MAX_REASON_BYTES,
    JOB_RELAY_STREAM_MAX_RECEIPT_BYTES, JOB_RELAY_STREAM_PROTOCOL, JOB_RELAY_STREAM_SCHEMA_VERSION,
};

// Re-export libp2p's `PeerId` so downstream Phase crates (lucidd, future
// workers) can speak in canonical peer identifiers without each one taking
// a direct `libp2p` dependency. This is the same pattern phase-identity
// uses for `SigningKey`/`VerifyingKey` from `ed25519-dalek`.
pub use libp2p::{Multiaddr, PeerId};

/// Re-export of `libp2p::identity` so downstream Phase crates (the LUCID
/// model registry, future workers) can derive `PeerId` from an Ed25519
/// public key — the same derivation libp2p itself uses internally — without
/// pulling `libp2p` in as a direct dependency. This keeps lucidd's
/// Cargo.toml free of a heavyweight transport stack it doesn't otherwise
/// need.
pub use libp2p::identity as libp2p_identity;
