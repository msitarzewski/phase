// SPDX-License-Identifier: Apache-2.0

//! Peer capabilities and peer info advertised over the DHT.
//!
//! Prior to phase-core M2, `PeerCapabilities` was WASM-specific: it carried a
//! free-form `wasm_runtime: String` like `"wasmtime-27"`. That worked for
//! Plasm but couldn't describe a LUCID inference node, a future image-gen
//! node, or any other workload Phase will host.
//!
//! The new shape is workload-agnostic: a node advertises the
//! [`JobSpecKind`]s it can serve, plus coarse hardware and live-load buckets.
//! Latency / bandwidth / concurrency are intentionally **coarse buckets**, not
//! precise numbers — this is what the gossip-not-telemetry design discussion
//! settled on (see MISSION.md). Sharing exact mbps/ms over a public mesh is a
//! privacy footprint; sharing "high / mid / low" is enough for routing.
//!
//! Measurement is a follow-up task. The new fields populate to `None` by
//! default; an active probe job (or passive observation of completed jobs)
//! will fill them in later.

use libp2p::PeerId;
use phase_protocol::JobSpecKind;
use serde::{Deserialize, Serialize};

/// Information about a discovered peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Peer ID (libp2p multihash, string-encoded).
    pub peer_id: String,

    /// Multiaddrs (network addresses).
    pub addresses: Vec<String>,

    /// Advertised capabilities.
    pub capabilities: PeerCapabilities,
}

/// What a peer claims it can do.
///
/// All "measured" fields are `Option<_>` and default to `None` — they are
/// populated by observation, not by self-attestation, so a fresh node has
/// nothing to put there.
///
/// Coarse buckets only. The protocol surface intentionally does **not**
/// carry precise mbps / ms numbers; see the module docstring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCapabilities {
    /// CPU architecture (e.g. `"x86_64"`, `"aarch64"`). The string matches
    /// `std::env::consts::ARCH` on the advertising node.
    pub arch: String,

    /// Logical CPU count.
    pub cpu_count: u32,

    /// Memory available, in MiB.
    pub memory_mb: u64,

    /// Job kinds this peer is willing to serve. A scheduler matches the
    /// requested [`JobSpecKind`] (from `JobSpec::kind()`) against this list
    /// before paying the cost of dispatching a full manifest.
    pub supported_kinds: Vec<JobSpecKind>,

    /// Measured round-trip latency bucket. `None` until observation fills it
    /// in. See [`LatencyBucket`].
    #[serde(default)]
    pub measured_latency_bucket: Option<LatencyBucket>,

    /// Measured bandwidth bucket. `None` until observation fills it in.
    /// See [`BandwidthBucket`].
    #[serde(default)]
    pub measured_bandwidth_bucket: Option<BandwidthBucket>,

    /// Number of jobs currently in flight on this peer. `None` if the peer
    /// does not gossip live load (e.g. for privacy reasons).
    #[serde(default)]
    pub current_concurrency: Option<u32>,

    /// Unix timestamp (seconds) of the last measurement that produced the
    /// fields above. `None` if no measurement has happened yet.
    #[serde(default)]
    pub last_measured_at: Option<u64>,
}

/// Coarse round-trip-time bucket. Workers gossip the bucket they observed
/// during the last completed job, never the exact millisecond figure.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyBucket {
    /// Sub-100ms round trip — close-by peer, good for chatty workloads.
    Good,
    /// 100ms – 500ms round trip — workable for batch and most chat jobs.
    Fair,
    /// >500ms round trip — only acceptable for fire-and-forget batch work.
    Poor,
    /// Unable to measure (transport refused, peer dropped before completion,
    /// or the measurement is too stale to trust).
    Unknown,
}

/// Coarse outbound-bandwidth bucket. Same rationale as [`LatencyBucket`]:
/// share a routing hint, not a precise throughput number.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandwidthBucket {
    /// Plenty of headroom (rough order: >100 Mbit/s sustainable).
    HighBw,
    /// Adequate for most jobs (rough order: 10–100 Mbit/s).
    MidBw,
    /// Constrained (rough order: <10 Mbit/s) — schedulers should avoid
    /// pushing large model weights or big artifact bundles through this peer.
    LowBw,
    /// Unable to measure.
    Unknown,
}

impl Default for PeerCapabilities {
    fn default() -> Self {
        Self {
            arch: std::env::consts::ARCH.to_string(),
            cpu_count: num_cpus::get() as u32,
            // TODO(M-future): detect actual RAM. The November 2025 MVP
            // hard-coded 4096 MiB; preserved here to avoid behaviour drift
            // during the extraction.
            memory_mb: 4096,
            // Default to "the workloads Plasm serves today". Other node
            // implementations (LUCID, image-gen, etc.) overwrite this in
            // their own bootstrap; phase-net does not assume a runtime.
            supported_kinds: vec![JobSpecKind::Wasm],
            measured_latency_bucket: None,
            measured_bandwidth_bucket: None,
            current_concurrency: None,
            last_measured_at: None,
        }
    }
}

impl PeerInfo {
    pub fn new(peer_id: PeerId, capabilities: PeerCapabilities) -> Self {
        Self {
            peer_id: peer_id.to_string(),
            addresses: Vec::new(),
            capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capabilities_are_sensible() {
        let caps = PeerCapabilities::default();
        assert!(!caps.arch.is_empty());
        assert!(caps.cpu_count > 0);
        assert!(caps.memory_mb > 0);
        assert!(caps.supported_kinds.contains(&JobSpecKind::Wasm));
        // Live-load fields default to None — a fresh node has nothing measured.
        assert!(caps.measured_latency_bucket.is_none());
        assert!(caps.measured_bandwidth_bucket.is_none());
        assert!(caps.current_concurrency.is_none());
        assert!(caps.last_measured_at.is_none());
    }

    #[test]
    fn capabilities_round_trip_through_serde() {
        let caps = PeerCapabilities {
            arch: "x86_64".into(),
            cpu_count: 4,
            memory_mb: 4096,
            supported_kinds: vec![JobSpecKind::Wasm, JobSpecKind::Inference],
            measured_latency_bucket: Some(LatencyBucket::Good),
            measured_bandwidth_bucket: Some(BandwidthBucket::MidBw),
            current_concurrency: Some(2),
            last_measured_at: Some(1_700_000_000),
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: PeerCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back.arch, caps.arch);
        assert_eq!(back.cpu_count, caps.cpu_count);
        assert_eq!(back.supported_kinds.len(), 2);
        assert_eq!(back.measured_latency_bucket, Some(LatencyBucket::Good));
        assert_eq!(back.current_concurrency, Some(2));
    }
}
