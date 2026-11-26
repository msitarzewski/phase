// SPDX-License-Identifier: Apache-2.0

//! Peer discovery and capability gossip over libp2p.
//!
//! `Discovery` owns the libp2p swarm — Kademlia DHT, mDNS, and a
//! JSON-encoded `request_response` protocol for the JobOffer / JobResponse
//! exchange. It runs its event loop on an internal Tokio task and exposes a
//! command-channel API to the rest of the daemon.
//!
//! Why a background task? Prior to phase-core M2 the swarm was driven by
//! whoever called `Discovery::run().await` — fine when that caller was
//! `plasmd`'s main loop, but it made it impossible to expose a synchronous
//! `send_job_offer()` API because the request and the response are emitted
//! by the *same* swarm. A driver task lets the public API send a command and
//! receive the response without the caller having to interleave it with
//! swarm polling.

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::SigningKey;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt};
use libp2p::{
    autonat, connection_limits, dcutr, identify,
    identity::Keypair,
    kad::{
        store::MemoryStore, Behaviour as KademliaBehaviour, Event as KademliaEvent, GetProvidersOk,
        GetRecordOk, Mode as KademliaMode, QueryId, QueryResult,
    },
    mdns,
    multiaddr::Protocol,
    relay, rendezvous,
    request_response::{self, cbor, json, OutboundRequestId, ProtocolSupport, ResponseChannel},
    swarm::{
        behaviour::toggle::Toggle,
        dial_opts::{DialOpts, PeerCondition},
        ConnectionId, NetworkBehaviour, SwarmEvent,
    },
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
};
use phase_identity::NodeIdentity;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info, warn};

use crate::peer::PeerCapabilities;
use crate::protocol::{
    BlobStreamFrame, BlobStreamFrameKind, BlobStreamRequest, BlobStreamValidator, JobOffer,
    JobRelayRequest, JobRelayResponse, JobRelayStreamControl, JobRelayStreamControlKind,
    JobRelayStreamFrame, JobRelayStreamFrameKind, JobRelayStreamOpen, JobRelayStreamValidator,
    JobResponse, RejectionReason, ShardStageRequest, ShardStageResponse, ShardStageResponseKind,
    BLOB_STREAM_MAX_CHUNK_BYTES, BLOB_STREAM_MAX_METADATA_BYTES, BLOB_STREAM_PROTOCOL,
    JOB_RELAY_STREAM_DEFAULT_IDLE_TIMEOUT_MS, JOB_RELAY_STREAM_MAX_EVENT_BYTES,
    JOB_RELAY_STREAM_MAX_OPEN_BYTES, JOB_RELAY_STREAM_PROTOCOL, JOB_RELAY_STREAM_SCHEMA_VERSION,
    SHARD_STAGE_MAX_PAYLOAD_BYTES, SHARD_STAGE_MAX_PLAN_BYTES, SHARD_STAGE_MAX_RECEIPT_BYTES,
    SHARD_STAGE_PROTOCOL, SHARD_STAGE_SCHEMA_VERSION,
};

/// Wire protocol identifier for the JobOffer request/response exchange.
const JOB_OFFER_PROTOCOL: &str = "/phase/job-offer/1.0.0";

/// LUCID M5 peer-relay protocol identifier. Carries a CBOR-encoded
/// `JobRelayRequest` and gets back a `JobRelayResponse`. The inner payload
/// is bincode owned by lucidd; phase-net stays inference-agnostic.
const JOB_RELAY_PROTOCOL: &str = "/phase/job-relay/1.0.0";

const MAX_INBOUND_STREAM_TASKS: usize = 64;
const MAX_INBOUND_STREAMS_PER_PEER: usize = 4;
const MAX_BLOB_BYTES_PER_PEER_PER_SECOND: usize = 16 * 1024 * 1024;
const BLOB_BANDWIDTH_WINDOW: Duration = Duration::from_secs(1);

struct BlobBandwidthWindow {
    started: tokio::time::Instant,
    bytes: usize,
}

/// Generic Kademlia allocation bounds. Workload layers impose tighter schema
/// limits after this transport gate, but peer-supplied records must be bounded
/// before the driver accumulates them in memory.
pub const MAX_KAD_RECORD_KEY_BYTES: usize = 512;
pub const MAX_KAD_RECORD_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_KAD_RECORD_VALUES_PER_QUERY: usize = 256;
pub const MAX_KAD_RECORD_BYTES_PER_QUERY: usize = 4 * 1024 * 1024;
pub const MAX_PROVIDER_PEERS_PER_QUERY: usize = 64;
const MAX_PENDING_PROVIDER_QUERIES: usize = 32;
const MAX_PENDING_RELAY_LISTENS: usize = 32;
const DISCOVERY_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Callback the daemon registers to serve inbound JobRelay requests.
///
/// phase-net knows nothing about `SignedManifest<JobSpec>` or `JobEvent` —
/// the bytes flow through unmodified. lucidd installs a handler that
/// decodes the bincode payload, runs its local worker, and re-encodes the
/// resulting stream as a batch.
///
/// SEC-06: the handler receives the **delivering peer's libp2p `PeerId`** as
/// its first argument. lucidd's authz gate (SEC-01) uses it as an alternative
/// acceptance path — a signer whose Ed25519 key derives to this PeerId is
/// implicitly authorized even if absent from the operator allowlist. phase-net
/// supplies the identity; the policy decision stays in lucidd.
pub type JobRelayHandler = std::sync::Arc<
    dyn Fn(
            PeerId,
            Vec<u8>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = JobRelayResponse> + Send>>
        + Send
        + Sync
        + 'static,
>;

/// Live-relay server callback. The handler receives a validated open envelope,
/// a bounded stream of requester control messages, and a bounded sender for
/// server response frames. It must emit the v2 decision/event/receipt lifecycle
/// in sequence; phase-net validates every frame again before it reaches the
/// wire.
pub type JobRelayStreamHandler = Arc<
    dyn Fn(
            PeerId,
            JobRelayStreamOpen,
            mpsc::Receiver<JobRelayStreamControl>,
            mpsc::Sender<JobRelayStreamFrame>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
>;

/// Workload-neutral content callback. The caller owns lookup, authorization,
/// and storage; phase-net supplies a validated request and a bounded sender
/// whose failure signals that the requester closed the stream.
pub type BlobStreamHandler = Arc<
    dyn Fn(
            PeerId,
            BlobStreamRequest,
            mpsc::Sender<BlobStreamFrame>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
>;

/// Workload-neutral pipeline-stage execution callback. `phase-net` validates
/// frame bounds and delivers the authenticated libp2p `PeerId` alongside the
/// opaque `ShardStageRequest`; the workload layer enforces authorization,
/// partition-plan verification, execution, and stage-receipt signing.
pub type ShardStageHandler = Arc<
    dyn Fn(PeerId, ShardStageRequest) -> Pin<Box<dyn Future<Output = ShardStageResponse> + Send>>
        + Send
        + Sync
        + 'static,
>;

struct InboundStreamAdmission {
    global: Arc<Semaphore>,
    per_peer: StdMutex<HashMap<PeerId, usize>>,
    blob_bandwidth: StdMutex<HashMap<PeerId, BlobBandwidthWindow>>,
    per_peer_limit: usize,
}

impl InboundStreamAdmission {
    fn new(global_limit: usize, per_peer_limit: usize) -> Arc<Self> {
        assert!(global_limit > 0 && per_peer_limit > 0);
        Arc::new(Self {
            global: Arc::new(Semaphore::new(global_limit)),
            per_peer: StdMutex::new(HashMap::new()),
            blob_bandwidth: StdMutex::new(HashMap::new()),
            per_peer_limit,
        })
    }

    fn try_acquire(self: &Arc<Self>, peer: PeerId) -> Option<InboundStreamPermit> {
        let global_permit = self.global.clone().try_acquire_owned().ok()?;
        let mut counts = self.per_peer.lock().ok()?;
        let count = counts.entry(peer).or_default();
        if *count >= self.per_peer_limit {
            return None;
        }
        *count += 1;
        drop(counts);
        Some(InboundStreamPermit {
            admission: self.clone(),
            peer,
            _global_permit: global_permit,
        })
    }
}

struct InboundStreamPermit {
    admission: Arc<InboundStreamAdmission>,
    peer: PeerId,
    _global_permit: OwnedSemaphorePermit,
}

impl Drop for InboundStreamPermit {
    fn drop(&mut self) {
        let Ok(mut counts) = self.admission.per_peer.lock() else {
            return;
        };
        let mut remove_bandwidth = false;
        if let Some(count) = counts.get_mut(&self.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.peer);
                remove_bandwidth = true;
            }
        }
        drop(counts);
        if remove_bandwidth {
            if let Ok(mut bandwidth) = self.admission.blob_bandwidth.lock() {
                bandwidth.remove(&self.peer);
            }
        }
    }
}

impl InboundStreamPermit {
    /// Fixed-window aggregate egress cap shared by every blob stream from one
    /// peer. Admission already bounds stream count; this prevents those
    /// admitted streams from multiplying unbounded content throughput.
    async fn wait_for_blob_budget(&self, bytes: usize) {
        debug_assert!(bytes <= BLOB_STREAM_MAX_CHUNK_BYTES);
        loop {
            let wait = {
                let now = tokio::time::Instant::now();
                let Ok(mut windows) = self.admission.blob_bandwidth.lock() else {
                    // Poisoning fails closed with one full-window delay rather
                    // than allowing unmetered egress.
                    tokio::time::sleep(BLOB_BANDWIDTH_WINDOW).await;
                    continue;
                };
                let window = windows.entry(self.peer).or_insert(BlobBandwidthWindow {
                    started: now,
                    bytes: 0,
                });
                if now.duration_since(window.started) >= BLOB_BANDWIDTH_WINDOW {
                    window.started = now;
                    window.bytes = 0;
                }
                if bytes <= MAX_BLOB_BYTES_PER_PEER_PER_SECOND.saturating_sub(window.bytes) {
                    window.bytes += bytes;
                    None
                } else {
                    Some(BLOB_BANDWIDTH_WINDOW.saturating_sub(now.duration_since(window.started)))
                }
            };
            match wait {
                None => return,
                Some(delay) => tokio::time::sleep(delay).await,
            }
        }
    }
}

/// Requester-side handle for one real libp2p v2 substream.
///
/// The frame channel is deliberately bounded. If the router or HTTP client is
/// slow, this handle stops reading the libp2p stream and transport backpressure
/// reaches the serving peer instead of growing an unbounded event buffer.
pub struct JobRelayLiveStream {
    job_id: [u8; 32],
    frames: mpsc::Receiver<std::result::Result<JobRelayStreamFrame, String>>,
    controls: mpsc::Sender<JobRelayStreamControl>,
}

impl JobRelayLiveStream {
    /// Receive the next validated frame. `None` means the v2 transport task
    /// has shut down; callers must still require an explicit terminal receipt.
    pub async fn next_frame(&mut self) -> Option<Result<JobRelayStreamFrame>> {
        self.frames
            .recv()
            .await
            .map(|result| result.map_err(anyhow::Error::msg))
    }

    /// Idempotently request cancellation of the remote job.
    pub async fn cancel(&self, reason: impl Into<String>) -> Result<()> {
        self.send_control(JobRelayStreamControlKind::Cancel {
            reason: reason.into(),
        })
        .await
    }

    /// Acknowledge that the terminal receipt was received and verified.
    pub async fn acknowledge_receipt(&self) -> Result<()> {
        self.send_control(JobRelayStreamControlKind::ReceiptAck)
            .await
    }

    async fn send_control(&self, kind: JobRelayStreamControlKind) -> Result<()> {
        let control = JobRelayStreamControl {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: self.job_id,
            kind,
        };
        control
            .validate_for(self.job_id)
            .map_err(anyhow::Error::new)?;
        self.controls
            .send(control)
            .await
            .map_err(|_| anyhow!("live relay stream already closed"))
    }
}

/// Requester-side handle for one `/phase/blob/1.0.0` substream.
///
/// Frames are read directly from the libp2p stream, so no whole-object buffer
/// exists in phase-net. Dropping this value closes the stream; [`Self::cancel`]
/// provides an explicit async close when deterministic cancellation is useful.
pub struct BlobStream {
    content_id: [u8; 32],
    stream: libp2p::swarm::Stream,
    validator: BlobStreamValidator,
    idle_timeout: Duration,
    deadline: tokio::time::Instant,
    closed: bool,
}

impl BlobStream {
    pub fn content_id(&self) -> &[u8; 32] {
        &self.content_id
    }

    /// Read and validate the next frame. Completion is represented only by an
    /// explicit `Eof` or `Rejected` frame; an early stream close is an error.
    pub async fn next_frame(&mut self) -> Result<BlobStreamFrame> {
        if self.closed || self.validator.is_complete() {
            return Err(anyhow!("blob stream already completed"));
        }
        let now = tokio::time::Instant::now();
        if now >= self.deadline {
            let _ = self.stream.close().await;
            self.closed = true;
            return Err(anyhow!("blob stream deadline reached"));
        }
        let wait = self.idle_timeout.min(self.deadline - now);
        let frame = match tokio::time::timeout(
            wait,
            read_postcard_frame(&mut self.stream, BLOB_STREAM_WIRE_MAX_BYTES),
        )
        .await
        {
            Ok(Ok(frame)) => frame,
            Ok(Err(error)) => {
                let protocol_error = self.validator.validate_eof().err().map(anyhow::Error::new);
                let _ = self.stream.close().await;
                self.closed = true;
                return Err(
                    protocol_error.unwrap_or_else(|| error.context("read blob stream frame"))
                );
            }
            Err(_) if tokio::time::Instant::now() >= self.deadline => {
                let _ = self.stream.close().await;
                self.closed = true;
                return Err(anyhow!("blob stream deadline reached"));
            }
            Err(_) => {
                let _ = self.stream.close().await;
                self.closed = true;
                return Err(anyhow!("blob stream idle timeout reached"));
            }
        };
        if let Err(error) = self.validator.validate(&frame) {
            let _ = self.stream.close().await;
            self.closed = true;
            return Err(anyhow::Error::new(error));
        }
        if self.validator.is_complete() {
            self.closed = true;
            let _ = self.stream.close().await;
        }
        Ok(frame)
    }

    /// Cancel the request by closing its substream. No workload-specific
    /// cancellation message is introduced into this byte-transfer protocol.
    pub async fn cancel(mut self) -> Result<()> {
        self.stream.close().await.context("close blob stream")
    }
}

/// SEC-06: inbound relay request-size cap. A `SignedManifest<JobSpec>` for an
/// inference job is a few KB (a chat history plus a signature); 256 KiB is a
/// generous ceiling that rejects buffer-exhaustion floods at the libp2p codec
/// before the inner JSON is ever parsed.
const RELAY_MAX_REQUEST_BYTES: usize = 256 * 1024;

/// SEC-06: relay response-size cap. A batch-shaped `Vec<JobEvent>` for a
/// `max_tokens`-capped generation plus the signed receipt is bounded; 8 MiB
/// covers a long completion with headroom while capping how much a malicious
/// *serving* peer can make a requester buffer.
const RELAY_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// SEC-06: JobOffer is a tiny fixed-shape struct; 64 KiB each way is ample.
const OFFER_MAX_BYTES: usize = 64 * 1024;

const IDENTIFY_PROTOCOL: &str = "/phase/reachability/1.0.0";
const MAX_TRACKED_REACHABILITY_ADDRESSES: usize = 64;
const MAX_RENDEZVOUS_DISCOVER_RESULTS: u64 = 256;

const MAX_RELAY_RESERVATIONS: usize = 1_024;
const MAX_RELAY_RESERVATIONS_PER_PEER: usize = 8;
const MAX_RELAY_CIRCUITS: usize = 2_048;
const MAX_RELAY_CIRCUITS_PER_PEER: usize = 16;
const MAX_RELAY_CIRCUIT_DURATION: Duration = Duration::from_secs(60 * 60);
const MAX_RELAY_CIRCUIT_BYTES: u64 = 1024 * 1024 * 1024;

/// High-level reachability role. The default ordinary peer role may consume
/// relay, AutoNAT, and rendezvous services but is never instantiated as a
/// server for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachabilityRole {
    Peer,
    Infrastructure,
}

/// Bounded circuit-relay server allocation. This is only accepted for an
/// [`ReachabilityRole::Infrastructure`] node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayServerLimits {
    pub max_reservations: usize,
    pub max_reservations_per_peer: usize,
    pub reservation_duration: Duration,
    pub max_circuits: usize,
    pub max_circuits_per_peer: usize,
    pub max_circuit_duration: Duration,
    pub max_circuit_bytes: u64,
}

impl Default for RelayServerLimits {
    fn default() -> Self {
        Self {
            max_reservations: 128,
            max_reservations_per_peer: 2,
            reservation_duration: Duration::from_secs(60 * 60),
            max_circuits: 256,
            max_circuits_per_peer: 4,
            max_circuit_duration: Duration::from_secs(2 * 60),
            max_circuit_bytes: 64 * 1024 * 1024,
        }
    }
}

impl RelayServerLimits {
    fn validate(&self) -> Result<()> {
        if self.max_reservations == 0 || self.max_reservations > MAX_RELAY_RESERVATIONS {
            return Err(anyhow!(
                "relay max_reservations must be within 1..={MAX_RELAY_RESERVATIONS}"
            ));
        }
        if self.max_reservations_per_peer == 0
            || self.max_reservations_per_peer > MAX_RELAY_RESERVATIONS_PER_PEER
            || self.max_reservations_per_peer > self.max_reservations
        {
            return Err(anyhow!(
                "relay max_reservations_per_peer must be bounded by the per-peer and total limits"
            ));
        }
        if self.reservation_duration.is_zero()
            || self.reservation_duration > MAX_RELAY_CIRCUIT_DURATION
        {
            return Err(anyhow!(
                "relay reservation duration is outside the safe range"
            ));
        }
        if self.max_circuits == 0 || self.max_circuits > MAX_RELAY_CIRCUITS {
            return Err(anyhow!(
                "relay max_circuits must be within 1..={MAX_RELAY_CIRCUITS}"
            ));
        }
        if self.max_circuits_per_peer == 0
            || self.max_circuits_per_peer > MAX_RELAY_CIRCUITS_PER_PEER
            || self.max_circuits_per_peer > self.max_circuits
        {
            return Err(anyhow!(
                "relay max_circuits_per_peer must be bounded by the per-peer and total limits"
            ));
        }
        if self.max_circuit_duration.is_zero()
            || self.max_circuit_duration > MAX_RELAY_CIRCUIT_DURATION
        {
            return Err(anyhow!("relay circuit duration is outside the safe range"));
        }
        if self.max_circuit_bytes == 0 || self.max_circuit_bytes > MAX_RELAY_CIRCUIT_BYTES {
            return Err(anyhow!(
                "relay max_circuit_bytes must be within 1..={MAX_RELAY_CIRCUIT_BYTES}"
            ));
        }
        Ok(())
    }

    fn to_libp2p_config(&self) -> relay::Config {
        relay::Config {
            max_reservations: self.max_reservations,
            max_reservations_per_peer: self.max_reservations_per_peer,
            reservation_duration: self.reservation_duration,
            max_circuits: self.max_circuits,
            max_circuits_per_peer: self.max_circuits_per_peer,
            max_circuit_duration: self.max_circuit_duration,
            max_circuit_bytes: self.max_circuit_bytes,
            ..relay::Config::default()
        }
    }
}

/// TTL bounds available in libp2p-rendezvous 0.17. That upstream server does
/// not expose a registration-count limit, so infrastructure operators should
/// additionally bound inbound connections outside this behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendezvousServerLimits {
    pub min_ttl_seconds: u64,
    pub max_ttl_seconds: u64,
}

impl Default for RendezvousServerLimits {
    fn default() -> Self {
        Self {
            min_ttl_seconds: rendezvous::MIN_TTL,
            max_ttl_seconds: rendezvous::DEFAULT_TTL,
        }
    }
}

impl RendezvousServerLimits {
    fn validate(&self) -> Result<()> {
        if self.min_ttl_seconds < rendezvous::MIN_TTL
            || self.max_ttl_seconds > rendezvous::MAX_TTL
            || self.min_ttl_seconds > self.max_ttl_seconds
        {
            return Err(anyhow!(
                "rendezvous TTL bounds must satisfy {} <= min <= max <= {} seconds",
                rendezvous::MIN_TTL,
                rendezvous::MAX_TTL
            ));
        }
        Ok(())
    }

    fn to_libp2p_config(&self) -> rendezvous::server::Config {
        rendezvous::server::Config::default()
            .with_min_ttl(self.min_ttl_seconds)
            .with_max_ttl(self.max_ttl_seconds)
    }
}

/// Explicit gates for every Reachability Plane role. Use
/// [`Discovery::new_with_reachability`] to override the safe ordinary-peer
/// defaults without changing legacy [`DiscoveryConfig`] construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityConfig {
    pub role: ReachabilityRole,
    /// Disable direct listeners, multicast discovery and hole-punch/probe
    /// listeners. Reserved relay circuits remain available over outbound
    /// control connections; this mode never exposes infrastructure services.
    pub outbound_only: bool,
    pub relay_client: bool,
    pub dcutr: bool,
    pub identify: bool,
    pub autonat_client: bool,
    pub autonat_server: bool,
    pub rendezvous_client: bool,
    pub relay_server: Option<RelayServerLimits>,
    pub rendezvous_server: Option<RendezvousServerLimits>,
}

impl Default for ReachabilityConfig {
    fn default() -> Self {
        Self {
            role: ReachabilityRole::Peer,
            outbound_only: false,
            relay_client: true,
            dcutr: true,
            identify: true,
            autonat_client: true,
            autonat_server: false,
            rendezvous_client: true,
            relay_server: None,
            rendezvous_server: None,
        }
    }
}

impl ReachabilityConfig {
    pub fn validate(&self) -> Result<()> {
        if self.outbound_only && self.role == ReachabilityRole::Infrastructure {
            return Err(anyhow!(
                "outbound-only peers cannot run infrastructure services"
            ));
        }
        if self.role == ReachabilityRole::Peer
            && (self.relay_server.is_some()
                || self.autonat_server
                || self.rendezvous_server.is_some())
        {
            return Err(anyhow!(
                "ordinary peers cannot enable reachability server roles"
            ));
        }
        if self.dcutr && (!self.relay_client || !self.identify) {
            return Err(anyhow!("DCUtR requires relay-client and identify support"));
        }
        if self.autonat_client && !self.identify {
            return Err(anyhow!("AutoNAT client requires identify support"));
        }
        if let Some(limits) = &self.relay_server {
            limits.validate()?;
        }
        if let Some(limits) = &self.rendezvous_server {
            limits.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachabilityPath {
    Unknown,
    Direct,
    Relayed,
    Dcutr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatReachability {
    Unknown,
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityConnection {
    pub peer_id: PeerId,
    pub path: ReachabilityPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilitySnapshot {
    pub role: ReachabilityRole,
    pub outbound_only: bool,
    pub active_path: ReachabilityPath,
    pub nat: NatReachability,
    pub listen_addresses: Vec<Multiaddr>,
    pub observed_addresses: Vec<Multiaddr>,
    pub external_addresses: Vec<Multiaddr>,
    pub connections: Vec<ReachabilityConnection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendezvousPeer {
    pub namespace: String,
    pub peer_id: PeerId,
    pub addresses: Vec<Multiaddr>,
    pub ttl_seconds: u64,
}

#[allow(
    unreachable_code,
    reason = "libp2p NetworkBehaviour derive emits unreachable arms for Infallible sub-behaviours on Rust 1.100+"
)]
mod combined_behaviour {
    use super::*;

    /// Combined network behaviour: Kademlia DHT + mDNS local discovery +
    /// JSON-coded request/response for JobOffer.
    #[derive(NetworkBehaviour)]
    pub(super) struct CombinedBehaviour {
        /// Hard swarm-level bounds apply before any application protocol is
        /// negotiated. This protects rendezvous/relay deployments as well as the
        /// LUCID-specific substreams below.
        pub(super) connection_limits: connection_limits::Behaviour,
        pub(super) kademlia: KademliaBehaviour<MemoryStore>,
        pub(super) mdns: Toggle<mdns::tokio::Behaviour>,
        pub(super) relay_client: Toggle<relay::client::Behaviour>,
        pub(super) relay_server: Toggle<relay::Behaviour>,
        pub(super) dcutr: Toggle<dcutr::Behaviour>,
        pub(super) identify: Toggle<identify::Behaviour>,
        pub(super) autonat_client: Toggle<autonat::v2::client::Behaviour>,
        pub(super) autonat_server: Toggle<autonat::v2::server::Behaviour>,
        pub(super) rendezvous_client: Toggle<rendezvous::client::Behaviour>,
        pub(super) rendezvous_server: Toggle<rendezvous::server::Behaviour>,
        pub(super) job_offer: json::Behaviour<JobOffer, JobResponse>,
        /// LUCID M5 peer-relay request/response. CBOR-encoded so the binary
        /// payload (bincode `SignedManifest<JobSpec>` / `Vec<JobEvent>`) doesn't
        /// suffer the 4-5× blow-up JSON's u8 arrays cause.
        pub(super) job_relay: cbor::Behaviour<JobRelayRequest, JobRelayResponse>,
        /// Distinct v2 bidirectional substreams. The behaviour only negotiates
        /// and opens streams; Phase's bounded frame codec and state machine live
        /// in this module and `protocol.rs`.
        pub(super) job_relay_stream: libp2p_stream::Behaviour,
    }
}

use combined_behaviour::{CombinedBehaviour, CombinedBehaviourEvent};

/// Discovery configuration.
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Listen address (e.g., `"/ip4/0.0.0.0/tcp/0"`).
    pub listen_addr: String,

    /// Bootstrap peers to connect to.
    pub bootstrap_peers: Vec<String>,

    /// Local peer capabilities.
    pub capabilities: PeerCapabilities,

    /// Persistent node identity. When `Some`, the libp2p peer-id and the
    /// node's receipt-signing key both derive from this keypair, so the
    /// peer-id is stable across restarts. When `None`, a fresh keypair is
    /// generated (matches the legacy ephemeral behaviour and is appropriate
    /// for tests that don't care about identity continuity).
    pub identity: Option<NodeIdentity>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_string(),
            bootstrap_peers: Vec::new(),
            capabilities: PeerCapabilities::default(),
            identity: None,
        }
    }
}

/// Commands the public `Discovery` handle sends to the background driver task.
enum Command {
    Listen {
        addr: String,
        reply: oneshot::Sender<Result<()>>,
    },
    ListenAddrs {
        reply: oneshot::Sender<Vec<Multiaddr>>,
    },
    ReachabilitySnapshot {
        reply: oneshot::Sender<ReachabilitySnapshot>,
    },
    AddExternalAddress {
        address: Multiaddr,
        reply: oneshot::Sender<Result<()>>,
    },
    RendezvousRegister {
        rendezvous_node: PeerId,
        namespace: rendezvous::Namespace,
        ttl_seconds: Option<u64>,
        reply: oneshot::Sender<Result<()>>,
    },
    RendezvousDiscover {
        rendezvous_node: PeerId,
        namespace: Option<rendezvous::Namespace>,
        limit: u64,
        reply: oneshot::Sender<Result<Vec<RendezvousPeer>>>,
    },
    Bootstrap {
        reply: oneshot::Sender<Result<()>>,
    },
    Dial {
        addr: String,
        reply: oneshot::Sender<Result<()>>,
    },
    AdvertiseCapabilities {
        reply: oneshot::Sender<Result<()>>,
    },
    DiscoverPeers {
        arch: String,
        kind_label: String,
        reply: oneshot::Sender<Result<()>>,
    },
    PublishKadRecord {
        key: Vec<u8>,
        value: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Look up records published under `key` on the Kademlia DHT.
    ///
    /// The driver issues a `get_record` query and accumulates every
    /// peer-supplied value until libp2p reports the query as complete
    /// (`step.last == true`). Duplicate raw payloads are de-duplicated by
    /// the driver — callers see at most one entry per distinct value.
    GetKadRecord {
        key: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
    },
    StartProvidingKey {
        key: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    GetProviderPeers {
        key: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<PeerId>>>,
    },
    SendJobOffer {
        peer: PeerId,
        offer: JobOffer,
        reply: oneshot::Sender<Result<JobResponse>>,
    },
    /// Evaluate a JobOffer against local capabilities. Same logic as the
    /// receive-side handler, exposed so tests can drive the rejection
    /// contract without round-tripping through libp2p.
    EvaluateOffer {
        offer: JobOffer,
        reply: oneshot::Sender<JobResponse>,
    },
    /// LUCID M5: ship a `JobRelayRequest` to `peer` over the relay
    /// protocol and await the `JobRelayResponse`. The serving peer's
    /// inbound handler is whoever registered via `SetJobRelayHandler`.
    SendJobRelay {
        peer: PeerId,
        request: JobRelayRequest,
        reply: oneshot::Sender<Result<JobRelayResponse>>,
    },
    /// LUCID M5: install (or replace) the callback that serves inbound
    /// `JobRelayRequest`s. The default — no handler — refuses every
    /// inbound relay request with a structured "no handler" reason so a
    /// daemon that never wired one in fails closed.
    SetJobRelayHandler {
        handler: Option<JobRelayHandler>,
    },
}

/// Peer discovery service. Owns no swarm directly — instead holds a handle
/// to the background driver task that does.
pub struct Discovery {
    local_peer_id: PeerId,
    signing_key: SigningKey,
    capabilities: PeerCapabilities,
    cmd_tx: mpsc::Sender<Command>,
    job_relay_stream_control: Arc<tokio::sync::Mutex<libp2p_stream::Control>>,
    job_relay_stream_handler: Arc<RwLock<Option<JobRelayStreamHandler>>>,
    blob_stream_handler: Arc<RwLock<Option<BlobStreamHandler>>>,
    shard_stage_handler: Arc<RwLock<Option<ShardStageHandler>>>,
    /// Background driver task. `run()` takes this to await shutdown; if the
    /// daemon never calls `run()`, the task is torn down when Discovery
    /// drops (the cmd_rx side sees its last Sender go and exits).
    driver: Option<tokio::task::JoinHandle<()>>,
}

impl Discovery {
    /// Create a new discovery service. Spawns the background swarm-driver task.
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
        Self::new_with_reachability(config, ReachabilityConfig::default())
    }

    /// Create a discovery service with explicit Reachability Plane roles.
    /// Ordinary callers should use [`Self::new`], whose defaults enable client
    /// traversal features without exposing relay, AutoNAT, or rendezvous
    /// servers.
    pub fn new_with_reachability(
        config: DiscoveryConfig,
        reachability: ReachabilityConfig,
    ) -> Result<Self> {
        reachability.validate()?;

        // Resolve node identity. If the caller provided a persistent one,
        // use it; otherwise fall back to a freshly generated ephemeral
        // identity (legacy behaviour). The libp2p peer-id and the receipt
        // signing key are both derived from this same 32-byte Ed25519
        // secret, so they always agree.
        let node_identity = config
            .identity
            .clone()
            .unwrap_or_else(NodeIdentity::generate);

        let signing_key = node_identity.signing_key().clone();
        let secret_bytes = signing_key.to_bytes();
        let mut secret_for_libp2p = secret_bytes;
        let keypair = Keypair::ed25519_from_bytes(&mut secret_for_libp2p)
            .context("Failed to derive libp2p keypair from node identity")?;
        let local_peer_id = PeerId::from(keypair.public());

        info!("Local peer ID: {}", local_peer_id);
        info!(
            "Node public key: {}",
            hex::encode(signing_key.verifying_key().to_bytes())
        );

        // Create Kademlia behaviour. libp2p-kad 0.48 defaults new nodes to
        // `Mode::Client`, which means they CAN issue queries but won't
        // SERVE them — `GetRecord` requests to a client-mode peer fail
        // with "protocol not supported". For our small-network use (mDNS
        // discovery on a LAN, no global DHT bootstrap), we always want to
        // be a server so other peers can resolve our advertised records.
        let store = MemoryStore::new(local_peer_id);
        let kad_behaviour = {
            let mut k = KademliaBehaviour::new(local_peer_id, store);
            k.set_mode(Some(KademliaMode::Server));
            k
        };

        // Build swarm with tokio executor.
        let mut swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_quic()
            .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)?
            .with_behaviour(|key, relay_client| {
                // Create mDNS behaviour for local network discovery.
                let mdns_behaviour = if reachability.outbound_only {
                    None
                } else {
                    Some(mdns::tokio::Behaviour::new(
                        mdns::Config::default(),
                        key.public().to_peer_id(),
                    )?)
                };

                // JSON-coded request/response for JobOffer. SEC-06: cap both
                // directions — a JobOffer is a small fixed-shape struct, so an
                // oversized frame is always abuse. The size maxima live on the
                // codec; `with_codec` installs the configured one.
                let offer_codec = json::codec::Codec::<JobOffer, JobResponse>::default()
                    .set_request_size_maximum(OFFER_MAX_BYTES as u64)
                    .set_response_size_maximum(OFFER_MAX_BYTES as u64);
                let job_offer = json::Behaviour::<JobOffer, JobResponse>::with_codec(
                    offer_codec,
                    [(
                        StreamProtocol::new(JOB_OFFER_PROTOCOL),
                        ProtocolSupport::Full,
                    )],
                    request_response::Config::default(),
                );

                // CBOR-coded request/response for the LUCID M5 job relay.
                // The default per-request timeout is generous enough for a
                // batch-shaped inference response (we set a wall-clock cap
                // on the requesting side anyway). SEC-06: cap request and
                // response sizes so an oversized frame is rejected at the
                // codec (`io.take(max)`) before it is ever buffered/parsed.
                let relay_codec =
                    cbor::codec::Codec::<JobRelayRequest, JobRelayResponse>::default()
                        .set_request_size_maximum(RELAY_MAX_REQUEST_BYTES as u64)
                        .set_response_size_maximum(RELAY_MAX_RESPONSE_BYTES as u64);
                let job_relay = cbor::Behaviour::<JobRelayRequest, JobRelayResponse>::with_codec(
                    relay_codec,
                    [(
                        StreamProtocol::new(JOB_RELAY_PROTOCOL),
                        ProtocolSupport::Full,
                    )],
                    request_response::Config::default()
                        .with_request_timeout(Duration::from_secs(5 * 60)),
                );
                let job_relay_stream = libp2p_stream::Behaviour::new();

                let relay_client = reachability.relay_client.then_some(relay_client).into();
                let relay_server = reachability
                    .relay_server
                    .as_ref()
                    .map(|limits| relay::Behaviour::new(local_peer_id, limits.to_libp2p_config()))
                    .into();
                let dcutr = (reachability.dcutr && !reachability.outbound_only)
                    .then(|| dcutr::Behaviour::new(local_peer_id))
                    .into();
                let identify = reachability
                    .identify
                    .then(|| {
                        identify::Behaviour::new(
                            identify::Config::new_with_signed_peer_record(
                                IDENTIFY_PROTOCOL.to_string(),
                                key,
                            )
                            .with_agent_version(format!("phase-net/{}", env!("CARGO_PKG_VERSION")))
                            .with_push_listen_addr_updates(true),
                        )
                    })
                    .into();
                let autonat_client = (reachability.autonat_client && !reachability.outbound_only)
                    .then(autonat::v2::client::Behaviour::default)
                    .into();
                let autonat_server = reachability
                    .autonat_server
                    .then(autonat::v2::server::Behaviour::default)
                    .into();
                let rendezvous_client = reachability
                    .rendezvous_client
                    .then(|| rendezvous::client::Behaviour::new(key.clone()))
                    .into();
                let rendezvous_server = reachability
                    .rendezvous_server
                    .as_ref()
                    .map(|limits| rendezvous::server::Behaviour::new(limits.to_libp2p_config()))
                    .into();

                Ok(CombinedBehaviour {
                    connection_limits: connection_limits::Behaviour::new(
                        connection_limits::ConnectionLimits::default()
                            .with_max_pending_incoming(Some(32))
                            .with_max_pending_outgoing(Some(32))
                            .with_max_established_incoming(Some(128))
                            .with_max_established_outgoing(Some(128))
                            .with_max_established_per_peer(Some(4))
                            .with_max_established(Some(256)),
                    ),
                    kademlia: kad_behaviour,
                    mdns: mdns_behaviour.into(),
                    relay_client,
                    relay_server,
                    dcutr,
                    identify,
                    autonat_client,
                    autonat_server,
                    rendezvous_client,
                    rendezvous_server,
                    job_offer,
                    job_relay,
                    job_relay_stream,
                })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        // Register the v2 stream protocol exactly once on the same swarm.
        // The handler itself is replaceable at runtime through the shared
        // slot; accepting substreams never blocks swarm polling.
        let mut job_relay_stream_control = swarm.behaviour().job_relay_stream.new_control();
        let incoming_job_relay_streams = job_relay_stream_control
            .accept(StreamProtocol::new(JOB_RELAY_STREAM_PROTOCOL))
            .context("register live relay stream protocol")?;
        let incoming_blob_streams = job_relay_stream_control
            .accept(StreamProtocol::new(BLOB_STREAM_PROTOCOL))
            .context("register blob stream protocol")?;
        let incoming_shard_stage_streams = job_relay_stream_control
            .accept(StreamProtocol::new(SHARD_STAGE_PROTOCOL))
            .context("register shard stage protocol")?;
        let job_relay_stream_handler: Arc<RwLock<Option<JobRelayStreamHandler>>> =
            Arc::new(RwLock::new(None));
        let blob_stream_handler: Arc<RwLock<Option<BlobStreamHandler>>> =
            Arc::new(RwLock::new(None));
        let shard_stage_handler: Arc<RwLock<Option<ShardStageHandler>>> =
            Arc::new(RwLock::new(None));
        let inbound_stream_admission =
            InboundStreamAdmission::new(MAX_INBOUND_STREAM_TASKS, MAX_INBOUND_STREAMS_PER_PEER);
        tokio::spawn(run_incoming_job_relay_streams(
            incoming_job_relay_streams,
            job_relay_stream_handler.clone(),
            inbound_stream_admission.clone(),
        ));
        tokio::spawn(run_incoming_blob_streams(
            incoming_blob_streams,
            blob_stream_handler.clone(),
            inbound_stream_admission.clone(),
        ));
        tokio::spawn(run_incoming_shard_stage_streams(
            incoming_shard_stage_streams,
            shard_stage_handler.clone(),
            inbound_stream_admission,
        ));
        let job_relay_stream_control = Arc::new(tokio::sync::Mutex::new(job_relay_stream_control));

        // Wire up bootstrap peers. Format expected:
        //   /ip4/x.x.x.x/tcp/<port>/p2p/<peer-id>
        //   /dns4/host.example/tcp/<port>/p2p/<peer-id>
        //   /ip6/...
        // We extract the trailing /p2p/<id> component, add the address to
        // Kademlia's routing table so DHT queries can route to it, then
        // queue a dial so the connection establishes during driver startup.
        for peer_addr_str in &config.bootstrap_peers {
            match peer_addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    let peer_id_opt = addr.iter().find_map(|p| match p {
                        libp2p::multiaddr::Protocol::P2p(peer_id) => Some(peer_id),
                        _ => None,
                    });
                    match peer_id_opt {
                        Some(peer_id) => {
                            swarm
                                .behaviour_mut()
                                .kademlia
                                .add_address(&peer_id, addr.clone());
                            match swarm.dial(addr.clone()) {
                                Ok(()) => info!("Dialing bootstrap peer: {}", addr),
                                Err(e) => warn!("Bootstrap dial failed: {}: {}", addr, e),
                            }
                        }
                        None => warn!("Bootstrap peer missing /p2p/ component: {}", addr),
                    }
                }
                Err(e) => warn!("Invalid bootstrap multiaddr {peer_addr_str:?}: {e}"),
            }
        }

        // Channel sized large enough for typical bursts (one command per
        // public method call). Backpressure here would mean the daemon is
        // calling Discovery faster than libp2p can keep up — fine to block.
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);

        let driver_caps = config.capabilities.clone();
        let driver_peer_id = local_peer_id;
        let driver_reachability = reachability.clone();
        let driver = tokio::spawn(async move {
            Driver::run(
                swarm,
                cmd_rx,
                driver_caps,
                driver_peer_id,
                driver_reachability,
            )
            .await;
        });

        Ok(Self {
            local_peer_id,
            signing_key,
            capabilities: config.capabilities,
            cmd_tx,
            job_relay_stream_control,
            job_relay_stream_handler,
            blob_stream_handler,
            shard_stage_handler,
            driver: Some(driver),
        })
    }

    /// Get the local libp2p peer ID.
    pub fn local_peer_id(&self) -> &PeerId {
        &self.local_peer_id
    }

    /// Get local advertised capabilities.
    pub fn capabilities(&self) -> &PeerCapabilities {
        &self.capabilities
    }

    /// Node's Ed25519 verifying key, hex-encoded.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }

    /// Borrow the node's Ed25519 signing key.
    ///
    /// Used by the daemon to build a co-key'd ExecutionHandler (or, after
    /// M4, any Worker implementation) so receipts and the libp2p PeerId
    /// share one root identity.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Start listening on the configured address.
    ///
    /// Takes `&self` rather than `&mut self`: the swarm lives behind the
    /// internal driver task and is reached through a cloneable
    /// `mpsc::Sender`. That lets a single `Arc<Discovery>` be shared across
    /// the router (LUCID M5), the model registry's refresh task, and any
    /// HTTP handler without external locking.
    pub async fn listen(&self, addr: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Listen {
                addr: addr.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Return the concrete addresses currently bound by the swarm. This is
    /// especially useful after listening on port zero and avoids tests or
    /// operators guessing an ephemeral port from log output.
    pub async fn listen_addrs(&self) -> Result<Vec<Multiaddr>> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ListenAddrs { reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))
    }

    /// Snapshot the currently observed reachability state. Paths represent
    /// real established connections or successful DCUtR events; this method
    /// never infers traversal success merely from enabled configuration.
    pub async fn reachability_snapshot(&self) -> Result<ReachabilitySnapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ReachabilitySnapshot { reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))
    }

    /// Add an operator-verified external address. Unspecified, multicast,
    /// memory-only, and transport-less addresses are rejected before they can
    /// be advertised through identify or rendezvous.
    pub async fn add_external_address(&self, address: Multiaddr) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::AddExternalAddress { address, reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Register this node's confirmed external addresses with a rendezvous
    /// server and wait for the signed registration acknowledgement.
    pub async fn register_rendezvous(
        &self,
        rendezvous_node: PeerId,
        namespace: &str,
        ttl_seconds: Option<u64>,
    ) -> Result<()> {
        let namespace = parse_rendezvous_namespace(namespace)?;
        if ttl_seconds
            .is_some_and(|ttl| !(rendezvous::MIN_TTL..=rendezvous::MAX_TTL).contains(&ttl))
        {
            return Err(anyhow!(
                "rendezvous TTL must be within {}..={} seconds",
                rendezvous::MIN_TTL,
                rendezvous::MAX_TTL
            ));
        }
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::RendezvousRegister {
                rendezvous_node,
                namespace,
                ttl_seconds,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Discover a bounded page of registrations from a rendezvous server.
    /// Returned addresses are protocol-signed by the registering peer; callers
    /// still decide which peer to dial.
    pub async fn discover_rendezvous(
        &self,
        rendezvous_node: PeerId,
        namespace: Option<&str>,
        limit: u64,
    ) -> Result<Vec<RendezvousPeer>> {
        if !(1..=MAX_RENDEZVOUS_DISCOVER_RESULTS).contains(&limit) {
            return Err(anyhow!(
                "rendezvous discover limit must be within 1..={MAX_RENDEZVOUS_DISCOVER_RESULTS}"
            ));
        }
        let namespace = namespace.map(parse_rendezvous_namespace).transpose()?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::RendezvousDiscover {
                rendezvous_node,
                namespace,
                limit,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Bootstrap the DHT.
    pub async fn bootstrap(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Bootstrap { reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Manually dial a peer by multiaddr.
    pub async fn dial_peer(&self, addr: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Dial {
                addr: addr.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Advertise this node's capabilities on the DHT.
    pub async fn advertise_capabilities(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::AdvertiseCapabilities { reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Discover peers advertising a given architecture + workload label.
    ///
    /// `kind_label` is the string form of the kind, e.g. `"wasm"` or
    /// `"inference"`. Pre-M2 this was a free-form runtime string like
    /// `"wasmtime"`; the workload-agnostic form is what M2 introduces.
    pub async fn discover_peers(&self, arch: &str, kind_label: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::DiscoverPeers {
                arch: arch.to_string(),
                kind_label: kind_label.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Publish an opaque Kademlia record (key + value bytes).
    ///
    /// The daemon's `ManifestRecord` keys/values go through this entry
    /// point — phase-net deliberately does not depend on `ManifestRecord`
    /// (it lives in `daemon/src/provider/`) so the API is bytes-in,
    /// bytes-out. The boot manifest record shape stays daemon-side and is
    /// scheduled to move to `phase-artifact-server` in M6.
    pub async fn publish_kad_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        validate_kad_record_key(&key)?;
        if value.is_empty() || value.len() > MAX_KAD_RECORD_VALUE_BYTES {
            return Err(anyhow!(
                "Kademlia record value must be within 1..={MAX_KAD_RECORD_VALUE_BYTES} bytes"
            ));
        }
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::PublishKadRecord {
                key,
                value,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Look up records published under `key` on the Kademlia DHT.
    ///
    /// Issues a Kademlia `get_record` query and accumulates distinct
    /// peer-supplied values until completion, subject to the generic record,
    /// count, and aggregate-byte bounds above. Decoding (and signature
    /// verification, in the case of LUCID records) remains the caller's
    /// responsibility.
    ///
    /// Returns an empty `Vec` if no peer holds a record under `key` — that
    /// is *not* an error, just a normal "miss".
    pub async fn get_kad_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        validate_kad_record_key(&key)?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::GetKadRecord { key, reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Advertise this peer as a provider for an opaque, bounded application
    /// key. Completion reflects the Kademlia operation, not an application
    /// trust or availability verdict.
    pub async fn start_providing_key(&self, key: Vec<u8>) -> Result<()> {
        validate_kad_record_key(&key)?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::StartProvidingKey { key, reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Discover at most [`MAX_PROVIDER_PEERS_PER_QUERY`] distinct provider
    /// identities for an opaque application key. Callers must authenticate
    /// the application records served by those peers independently.
    pub async fn get_provider_peers(&self, key: Vec<u8>) -> Result<Vec<PeerId>> {
        validate_kad_record_key(&key)?;
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::GetProviderPeers { key, reply: tx })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Send a `JobOffer` to a peer over the libp2p request/response wire
    /// and await its `JobResponse`.
    ///
    /// This is the wire-level dispatch entry point — what the November
    /// 2025 MVP exposed as the local-only `handle_job_offer` helper now
    /// has a real over-the-network counterpart. The pre-M2 boundary test
    /// (`daemon/tests/boundary_libp2p_job.rs`) used the local helper; M2
    /// re-points it at this method.
    ///
    /// Returns an error if the peer is unreachable, the request times out,
    /// or the response fails to deserialize.
    pub async fn send_job_offer(&self, peer: PeerId, offer: JobOffer) -> Result<JobResponse> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SendJobOffer {
                peer,
                offer,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Send a [`JobRelayRequest`] to `peer` over the `/phase/job-relay/1.0.0`
    /// protocol and await the [`JobRelayResponse`].
    ///
    /// Phase-net does not interpret the inner payload — it only ferries
    /// the bytes. The serving peer's inbound handler (registered via
    /// [`Discovery::set_job_relay_handler`]) is responsible for decoding
    /// the bincode `SignedManifest<JobSpec>`, executing it on a local
    /// worker, and re-encoding the result.
    pub async fn send_job_relay(
        &self,
        peer: PeerId,
        request: JobRelayRequest,
    ) -> Result<JobRelayResponse> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SendJobRelay {
                peer,
                request,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        rx.await
            .map_err(|_| anyhow!("Discovery driver dropped reply"))?
    }

    /// Install (or replace) the inbound handler for the LUCID M5 job
    /// relay protocol. Pass `None` to fall back to the default
    /// "no handler installed" refusal — every inbound request comes back
    /// as `JobRelayResponse::Err`.
    pub async fn set_job_relay_handler(&self, handler: Option<JobRelayHandler>) -> Result<()> {
        self.cmd_tx
            .send(Command::SetJobRelayHandler { handler })
            .await
            .map_err(|_| anyhow!("Discovery driver shut down"))?;
        Ok(())
    }

    /// Open a distinct v2 live-relay substream and send its validated open
    /// envelope. The returned handle yields server frames as they arrive;
    /// it does not batch them or synthesize a stream after completion.
    pub async fn open_job_relay_stream(
        &self,
        peer: PeerId,
        open: JobRelayStreamOpen,
    ) -> Result<JobRelayLiveStream> {
        let now_unix_ms = unix_time_ms();
        open.validate(now_unix_ms).map_err(anyhow::Error::new)?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(open.deadline_unix_ms.saturating_sub(now_unix_ms));
        let idle_timeout = Duration::from_millis(open.idle_timeout_ms);
        let negotiation_deadline = outbound_stream_negotiation_deadline(deadline);

        // `libp2p_stream::Control::open_stream` takes `&mut self`. A single
        // mutex preserves the crate's intended control backpressure without
        // cloning one control per job; the lock is released as soon as the
        // substream is negotiated.
        let mut control =
            tokio::time::timeout_at(negotiation_deadline, self.job_relay_stream_control.lock())
                .await
                .map_err(|_| anyhow!("live relay negotiation timed out"))?;
        let mut stream = tokio::time::timeout_at(
            negotiation_deadline,
            control.open_stream(peer, StreamProtocol::new(JOB_RELAY_STREAM_PROTOCOL)),
        )
        .await
        .map_err(|_| anyhow!("live relay negotiation timed out"))?
        .map_err(|e| anyhow!("open live relay stream to {peer}: {e}"))?;
        drop(control);

        tokio::time::timeout_at(
            negotiation_deadline,
            write_postcard_frame(&mut stream, &open, JOB_RELAY_STREAM_MAX_OPEN_BYTES + 1024),
        )
        .await
        .map_err(|_| anyhow!("live relay open write timed out"))?
        .context("write live relay open envelope")?;

        let (reader, writer) = stream.split();
        let (frames_tx, frames_rx) = mpsc::channel(8);
        let (controls_tx, controls_rx) = mpsc::channel(4);
        let job_id = open.job_id;
        tokio::spawn(drive_outbound_job_relay_stream(
            reader,
            writer,
            job_id,
            frames_tx,
            controls_rx,
            idle_timeout,
            deadline,
        ));

        Ok(JobRelayLiveStream {
            job_id,
            frames: frames_rx,
            controls: controls_tx,
        })
    }

    /// Install or clear the server callback for v2 live-relay substreams.
    /// With no handler, phase-net returns an explicit sequence-zero refusal.
    pub fn set_job_relay_stream_handler(
        &self,
        handler: Option<JobRelayStreamHandler>,
    ) -> Result<()> {
        let mut slot = self
            .job_relay_stream_handler
            .write()
            .map_err(|_| anyhow!("live relay handler lock poisoned"))?;
        *slot = handler;
        Ok(())
    }

    /// Open a workload-neutral, resumable content stream to `peer`.
    ///
    /// The request carries a fixed 32-byte content ID plus bounded opaque
    /// metadata. The returned handle yields validated frames immediately as
    /// they arrive and never buffers the complete object.
    pub async fn open_blob_stream(
        &self,
        peer: PeerId,
        request: BlobStreamRequest,
    ) -> Result<BlobStream> {
        let now = unix_time_ms();
        request.validate(now).map_err(anyhow::Error::new)?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(request.deadline_unix_ms.saturating_sub(now));
        let idle_timeout = Duration::from_millis(request.idle_timeout_ms);
        let negotiation_deadline = outbound_stream_negotiation_deadline(deadline);

        let mut control =
            tokio::time::timeout_at(negotiation_deadline, self.job_relay_stream_control.lock())
                .await
                .map_err(|_| anyhow!("blob stream negotiation timed out"))?;
        let mut stream = tokio::time::timeout_at(
            negotiation_deadline,
            control.open_stream(peer, StreamProtocol::new(BLOB_STREAM_PROTOCOL)),
        )
        .await
        .map_err(|_| anyhow!("blob stream negotiation timed out"))?
        .map_err(|error| anyhow!("open blob stream to {peer}: {error}"))?;
        drop(control);

        tokio::time::timeout_at(
            negotiation_deadline,
            write_postcard_frame(&mut stream, &request, BLOB_STREAM_REQUEST_WIRE_MAX_BYTES),
        )
        .await
        .map_err(|_| anyhow!("blob stream request write timed out"))??;

        let content_id = request.content_id;
        let offset = request.offset;
        Ok(BlobStream {
            content_id,
            stream,
            validator: BlobStreamValidator::new(content_id, offset),
            idle_timeout,
            deadline,
            closed: false,
        })
    }

    /// Install or clear the inbound blob callback. With no handler, phase-net
    /// returns a bounded explicit rejection without consulting storage.
    pub fn set_blob_stream_handler(&self, handler: Option<BlobStreamHandler>) -> Result<()> {
        let mut slot = self
            .blob_stream_handler
            .write()
            .map_err(|_| anyhow!("blob stream handler lock poisoned"))?;
        *slot = handler;
        Ok(())
    }

    /// Execute one bounded pipeline stage on `peer` over [`SHARD_STAGE_PROTOCOL`].
    pub async fn execute_shard_stage(
        &self,
        peer: PeerId,
        request: ShardStageRequest,
    ) -> Result<ShardStageResponse> {
        let now = unix_time_ms();
        request.validate(now).map_err(anyhow::Error::new)?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(request.deadline_unix_ms.saturating_sub(now));
        let idle_timeout = Duration::from_millis(request.idle_timeout_ms);
        let negotiation_deadline = outbound_stream_negotiation_deadline(deadline);

        let mut control =
            tokio::time::timeout_at(negotiation_deadline, self.job_relay_stream_control.lock())
                .await
                .map_err(|_| anyhow!("shard stage negotiation timed out"))?;
        let mut stream = tokio::time::timeout_at(
            negotiation_deadline,
            control.open_stream(peer, StreamProtocol::new(SHARD_STAGE_PROTOCOL)),
        )
        .await
        .map_err(|_| anyhow!("shard stage negotiation timed out"))?
        .map_err(|error| anyhow!("open shard stage stream to {peer}: {error}"))?;
        drop(control);

        tokio::time::timeout_at(
            negotiation_deadline,
            write_postcard_frame(&mut stream, &request, SHARD_STAGE_REQUEST_WIRE_MAX_BYTES),
        )
        .await
        .map_err(|_| anyhow!("shard stage request write timed out"))??;

        let read_deadline = deadline.min(tokio::time::Instant::now() + idle_timeout);
        let response: ShardStageResponse = tokio::time::timeout_at(
            read_deadline,
            read_postcard_frame(&mut stream, SHARD_STAGE_RESPONSE_WIRE_MAX_BYTES),
        )
        .await
        .map_err(|_| anyhow!("shard stage response timed out"))??;
        let _ = stream.close().await;

        response
            .validate_for(request.session_id, request.stage_index)
            .map_err(anyhow::Error::new)?;
        Ok(response)
    }

    /// Install or clear the inbound pipeline-stage callback. With no handler,
    /// phase-net drops inbound stage streams fail-closed.
    pub fn set_shard_stage_handler(&self, handler: Option<ShardStageHandler>) -> Result<()> {
        let mut slot = self
            .shard_stage_handler
            .write()
            .map_err(|_| anyhow!("shard stage handler lock poisoned"))?;
        *slot = handler;
        Ok(())
    }

    /// Run until the background driver task exits. The November 2025 MVP's
    /// `plasmd start` calls this to keep the daemon alive after dispatching
    /// configuration. After M2 the actual swarm polling lives inside the
    /// driver task; this method just waits for that task to finish, which
    /// happens when all `Discovery` handles are dropped or the process is
    /// killed.
    pub async fn run(&mut self) -> Result<()> {
        if let Some(handle) = self.driver.take() {
            // The driver task should outlive `run()` only if the caller
            // intentionally tears the daemon down. JoinError just means the
            // task was cancelled — surface it but don't crash the daemon.
            if let Err(e) = handle.await {
                warn!("Discovery driver task exited with error: {:?}", e);
            }
        }
        Ok(())
    }

    /// Evaluate a JobOffer against local capabilities without going over
    /// the wire. Preserved as a public entry point for tests that already
    /// drove the rejection contract through this surface; production code
    /// should prefer [`Discovery::send_job_offer`].
    pub async fn handle_job_offer(&self, offer: JobOffer) -> JobResponse {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(Command::EvaluateOffer {
                offer: offer.clone(),
                reply: tx,
            })
            .await
            .is_err()
        {
            // Driver gone; return a synthetic rejection so the API stays
            // total. Production callers should treat this as fatal.
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::InvalidRequest {
                    details: "discovery driver shut down".into(),
                },
            };
        }
        rx.await.unwrap_or_else(|_| JobResponse::Rejected {
            job_id: offer.job_id,
            reason: RejectionReason::InvalidRequest {
                details: "discovery driver dropped reply".into(),
            },
        })
    }
}

const JOB_RELAY_STREAM_WIRE_MAX_BYTES: usize = JOB_RELAY_STREAM_MAX_EVENT_BYTES + 4096;
const BLOB_STREAM_REQUEST_WIRE_MAX_BYTES: usize = BLOB_STREAM_MAX_METADATA_BYTES + 1024;
const BLOB_STREAM_WIRE_MAX_BYTES: usize = BLOB_STREAM_MAX_CHUNK_BYTES + 1024;
const SHARD_STAGE_REQUEST_WIRE_MAX_BYTES: usize =
    SHARD_STAGE_MAX_PLAN_BYTES + SHARD_STAGE_MAX_PAYLOAD_BYTES + 4096;
const SHARD_STAGE_RESPONSE_WIRE_MAX_BYTES: usize =
    SHARD_STAGE_MAX_PAYLOAD_BYTES + SHARD_STAGE_MAX_RECEIPT_BYTES + 4096;
/// Transport control-lock, multistream-select, and initial-open-envelope
/// budget. Caller-provided idle timeouts govern silence only after a substream
/// is established; coupling them to scheduler/negotiation time made the
/// protocol's valid 250 ms minimum unusable under bounded CPU load.
const OUTBOUND_STREAM_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(10);
const BLOB_STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

fn outbound_stream_negotiation_deadline(
    total_deadline: tokio::time::Instant,
) -> tokio::time::Instant {
    total_deadline.min(tokio::time::Instant::now() + OUTBOUND_STREAM_NEGOTIATION_TIMEOUT)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn write_postcard_frame<W, T>(writer: &mut W, value: &T, maximum: usize) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let encoded = postcard::to_stdvec(value).context("encode live relay frame")?;
    if encoded.is_empty() || encoded.len() > maximum {
        return Err(anyhow!(
            "encoded live relay frame size {} exceeds maximum {maximum}",
            encoded.len()
        ));
    }
    let length = u32::try_from(encoded.len()).context("live relay frame length exceeds u32")?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .context("write live relay frame length")?;
    writer
        .write_all(&encoded)
        .await
        .context("write live relay frame body")?;
    writer.flush().await.context("flush live relay frame")?;
    Ok(())
}

async fn read_postcard_frame<R, T>(reader: &mut R, maximum: usize) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut length_bytes = [0_u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .await
        .context("read live relay frame length")?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > maximum {
        return Err(anyhow!(
            "live relay wire frame size {length} is outside 1..={maximum}"
        ));
    }
    let mut encoded = vec![0_u8; length];
    reader
        .read_exact(&mut encoded)
        .await
        .context("read live relay frame body")?;
    postcard::from_bytes(&encoded).context("decode live relay frame")
}

async fn drive_outbound_job_relay_stream<R, W>(
    mut reader: R,
    mut writer: W,
    job_id: [u8; 32],
    frames_tx: mpsc::Sender<std::result::Result<JobRelayStreamFrame, String>>,
    mut controls_rx: mpsc::Receiver<JobRelayStreamControl>,
    idle_timeout: Duration,
    deadline: tokio::time::Instant,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut validator = JobRelayStreamValidator::new(job_id);
    let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                fail_outbound_live_stream(
                    &mut writer,
                    &frames_tx,
                    job_id,
                    "live relay deadline reached",
                ).await;
                break;
            }
            _ = tokio::time::sleep_until(idle_deadline) => {
                fail_outbound_live_stream(
                    &mut writer,
                    &frames_tx,
                    job_id,
                    "live relay idle timeout reached",
                ).await;
                break;
            }
            control = controls_rx.recv() => {
                let Some(control) = control else {
                    let cancel = JobRelayStreamControl {
                        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                        job_id,
                        kind: JobRelayStreamControlKind::Cancel {
                            reason: "requester dropped live relay stream".to_string(),
                        },
                    };
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        write_postcard_frame(
                            &mut writer,
                            &cancel,
                            JOB_RELAY_STREAM_MAX_OPEN_BYTES,
                        ),
                    ).await;
                    let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
                    break;
                };
                if let Err(error) = control.validate_for(job_id) {
                    let _ = frames_tx.try_send(Err(error.to_string()));
                    break;
                }
                if matches!(control.kind, JobRelayStreamControlKind::ReceiptAck)
                    && !validator.is_complete()
                {
                    let _ = frames_tx.try_send(Err(
                        "receipt acknowledged before terminal receipt".to_string()
                    ));
                    break;
                }
                let acknowledged = matches!(control.kind, JobRelayStreamControlKind::ReceiptAck);
                let write_deadline = deadline.min(tokio::time::Instant::now() + idle_timeout);
                match tokio::time::timeout_at(
                    write_deadline,
                    write_postcard_frame(
                        &mut writer,
                        &control,
                        JOB_RELAY_STREAM_MAX_OPEN_BYTES,
                    ),
                ).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        let _ = frames_tx.try_send(Err(error.to_string()));
                        break;
                    }
                    Err(_) => {
                        let _ = frames_tx.try_send(Err(
                            "live relay control write timed out".to_string()
                        ));
                        break;
                    }
                }
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
                if acknowledged {
                    let _ = writer.close().await;
                    break;
                }
            }
            result = read_postcard_frame::<_, JobRelayStreamFrame>(
                &mut reader,
                JOB_RELAY_STREAM_WIRE_MAX_BYTES,
            ) => {
                let frame = match result {
                    Ok(frame) => frame,
                    Err(error) => {
                        if validator.validate_eof().is_err() {
                            let _ = frames_tx.try_send(Err(error.to_string()));
                        }
                        break;
                    }
                };
                if let Err(error) = validator.validate(&frame) {
                    let _ = frames_tx.try_send(Err(error.to_string()));
                    break;
                }
                let delivery_deadline = deadline.min(tokio::time::Instant::now() + idle_timeout);
                let delivery = tokio::time::timeout_at(delivery_deadline, frames_tx.send(Ok(frame))).await;
                if !matches!(delivery, Ok(Ok(()))) {
                    let cancel = JobRelayStreamControl {
                        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                        job_id,
                        kind: JobRelayStreamControlKind::Cancel {
                            reason: "requester stopped consuming live relay output before liveness deadline".to_string(),
                        },
                    };
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        write_postcard_frame(
                            &mut writer,
                            &cancel,
                            JOB_RELAY_STREAM_MAX_OPEN_BYTES,
                        ),
                    ).await;
                    break;
                }
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
            }
        }
    }
}

async fn fail_outbound_live_stream<W>(
    writer: &mut W,
    frames_tx: &mpsc::Sender<std::result::Result<JobRelayStreamFrame, String>>,
    job_id: [u8; 32],
    reason: &'static str,
) where
    W: AsyncWrite + Unpin,
{
    let cancel = JobRelayStreamControl {
        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
        job_id,
        kind: JobRelayStreamControlKind::Cancel {
            reason: reason.to_string(),
        },
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        write_postcard_frame(writer, &cancel, JOB_RELAY_STREAM_MAX_OPEN_BYTES),
    )
    .await;
    let _ = frames_tx.try_send(Err(reason.to_string()));
    let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
}

async fn run_incoming_job_relay_streams(
    mut incoming: libp2p_stream::IncomingStreams,
    handler_slot: Arc<RwLock<Option<JobRelayStreamHandler>>>,
    admission: Arc<InboundStreamAdmission>,
) {
    while let Some((peer, stream)) = incoming.next().await {
        let serving_enabled = handler_slot
            .read()
            .map(|handler| handler.is_some())
            .unwrap_or(false);
        if !serving_enabled {
            debug!(peer = %peer, "dropping inbound live relay stream while serving is disabled");
            drop(stream);
            continue;
        }
        let Some(permit) = admission.try_acquire(peer) else {
            warn!(peer = %peer, "dropping inbound live relay stream at admission limit");
            drop(stream);
            continue;
        };
        let handler_slot = handler_slot.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = serve_incoming_job_relay_stream(peer, stream, handler_slot).await {
                warn!(peer = %peer, error = %error, "live relay inbound stream failed");
            }
        });
    }
}

async fn serve_incoming_job_relay_stream(
    peer: PeerId,
    stream: libp2p::swarm::Stream,
    handler_slot: Arc<RwLock<Option<JobRelayStreamHandler>>>,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();
    let open: JobRelayStreamOpen = tokio::time::timeout(
        Duration::from_millis(JOB_RELAY_STREAM_DEFAULT_IDLE_TIMEOUT_MS),
        read_postcard_frame(&mut reader, JOB_RELAY_STREAM_MAX_OPEN_BYTES + 1024),
    )
    .await
    .map_err(|_| anyhow!("live relay open envelope timed out"))?
    .context("read live relay open envelope")?;

    if let Err(error) = open.validate(unix_time_ms()) {
        let rejection = JobRelayStreamFrame {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: open.job_id,
            sequence: 0,
            kind: JobRelayStreamFrameKind::Rejected {
                reason: format!("invalid live relay open: {error}"),
            },
        };
        write_postcard_frame(&mut writer, &rejection, JOB_RELAY_STREAM_WIRE_MAX_BYTES).await?;
        writer.close().await?;
        return Ok(());
    }

    let handler = handler_slot
        .read()
        .map_err(|_| anyhow!("live relay handler lock poisoned"))?
        .clone();
    let Some(handler) = handler else {
        let rejection = JobRelayStreamFrame {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: open.job_id,
            sequence: 0,
            kind: JobRelayStreamFrameKind::Rejected {
                reason: "no live relay handler installed".to_string(),
            },
        };
        write_postcard_frame(&mut writer, &rejection, JOB_RELAY_STREAM_WIRE_MAX_BYTES).await?;
        writer.close().await?;
        return Ok(());
    };

    let job_id = open.job_id;
    let deadline_after =
        Duration::from_millis(open.deadline_unix_ms.saturating_sub(unix_time_ms()));
    let deadline = tokio::time::Instant::now() + deadline_after;
    let idle_timeout = Duration::from_millis(open.idle_timeout_ms);
    let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
    let (controls_tx, controls_rx) = mpsc::channel(4);
    let (frames_tx, mut frames_rx) = mpsc::channel(8);
    let mut handler_task = tokio::spawn(handler(peer, open, controls_rx, frames_tx));
    let mut validator = JobRelayStreamValidator::new(job_id);
    let mut accepted = false;
    let mut next_sequence = 0_u64;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                let _ = controls_tx.send(JobRelayStreamControl {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id,
                    kind: JobRelayStreamControlKind::Cancel {
                        reason: "live relay deadline reached".to_string(),
                    },
                }).await;
                send_live_timeout_frame(
                    &mut writer,
                    &mut validator,
                    job_id,
                    next_sequence,
                    accepted,
                    "live relay deadline reached",
                    deadline,
                ).await?;
                break;
            }
            _ = tokio::time::sleep_until(idle_deadline) => {
                let _ = controls_tx.send(JobRelayStreamControl {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id,
                    kind: JobRelayStreamControlKind::Cancel {
                        reason: "live relay idle timeout reached".to_string(),
                    },
                }).await;
                send_live_timeout_frame(
                    &mut writer,
                    &mut validator,
                    job_id,
                    next_sequence,
                    accepted,
                    "live relay idle timeout reached",
                    deadline,
                ).await?;
                break;
            }
            frame = frames_rx.recv() => {
                let Some(frame) = frame else {
                    break;
                };
                validator.validate(&frame).map_err(anyhow::Error::new)?;
                accepted |= matches!(&frame.kind, JobRelayStreamFrameKind::Accepted);
                next_sequence = next_sequence.saturating_add(1);
                let write_deadline = deadline.min(tokio::time::Instant::now() + idle_timeout);
                tokio::time::timeout_at(
                    write_deadline,
                    write_postcard_frame(
                        &mut writer,
                        &frame,
                        JOB_RELAY_STREAM_WIRE_MAX_BYTES,
                    ),
                )
                .await
                .map_err(|_| anyhow!("live relay response write timed out"))??;
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
            }
            result = read_postcard_frame::<_, JobRelayStreamControl>(
                &mut reader,
                JOB_RELAY_STREAM_MAX_OPEN_BYTES,
            ) => {
                let control = match result {
                    Ok(control) => control,
                    Err(error) => {
                        let _ = controls_tx.send(JobRelayStreamControl {
                            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                            job_id,
                            kind: JobRelayStreamControlKind::Cancel {
                                reason: format!("requester stream lost: {error}"),
                            },
                        }).await;
                        break;
                    }
                };
                control.validate_for(job_id).map_err(anyhow::Error::new)?;
                let acknowledged = matches!(control.kind, JobRelayStreamControlKind::ReceiptAck);
                controls_tx
                    .send(control)
                    .await
                    .map_err(|_| anyhow!("live relay handler stopped receiving controls"))?;
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
                if acknowledged {
                    if !validator.is_complete() {
                        return Err(anyhow!("requester acknowledged receipt before completion"));
                    }
                    break;
                }
            }
        }
    }

    // Give a cooperative handler one short cleanup window to observe the
    // cancellation we just delivered. A non-cooperative handler is then
    // aborted deterministically so its worker permit and stream resources
    // cannot leak beyond the liveness boundary.
    if tokio::time::timeout(Duration::from_millis(250), &mut handler_task)
        .await
        .is_err()
    {
        handler_task.abort();
        let _ = handler_task.await;
    }
    writer.close().await.context("close live relay stream")?;
    Ok(())
}

async fn send_live_timeout_frame<W>(
    writer: &mut W,
    validator: &mut JobRelayStreamValidator,
    job_id: [u8; 32],
    sequence: u64,
    accepted: bool,
    reason: &'static str,
    deadline: tokio::time::Instant,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if validator.is_complete() {
        return Ok(());
    }
    let kind = if accepted {
        JobRelayStreamFrameKind::Failed {
            reason: reason.to_string(),
        }
    } else {
        JobRelayStreamFrameKind::Rejected {
            reason: reason.to_string(),
        }
    };
    let frame = JobRelayStreamFrame {
        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
        job_id,
        sequence,
        kind,
    };
    validator.validate(&frame).map_err(anyhow::Error::new)?;
    let write_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(1));
    let _ = tokio::time::timeout_at(
        write_deadline,
        write_postcard_frame(writer, &frame, JOB_RELAY_STREAM_WIRE_MAX_BYTES),
    )
    .await;
    Ok(())
}

async fn run_incoming_blob_streams(
    mut incoming: libp2p_stream::IncomingStreams,
    handler_slot: Arc<RwLock<Option<BlobStreamHandler>>>,
    admission: Arc<InboundStreamAdmission>,
) {
    while let Some((peer, stream)) = incoming.next().await {
        let serving_enabled = handler_slot
            .read()
            .map(|handler| handler.is_some())
            .unwrap_or(false);
        if !serving_enabled {
            debug!(peer = %peer, "dropping inbound blob stream while content serving is disabled");
            drop(stream);
            continue;
        }
        let Some(permit) = admission.try_acquire(peer) else {
            warn!(peer = %peer, "dropping inbound blob stream at admission limit");
            drop(stream);
            continue;
        };
        let handler_slot = handler_slot.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_incoming_blob_stream(peer, stream, handler_slot, permit).await
            {
                warn!(peer = %peer, error = %error, "blob inbound stream failed");
            }
        });
    }
}

async fn run_incoming_shard_stage_streams(
    mut incoming: libp2p_stream::IncomingStreams,
    handler_slot: Arc<RwLock<Option<ShardStageHandler>>>,
    admission: Arc<InboundStreamAdmission>,
) {
    while let Some((peer, stream)) = incoming.next().await {
        let serving_enabled = handler_slot
            .read()
            .map(|handler| handler.is_some())
            .unwrap_or(false);
        if !serving_enabled {
            debug!(peer = %peer, "dropping inbound shard stage stream while cluster serving is disabled");
            drop(stream);
            continue;
        }
        let Some(permit) = admission.try_acquire(peer) else {
            warn!(peer = %peer, "dropping inbound shard stage stream at admission limit");
            drop(stream);
            continue;
        };
        let handler_slot = handler_slot.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = serve_incoming_shard_stage_stream(peer, stream, handler_slot).await
            {
                warn!(peer = %peer, error = %error, "shard stage inbound stream failed");
            }
        });
    }
}

async fn serve_incoming_shard_stage_stream(
    peer: PeerId,
    mut stream: libp2p::swarm::Stream,
    handler_slot: Arc<RwLock<Option<ShardStageHandler>>>,
) -> Result<()> {
    let request: ShardStageRequest = tokio::time::timeout(
        BLOB_STREAM_OPEN_TIMEOUT,
        read_postcard_frame(&mut stream, SHARD_STAGE_REQUEST_WIRE_MAX_BYTES),
    )
    .await
    .map_err(|_| anyhow!("shard stage request open timed out"))?
    .context("read shard stage request")?;

    if let Err(error) = request.validate(unix_time_ms()) {
        let rejection = ShardStageResponse {
            schema_version: SHARD_STAGE_SCHEMA_VERSION,
            session_id: request.session_id,
            stage_index: request.stage_index,
            kind: ShardStageResponseKind::Rejected {
                reason: format!("invalid shard stage request: {error}"),
            },
        };
        write_postcard_frame(&mut stream, &rejection, SHARD_STAGE_RESPONSE_WIRE_MAX_BYTES).await?;
        stream
            .close()
            .await
            .context("close rejected shard stream")?;
        return Ok(());
    }

    let handler = handler_slot
        .read()
        .map_err(|_| anyhow!("shard stage handler lock poisoned"))?
        .clone();
    let Some(handler) = handler else {
        let rejection = ShardStageResponse {
            schema_version: SHARD_STAGE_SCHEMA_VERSION,
            session_id: request.session_id,
            stage_index: request.stage_index,
            kind: ShardStageResponseKind::Rejected {
                reason: "no shard stage handler installed".to_string(),
            },
        };
        write_postcard_frame(&mut stream, &rejection, SHARD_STAGE_RESPONSE_WIRE_MAX_BYTES).await?;
        stream
            .close()
            .await
            .context("close rejected shard stream")?;
        return Ok(());
    };

    let session_id = request.session_id;
    let stage_index = request.stage_index;
    let deadline_after =
        Duration::from_millis(request.deadline_unix_ms.saturating_sub(unix_time_ms()));
    let exec_timeout = deadline_after.min(Duration::from_millis(request.idle_timeout_ms));

    let response = match tokio::time::timeout(exec_timeout, handler(peer, request)).await {
        Ok(resp) => resp,
        Err(_) => ShardStageResponse {
            schema_version: SHARD_STAGE_SCHEMA_VERSION,
            session_id,
            stage_index,
            kind: ShardStageResponseKind::Failed {
                reason: "shard stage execution timed out".to_string(),
            },
        },
    };
    response
        .validate_for(session_id, stage_index)
        .map_err(anyhow::Error::new)?;
    write_postcard_frame(&mut stream, &response, SHARD_STAGE_RESPONSE_WIRE_MAX_BYTES).await?;
    stream.close().await.context("close shard stage stream")?;
    Ok(())
}

async fn serve_incoming_blob_stream(
    peer: PeerId,
    mut stream: libp2p::swarm::Stream,
    handler_slot: Arc<RwLock<Option<BlobStreamHandler>>>,
    admission_permit: InboundStreamPermit,
) -> Result<()> {
    let request: BlobStreamRequest = tokio::time::timeout(
        BLOB_STREAM_OPEN_TIMEOUT,
        read_postcard_frame(&mut stream, BLOB_STREAM_REQUEST_WIRE_MAX_BYTES),
    )
    .await
    .map_err(|_| anyhow!("blob request open timed out"))?
    .context("read blob request")?;

    if let Err(error) = request.validate(unix_time_ms()) {
        let rejection = BlobStreamFrame {
            schema_version: crate::protocol::BLOB_STREAM_SCHEMA_VERSION,
            content_id: request.content_id,
            kind: BlobStreamFrameKind::Rejected {
                reason: format!("invalid blob request: {error}"),
            },
        };
        write_postcard_frame(&mut stream, &rejection, BLOB_STREAM_WIRE_MAX_BYTES).await?;
        stream.close().await.context("close rejected blob stream")?;
        return Ok(());
    }

    let handler = handler_slot
        .read()
        .map_err(|_| anyhow!("blob stream handler lock poisoned"))?
        .clone();
    let Some(handler) = handler else {
        let rejection = BlobStreamFrame {
            schema_version: crate::protocol::BLOB_STREAM_SCHEMA_VERSION,
            content_id: request.content_id,
            kind: BlobStreamFrameKind::Rejected {
                reason: "no blob stream handler installed".to_string(),
            },
        };
        write_postcard_frame(&mut stream, &rejection, BLOB_STREAM_WIRE_MAX_BYTES).await?;
        stream.close().await.context("close rejected blob stream")?;
        return Ok(());
    };

    let content_id = request.content_id;
    let requested_offset = request.offset;
    let idle_timeout = Duration::from_millis(request.idle_timeout_ms);
    let deadline_after =
        Duration::from_millis(request.deadline_unix_ms.saturating_sub(unix_time_ms()));
    let deadline = tokio::time::Instant::now() + deadline_after;
    let (mut reader, mut writer) = stream.split();
    let (frames_tx, mut frames_rx) = mpsc::channel(4);
    let handler_task = tokio::spawn(handler(peer, request, frames_tx));
    let mut validator = BlobStreamValidator::new(content_id, requested_offset);
    let mut close_probe = [0_u8; 1];

    let result = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                break Ok(());
            }
            _ = tokio::time::sleep(idle_timeout) => {
                break Ok(());
            }
            frame = frames_rx.recv() => {
                let Some(frame) = frame else {
                    break validator.validate_eof().map_err(anyhow::Error::new);
                };
                if let Err(error) = validator.validate(&frame) {
                    break Err(anyhow::Error::new(error));
                }
                if let BlobStreamFrameKind::Chunk { bytes, .. } = &frame.kind {
                    if tokio::time::timeout_at(
                        deadline,
                        admission_permit.wait_for_blob_budget(bytes.len()),
                    )
                    .await
                    .is_err()
                    {
                        break Ok(());
                    }
                }
                let write_wait = idle_timeout.min(
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                );
                match tokio::time::timeout(
                    write_wait,
                    write_postcard_frame(&mut writer, &frame, BLOB_STREAM_WIRE_MAX_BYTES),
                ).await {
                    Ok(Ok(())) if validator.is_complete() => break Ok(()),
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break Err(error.context("write blob frame")),
                    Err(_) => break Ok(()),
                }
            }
            close = reader.read(&mut close_probe) => {
                match close {
                    Ok(0) | Err(_) => break Ok(()),
                    Ok(_) => break Err(anyhow!("requester sent data after blob request")),
                }
            }
        }
    };

    handler_task.abort();
    let _ = handler_task.await;
    let close_result = writer.close().await.context("close blob stream");
    result?;
    close_result
}

// ---------------------------------------------------------------------------
// Driver — owns the swarm, processes commands and behaviour events.
// ---------------------------------------------------------------------------

struct Driver {
    swarm: Swarm<CombinedBehaviour>,
    capabilities: PeerCapabilities,
    local_peer_id: PeerId,
    reachability: ReachabilityConfig,
    nat_reachability: NatReachability,
    observed_addresses: Vec<Multiaddr>,
    connection_paths: HashMap<ConnectionId, ReachabilityConnection>,
    pending_relay_listens: Vec<PendingRelayListen>,
    pending_provider_queries: HashMap<QueryId, PendingProviderQuery>,
    pending_rendezvous_registrations: PendingRendezvousRegistrations,
    pending_rendezvous_discoveries: PendingRendezvousDiscoveries,
    /// Outstanding SendJobOffer requests, keyed by libp2p's outbound id.
    pending_offers: HashMap<OutboundRequestId, oneshot::Sender<Result<JobResponse>>>,
    /// Outstanding GetKadRecord queries. Each entry pairs a Kademlia
    /// `QueryId` with the reply channel the caller is awaiting and the
    /// running accumulation of unique record payloads (the same peer can
    /// report a record more than once; we de-dupe before replying).
    pending_get_records: HashMap<QueryId, PendingGetRecord>,
    /// Outstanding outbound JobRelay requests.
    pending_relays: HashMap<OutboundRequestId, oneshot::Sender<Result<JobRelayResponse>>>,
    /// Inbound JobRelay handler. `None` → refuse every inbound request.
    job_relay_handler: Option<JobRelayHandler>,
    /// SEC-06: completed inbound-relay responses flow back from the spawned
    /// handler tasks here so the driver can call `send_response` without ever
    /// awaiting the (slow, GPU-heavy) handler itself. Decouples relay
    /// execution from the swarm event loop — one slow job no longer stalls
    /// peer connectivity.
    relay_reply_tx: mpsc::Sender<(ResponseChannel<JobRelayResponse>, JobRelayResponse)>,
}

struct PendingRelayListen {
    relay_peer: PeerId,
    address: Multiaddr,
    reply: oneshot::Sender<Result<()>>,
    deadline: tokio::time::Instant,
}

enum ProviderReply {
    Publish(oneshot::Sender<Result<()>>),
    Lookup(oneshot::Sender<Result<Vec<PeerId>>>),
}

struct PendingProviderQuery {
    reply: ProviderReply,
    peers: Vec<PeerId>,
    deadline: tokio::time::Instant,
}

impl PendingProviderQuery {
    fn insert(&mut self, peer: PeerId) {
        if self.peers.len() < MAX_PROVIDER_PEERS_PER_QUERY && !self.peers.contains(&peer) {
            self.peers.push(peer);
        }
    }

    fn is_closed(&self) -> bool {
        match &self.reply {
            ProviderReply::Publish(reply) => reply.is_closed(),
            ProviderReply::Lookup(reply) => reply.is_closed(),
        }
    }

    fn complete(mut self, error: Option<anyhow::Error>) {
        match self.reply {
            ProviderReply::Publish(reply) => {
                let _ = reply.send(error.map_or(Ok(()), Err));
            }
            ProviderReply::Lookup(reply) => {
                self.peers.sort_unstable();
                let result = match error {
                    Some(error) if self.peers.is_empty() => Err(error),
                    _ => Ok(self.peers),
                };
                let _ = reply.send(result);
            }
        }
    }
}

/// A circuit reservation must use an established direct control connection.
/// Otherwise relay-client's dial can collide with an already-pending bootstrap
/// dial, close the reservation listener, and leave no circuit to advertise.
fn relay_control_peer(address: &Multiaddr) -> Option<(PeerId, Multiaddr)> {
    let mut control = Multiaddr::empty();
    for protocol in address.iter() {
        if matches!(protocol, Protocol::P2pCircuit) {
            return match control.iter().last() {
                Some(Protocol::P2p(peer)) => Some((peer, control)),
                _ => None,
            };
        }
        control.push(protocol);
    }
    None
}

type RendezvousRegistrationKey = (PeerId, rendezvous::Namespace);
type RendezvousDiscoveryKey = (PeerId, Option<rendezvous::Namespace>);
type PendingRendezvousRegistrations =
    HashMap<RendezvousRegistrationKey, Vec<oneshot::Sender<Result<()>>>>;
type PendingRendezvousDiscoveries =
    HashMap<RendezvousDiscoveryKey, Vec<oneshot::Sender<Result<Vec<RendezvousPeer>>>>>;

/// Accumulator for an outstanding `GetKadRecord` query.
///
/// libp2p emits multiple `OutboundQueryProgressed` events as the iterative
/// query walks the kbucket tree — one per peer that responded — and the
/// driver folds each value into `values` before sending the final reply
/// when `step.last` is set.
struct PendingGetRecord {
    reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
    values: Vec<Vec<u8>>,
    total_bytes: usize,
    truncated: bool,
}

impl PendingGetRecord {
    fn insert_bounded(&mut self, value: Vec<u8>) {
        if value.is_empty() || value.len() > MAX_KAD_RECORD_VALUE_BYTES {
            self.truncated = true;
            return;
        }
        if self.values.contains(&value) {
            return;
        }
        let Some(projected_bytes) = self.total_bytes.checked_add(value.len()) else {
            self.truncated = true;
            return;
        };
        if self.values.len() >= MAX_KAD_RECORD_VALUES_PER_QUERY
            || projected_bytes > MAX_KAD_RECORD_BYTES_PER_QUERY
        {
            self.truncated = true;
            return;
        }
        self.total_bytes = projected_bytes;
        self.values.push(value);
    }
}

fn validate_kad_record_key(key: &[u8]) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KAD_RECORD_KEY_BYTES {
        return Err(anyhow!(
            "Kademlia record key must be within 1..={MAX_KAD_RECORD_KEY_BYTES} bytes"
        ));
    }
    Ok(())
}

impl Driver {
    fn has_direct_connection(&self, peer: &PeerId) -> bool {
        self.connection_paths.values().any(|connection| {
            connection.peer_id == *peer
                && matches!(
                    connection.path,
                    ReachabilityPath::Direct | ReachabilityPath::Dcutr
                )
        })
    }

    fn listen(&mut self, raw: String, reply: oneshot::Sender<Result<()>>) {
        let address = match raw.parse::<Multiaddr>() {
            Ok(address) => address,
            Err(error) => {
                let _ = reply.send(Err(anyhow!("parse listen address: {error}")));
                return;
            }
        };
        let relay = relay_control_peer(&address);
        if self.reachability.outbound_only && relay.is_none() {
            let _ = reply.send(Err(anyhow!("outbound-only mode refuses direct listeners")));
            return;
        }
        if let Some((relay_peer, control_address)) = relay {
            if !self.has_direct_connection(&relay_peer) {
                if self.pending_relay_listens.len() >= MAX_PENDING_RELAY_LISTENS {
                    let _ = reply.send(Err(anyhow!("relay-listener operation capacity reached")));
                    return;
                }
                let options = DialOpts::peer_id(relay_peer)
                    .condition(PeerCondition::NotDialing)
                    .addresses(vec![control_address])
                    .build();
                match self.swarm.dial(options) {
                    Ok(()) | Err(libp2p::swarm::DialError::DialPeerConditionFalse(_)) => {
                        self.pending_relay_listens.push(PendingRelayListen {
                            relay_peer,
                            address,
                            reply,
                            deadline: tokio::time::Instant::now() + DISCOVERY_OPERATION_TIMEOUT,
                        });
                    }
                    Err(error) => {
                        let _ = reply.send(Err(anyhow!("connect relay control peer: {error}")));
                    }
                }
                return;
            }
        }
        let result = self
            .swarm
            .listen_on(address.clone())
            .map(|_| {
                info!("Listening on: {}", address);
            })
            .map_err(anyhow::Error::from);
        let _ = reply.send(result);
    }

    fn finish_relay_listens(&mut self, peer: PeerId, failure: Option<&str>) {
        let pending = std::mem::take(&mut self.pending_relay_listens);
        for request in pending {
            if request.relay_peer != peer {
                self.pending_relay_listens.push(request);
                continue;
            }
            if request.reply.is_closed() {
                continue;
            }
            if let Some(reason) = failure {
                let _ = request
                    .reply
                    .send(Err(anyhow!("relay control connection failed: {reason}")));
            } else if request.deadline <= tokio::time::Instant::now() {
                let _ = request
                    .reply
                    .send(Err(anyhow!("relay control connection timed out")));
            } else {
                let result = self
                    .swarm
                    .listen_on(request.address)
                    .map(|_| ())
                    .map_err(anyhow::Error::from);
                let _ = request.reply.send(result);
            }
        }
    }

    fn expire_operations(&mut self) {
        let now = tokio::time::Instant::now();
        let pending = std::mem::take(&mut self.pending_relay_listens);
        for request in pending {
            if request.reply.is_closed() {
                continue;
            }
            if request.deadline <= now {
                let _ = request
                    .reply
                    .send(Err(anyhow!("relay control connection timed out")));
            } else {
                self.pending_relay_listens.push(request);
            }
        }
        let expired = self
            .pending_provider_queries
            .iter()
            .filter_map(|(id, query)| (query.deadline <= now || query.is_closed()).then_some(*id))
            .collect::<Vec<_>>();
        for id in expired {
            if let Some(mut query) = self.swarm.behaviour_mut().kademlia.query_mut(&id) {
                query.finish();
            }
            if let Some(query) = self.pending_provider_queries.remove(&id) {
                query.complete(Some(anyhow!("provider operation timed out")));
            }
        }
    }

    async fn run(
        swarm: Swarm<CombinedBehaviour>,
        mut cmd_rx: mpsc::Receiver<Command>,
        capabilities: PeerCapabilities,
        local_peer_id: PeerId,
        reachability: ReachabilityConfig,
    ) {
        // SEC-06: channel for spawned relay-handler tasks to hand finished
        // responses back to the driver. Bounded; if it fills, the spawned
        // task awaits — which throttles inbound relay completion, never the
        // swarm loop. Sized to a small multiple of typical concurrency.
        let (relay_reply_tx, mut relay_reply_rx) =
            mpsc::channel::<(ResponseChannel<JobRelayResponse>, JobRelayResponse)>(64);

        let mut driver = Driver {
            swarm,
            capabilities,
            local_peer_id,
            reachability,
            nat_reachability: NatReachability::Unknown,
            observed_addresses: Vec::new(),
            connection_paths: HashMap::new(),
            pending_relay_listens: Vec::new(),
            pending_provider_queries: HashMap::new(),
            pending_rendezvous_registrations: HashMap::new(),
            pending_rendezvous_discoveries: HashMap::new(),
            pending_offers: HashMap::new(),
            pending_get_records: HashMap::new(),
            pending_relays: HashMap::new(),
            job_relay_handler: None,
            relay_reply_tx,
        };

        let mut operation_expiry = tokio::time::interval(Duration::from_millis(200));
        operation_expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = operation_expiry.tick(), if !driver.pending_relay_listens.is_empty()
                    || !driver.pending_provider_queries.is_empty() => {
                    driver.expire_operations();
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(c) => driver.handle_command(c),
                        None => break, // All Discovery handles dropped.
                    }
                }
                // SEC-06: a spawned relay handler finished — ship its response.
                Some((channel, response)) = relay_reply_rx.recv() => {
                    driver.send_relay_response(channel, response);
                }
                event = driver.swarm.next() => {
                    match event {
                        Some(ev) => driver.handle_swarm_event(ev).await,
                        None => break,
                    }
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Listen { addr, reply } => {
                self.listen(addr, reply);
            }
            Command::ListenAddrs { reply } => {
                let _ = reply.send(self.swarm.listeners().cloned().collect());
            }
            Command::ReachabilitySnapshot { reply } => {
                let _ = reply.send(self.build_reachability_snapshot());
            }
            Command::AddExternalAddress { address, reply } => {
                let result = if is_observable_address(&address) {
                    self.swarm.add_external_address(address);
                    Ok(())
                } else {
                    Err(anyhow!("external address is not safely dialable"))
                };
                let _ = reply.send(result);
            }
            Command::RendezvousRegister {
                rendezvous_node,
                namespace,
                ttl_seconds,
                reply,
            } => {
                let result = self
                    .swarm
                    .behaviour_mut()
                    .rendezvous_client
                    .as_mut()
                    .ok_or_else(|| anyhow!("rendezvous client role is disabled"))
                    .and_then(|client| {
                        client
                            .register(namespace.clone(), rendezvous_node, ttl_seconds)
                            .map_err(|error| anyhow!("start rendezvous registration: {error}"))
                    });
                match result {
                    Ok(()) => self
                        .pending_rendezvous_registrations
                        .entry((rendezvous_node, namespace))
                        .or_default()
                        .push(reply),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::RendezvousDiscover {
                rendezvous_node,
                namespace,
                limit,
                reply,
            } => {
                let result = self
                    .swarm
                    .behaviour_mut()
                    .rendezvous_client
                    .as_mut()
                    .ok_or_else(|| anyhow!("rendezvous client role is disabled"))
                    .map(|client| {
                        client.discover(namespace.clone(), None, Some(limit), rendezvous_node);
                    });
                match result {
                    Ok(()) => self
                        .pending_rendezvous_discoveries
                        .entry((rendezvous_node, namespace))
                        .or_default()
                        .push(reply),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::Bootstrap { reply } => {
                let res = match self.swarm.behaviour_mut().kademlia.bootstrap() {
                    Ok(_) => {
                        info!("DHT bootstrap initiated");
                        Ok(())
                    }
                    Err(e) => {
                        warn!("DHT bootstrap failed (normal for standalone nodes): {}", e);
                        warn!("Node will wait for incoming connections or manual peer additions");
                        if !self.reachability.outbound_only {
                            info!("mDNS is active for local network peer discovery");
                        }
                        Ok(()) // Not fatal.
                    }
                };
                let _ = reply.send(res);
            }
            Command::Dial { addr, reply } => {
                let res = (|| -> Result<()> {
                    let multiaddr: Multiaddr =
                        addr.parse().context("Failed to parse peer address")?;
                    self.swarm.dial(multiaddr.clone())?;
                    info!("Dialing peer at: {}", multiaddr);
                    Ok(())
                })();
                let _ = reply.send(res);
            }
            Command::AdvertiseCapabilities { reply } => {
                use libp2p::kad::RecordKey;
                let res = (|| -> Result<()> {
                    // Advertise one record per supported kind. Each is a
                    // (arch, kind_label) tuple so a scheduler can ask the
                    // DHT for "x86_64 + inference" peers in one query.
                    for kind in &self.capabilities.supported_kinds {
                        let kind_label = serde_json::to_string(kind)
                            .ok()
                            .and_then(|s| {
                                // serde_json renders the enum as `"wasm"`;
                                // strip the quotes for the kad key.
                                let trimmed = s.trim_matches('"').to_string();
                                if trimmed.is_empty() {
                                    None
                                } else {
                                    Some(trimmed)
                                }
                            })
                            .unwrap_or_else(|| "unknown".into());

                        let capability_key = format!(
                            "/phase/capability/{}/{}",
                            self.capabilities.arch, kind_label
                        );
                        let key = RecordKey::new(&capability_key.as_bytes());
                        self.swarm
                            .behaviour_mut()
                            .kademlia
                            .start_providing(key)
                            .context("Failed to advertise capabilities")?;
                        info!("Advertising capabilities: {}", capability_key);
                    }
                    Ok(())
                })();
                let _ = reply.send(res);
            }
            Command::DiscoverPeers {
                arch,
                kind_label,
                reply,
            } => {
                use libp2p::kad::RecordKey;
                let capability_key = format!("/phase/capability/{}/{}", arch, kind_label);
                let key = RecordKey::new(&capability_key.as_bytes());
                self.swarm.behaviour_mut().kademlia.get_providers(key);
                info!("Discovering peers with capability: {}", capability_key);
                let _ = reply.send(Ok(()));
            }
            Command::PublishKadRecord { key, value, reply } => {
                use libp2p::kad::{Quorum, Record, RecordKey};
                let res = (|| -> Result<()> {
                    let record_key = RecordKey::new(&key);
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .put_record(Record::new(record_key, value), Quorum::One)
                        .map_err(|e| anyhow!("Failed to publish record: {:?}", e))?;
                    Ok(())
                })();
                let _ = reply.send(res);
            }
            Command::GetKadRecord { key, reply } => {
                use libp2p::kad::RecordKey;
                let record_key = RecordKey::new(&key);
                let query_id = self.swarm.behaviour_mut().kademlia.get_record(record_key);
                self.pending_get_records.insert(
                    query_id,
                    PendingGetRecord {
                        reply,
                        values: Vec::new(),
                        total_bytes: 0,
                        truncated: false,
                    },
                );
            }
            Command::StartProvidingKey { key, reply } => {
                if self.pending_provider_queries.len() >= MAX_PENDING_PROVIDER_QUERIES {
                    let _ = reply.send(Err(anyhow!("provider operation capacity reached")));
                    return;
                }
                let result = validate_kad_record_key(&key).and_then(|()| {
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .start_providing(libp2p::kad::RecordKey::new(&key))
                        .map_err(|error| anyhow!("start provider advertisement: {error}"))
                });
                match result {
                    Ok(query) => {
                        self.pending_provider_queries.insert(
                            query,
                            PendingProviderQuery {
                                reply: ProviderReply::Publish(reply),
                                peers: Vec::new(),
                                deadline: tokio::time::Instant::now() + DISCOVERY_OPERATION_TIMEOUT,
                            },
                        );
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::GetProviderPeers { key, reply } => {
                if let Err(error) = validate_kad_record_key(&key) {
                    let _ = reply.send(Err(error));
                    return;
                }
                if self.pending_provider_queries.len() >= MAX_PENDING_PROVIDER_QUERIES {
                    let _ = reply.send(Err(anyhow!("provider operation capacity reached")));
                    return;
                }
                let query = self
                    .swarm
                    .behaviour_mut()
                    .kademlia
                    .get_providers(libp2p::kad::RecordKey::new(&key));
                self.pending_provider_queries.insert(
                    query,
                    PendingProviderQuery {
                        reply: ProviderReply::Lookup(reply),
                        peers: Vec::new(),
                        deadline: tokio::time::Instant::now() + DISCOVERY_OPERATION_TIMEOUT,
                    },
                );
            }
            Command::SendJobOffer { peer, offer, reply } => {
                let req_id = self
                    .swarm
                    .behaviour_mut()
                    .job_offer
                    .send_request(&peer, offer);
                self.pending_offers.insert(req_id, reply);
            }
            Command::EvaluateOffer { offer, reply } => {
                let response = self.evaluate_offer(offer);
                let _ = reply.send(response);
            }
            Command::SendJobRelay {
                peer,
                request,
                reply,
            } => {
                let req_id = self
                    .swarm
                    .behaviour_mut()
                    .job_relay
                    .send_request(&peer, request);
                self.pending_relays.insert(req_id, reply);
            }
            Command::SetJobRelayHandler { handler } => {
                self.job_relay_handler = handler;
            }
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<CombinedBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Kademlia(kad)) => {
                self.handle_kad_event(kad);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Mdns(mdns_ev)) => {
                self.handle_mdns_event(mdns_ev);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::RelayClient(event)) => {
                self.handle_relay_client_event(event);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::RelayServer(event)) => {
                debug!(?event, "reachability: relay-server event");
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Dcutr(event)) => {
                self.handle_dcutr_event(event);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Identify(event)) => {
                self.handle_identify_event(event);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::AutonatClient(event)) => {
                self.handle_autonat_client_event(event);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::AutonatServer(event)) => {
                debug!(?event, "reachability: AutoNAT-server probe event");
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::RendezvousClient(event)) => {
                self.handle_rendezvous_client_event(event);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::RendezvousServer(event)) => {
                debug!(?event, "reachability: rendezvous-server event");
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::JobOffer(rr)) => {
                self.handle_job_offer_event(rr);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::JobRelay(rr)) => {
                self.handle_job_relay_event(rr);
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on new address: {}", address);
                if address
                    .iter()
                    .any(|part| matches!(part, Protocol::P2pCircuit))
                {
                    info!("Circuit-relay reservation exposed address: {}", address);
                }
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                ..
            } => {
                let path = if endpoint.is_relayed() {
                    ReachabilityPath::Relayed
                } else {
                    ReachabilityPath::Direct
                };
                self.connection_paths
                    .insert(connection_id, ReachabilityConnection { peer_id, path });
                info!(%peer_id, ?path, "Connected to peer");
                if !endpoint.is_relayed() {
                    self.finish_relay_listens(peer_id, None);
                }
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer),
                error,
                ..
            } => {
                // A competing behaviour may skip its dial because the
                // bootstrap control dial is pending. That is not a failure
                // of the control connection we are waiting for.
                if !matches!(error, libp2p::swarm::DialError::DialPeerConditionFalse(_))
                    && !self.has_direct_connection(&peer)
                {
                    self.finish_relay_listens(peer, Some(&error.to_string()));
                }
                debug!(%peer, %error, "outbound connection failed");
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                cause,
                ..
            } => {
                self.connection_paths.remove(&connection_id);
                debug!("Connection closed to {}: {:?}", peer_id, cause);
            }
            SwarmEvent::NewExternalAddrCandidate { address } => {
                self.track_observed_address(address);
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.track_observed_address(address);
                self.nat_reachability = NatReachability::Public;
            }
            SwarmEvent::ExternalAddrExpired { address } => {
                self.observed_addresses
                    .retain(|candidate| candidate != &address);
                if self.swarm.external_addresses().next().is_none() {
                    self.nat_reachability = NatReachability::Unknown;
                }
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                if is_observable_address(&address) {
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, address.clone());
                    self.swarm.add_peer_address(peer_id, address);
                }
            }
            other => {
                debug!("Other swarm event: {:?}", other);
            }
        }
    }

    fn handle_relay_client_event(&mut self, event: relay::client::Event) {
        match event {
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal,
                limit,
            } => {
                info!(%relay_peer_id, renewal, ?limit, "reachability: relay reservation accepted");
            }
            relay::client::Event::OutboundCircuitEstablished {
                relay_peer_id,
                limit,
            } => {
                info!(%relay_peer_id, ?limit, "reachability: outbound relay circuit established");
            }
            relay::client::Event::InboundCircuitEstablished { src_peer_id, limit } => {
                info!(%src_peer_id, ?limit, "reachability: inbound relay circuit established");
            }
        }
    }

    fn handle_dcutr_event(&mut self, event: dcutr::Event) {
        match event.result {
            Ok(connection_id) => {
                self.connection_paths.insert(
                    connection_id,
                    ReachabilityConnection {
                        peer_id: event.remote_peer_id,
                        path: ReachabilityPath::Dcutr,
                    },
                );
                info!(
                    peer_id = %event.remote_peer_id,
                    ?connection_id,
                    "reachability: DCUtR established a direct connection"
                );
            }
            Err(error) => {
                debug!(
                    peer_id = %event.remote_peer_id,
                    %error,
                    "reachability: DCUtR did not establish a direct connection"
                );
            }
        }
    }

    fn handle_identify_event(&mut self, event: identify::Event) {
        match event {
            identify::Event::Received { peer_id, info, .. } => {
                self.track_observed_address(info.observed_addr);
                for address in info.listen_addrs {
                    if !is_observable_address(&address) {
                        continue;
                    }
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, address.clone());
                    self.swarm.add_peer_address(peer_id, address);
                }
            }
            identify::Event::Error { peer_id, error, .. } => {
                debug!(%peer_id, %error, "reachability: identify exchange failed");
            }
            identify::Event::Sent { .. } | identify::Event::Pushed { .. } => {}
        }
    }

    fn handle_autonat_client_event(&mut self, event: autonat::v2::client::Event) {
        let autonat::v2::client::Event {
            tested_addr,
            server,
            result,
            ..
        } = event;
        self.track_observed_address(tested_addr.clone());
        match result {
            Ok(()) => {
                self.nat_reachability = NatReachability::Public;
                info!(%server, address = %tested_addr, "reachability: AutoNAT dial-back succeeded");
            }
            Err(error) => {
                if self.nat_reachability != NatReachability::Public {
                    self.nat_reachability = NatReachability::Private;
                }
                debug!(%server, address = %tested_addr, %error, "reachability: AutoNAT dial-back failed");
            }
        }
    }

    fn handle_rendezvous_client_event(&mut self, event: rendezvous::client::Event) {
        match event {
            rendezvous::client::Event::Registered {
                rendezvous_node,
                ttl,
                namespace,
            } => {
                if let Some(waiters) = self
                    .pending_rendezvous_registrations
                    .remove(&(rendezvous_node, namespace.clone()))
                {
                    for waiter in waiters {
                        let _ = waiter.send(Ok(()));
                    }
                }
                info!(%rendezvous_node, %namespace, ttl, "reachability: rendezvous registration accepted");
            }
            rendezvous::client::Event::RegisterFailed {
                rendezvous_node,
                namespace,
                error,
            } => {
                if let Some(waiters) = self
                    .pending_rendezvous_registrations
                    .remove(&(rendezvous_node, namespace.clone()))
                {
                    let message = format!("rendezvous registration failed: {error:?}");
                    for waiter in waiters {
                        let _ = waiter.send(Err(anyhow!(message.clone())));
                    }
                }
                warn!(%rendezvous_node, %namespace, ?error, "reachability: rendezvous registration failed");
            }
            rendezvous::client::Event::Discovered {
                rendezvous_node,
                registrations,
                cookie,
            } => {
                let namespace = cookie.namespace().cloned();
                let peers = registrations
                    .into_iter()
                    .map(|registration| {
                        let peer_id = registration.record.peer_id();
                        let addresses: Vec<_> = registration
                            .record
                            .addresses()
                            .iter()
                            .filter(|address| is_observable_address(address))
                            .cloned()
                            .collect();
                        for address in &addresses {
                            self.swarm
                                .behaviour_mut()
                                .kademlia
                                .add_address(&peer_id, address.clone());
                            self.swarm.add_peer_address(peer_id, address.clone());
                        }
                        RendezvousPeer {
                            namespace: registration.namespace.to_string(),
                            peer_id,
                            addresses,
                            ttl_seconds: registration.ttl,
                        }
                    })
                    .collect::<Vec<_>>();
                if let Some(waiters) = self
                    .pending_rendezvous_discoveries
                    .remove(&(rendezvous_node, namespace))
                {
                    for waiter in waiters {
                        let _ = waiter.send(Ok(peers.clone()));
                    }
                }
            }
            rendezvous::client::Event::DiscoverFailed {
                rendezvous_node,
                namespace,
                error,
            } => {
                if let Some(waiters) = self
                    .pending_rendezvous_discoveries
                    .remove(&(rendezvous_node, namespace.clone()))
                {
                    let message = format!("rendezvous discovery failed: {error:?}");
                    for waiter in waiters {
                        let _ = waiter.send(Err(anyhow!(message.clone())));
                    }
                }
                warn!(%rendezvous_node, ?namespace, ?error, "reachability: rendezvous discovery failed");
            }
            rendezvous::client::Event::Expired { peer } => {
                debug!(%peer, "reachability: rendezvous registration expired");
            }
        }
    }

    fn track_observed_address(&mut self, address: Multiaddr) {
        if !is_observable_address(&address) || self.observed_addresses.contains(&address) {
            return;
        }
        if self.observed_addresses.len() == MAX_TRACKED_REACHABILITY_ADDRESSES {
            self.observed_addresses.remove(0);
        }
        self.observed_addresses.push(address);
    }

    fn build_reachability_snapshot(&self) -> ReachabilitySnapshot {
        let mut listen_addresses = self.swarm.listeners().cloned().collect::<Vec<_>>();
        let mut observed_addresses = self.observed_addresses.clone();
        let mut external_addresses = self.swarm.external_addresses().cloned().collect::<Vec<_>>();
        listen_addresses.sort_by_key(ToString::to_string);
        observed_addresses.sort_by_key(ToString::to_string);
        external_addresses.sort_by_key(ToString::to_string);

        let mut connections = self.connection_paths.values().cloned().collect::<Vec<_>>();
        connections.sort_by(|left, right| {
            left.peer_id
                .to_string()
                .cmp(&right.peer_id.to_string())
                .then_with(|| {
                    reachability_path_rank(left.path).cmp(&reachability_path_rank(right.path))
                })
        });
        let active_path = connections
            .iter()
            .map(|connection| connection.path)
            .max_by_key(|path| reachability_path_rank(*path))
            .unwrap_or(ReachabilityPath::Unknown);

        ReachabilitySnapshot {
            role: self.reachability.role,
            outbound_only: self.reachability.outbound_only,
            active_path,
            nat: self.nat_reachability,
            listen_addresses,
            observed_addresses,
            external_addresses,
            connections,
        }
    }

    fn handle_mdns_event(&mut self, event: mdns::Event) {
        match event {
            mdns::Event::Discovered(list) => {
                for (peer_id, multiaddr) in list {
                    info!("mDNS discovered peer: {} at {}", peer_id, multiaddr);
                    // Register the discovered address for Kademlia (its own
                    // routing table) and via Swarm::add_peer_address so the
                    // request-response behaviour can dial without a prior
                    // explicit connection. libp2p 0.56 moved per-behaviour
                    // add_address onto the Swarm API; the older method is
                    // deprecated but still works for Kademlia's own table.
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, multiaddr.clone());
                    self.swarm.add_peer_address(peer_id, multiaddr);
                }
            }
            mdns::Event::Expired(list) => {
                for (peer_id, multiaddr) in list {
                    debug!("mDNS peer expired: {} at {}", peer_id, multiaddr);
                }
            }
        }
    }

    fn handle_kad_event(&mut self, event: KademliaEvent) {
        match event {
            KademliaEvent::OutboundQueryProgressed {
                id, result, step, ..
            } => {
                // Match against any in-flight `GetKadRecord` query. libp2p
                // emits one event per peer that responds plus a terminal
                // event with `step.last == true` — we fold values until the
                // terminator and only then reply to the awaiting caller.
                if let QueryResult::GetRecord(res) = result {
                    if let Some(pending) = self.pending_get_records.get_mut(&id) {
                        match res {
                            Ok(GetRecordOk::FoundRecord(rec)) => {
                                let was_truncated = pending.truncated;
                                pending.insert_bounded(rec.record.value);
                                if pending.truncated && !was_truncated {
                                    warn!(
                                        query = ?id,
                                        max_values = MAX_KAD_RECORD_VALUES_PER_QUERY,
                                        max_bytes = MAX_KAD_RECORD_BYTES_PER_QUERY,
                                        "Kademlia record query reached a transport allocation bound; extra values are dropped"
                                    );
                                }
                            }
                            Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {
                                // No new records; terminator handled below.
                            }
                            Err(e) => {
                                // `NotFound` is a normal miss, not an
                                // error — surface every other case as a
                                // log line; the caller still gets whatever
                                // values we accumulated so far.
                                debug!("get_record query {:?} returned error: {:?}", id, e);
                            }
                        }
                        if step.last {
                            // Take and reply once the iterative query has
                            // exhausted its candidates.
                            if let Some(p) = self.pending_get_records.remove(&id) {
                                let _ = p.reply.send(Ok(p.values));
                            }
                        }
                    } else {
                        debug!("get_record event for unknown query id {:?}", id);
                    }
                } else if let QueryResult::GetProviders(result) = result {
                    let mut error = None;
                    let mut complete = step.last;
                    if let Some(pending) = self.pending_provider_queries.get_mut(&id) {
                        match result {
                            Ok(GetProvidersOk::FoundProviders { providers, .. }) => {
                                for peer in providers {
                                    pending.insert(peer);
                                }
                                complete |= pending.peers.len() == MAX_PROVIDER_PEERS_PER_QUERY;
                            }
                            Ok(GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {}
                            Err(failure) => {
                                error = Some(anyhow!("provider lookup failed: {failure}"));
                                complete = true;
                            }
                        }
                    }
                    if complete {
                        if let Some(mut query) = self.swarm.behaviour_mut().kademlia.query_mut(&id)
                        {
                            query.finish();
                        }
                        if let Some(pending) = self.pending_provider_queries.remove(&id) {
                            pending.complete(error);
                        }
                    }
                } else if let QueryResult::StartProviding(result) = result {
                    if let Some(pending) = self.pending_provider_queries.remove(&id) {
                        pending.complete(
                            result
                                .err()
                                .map(|error| anyhow!("provider publication failed: {error}")),
                        );
                    }
                } else {
                    debug!("Outbound query result: {:?}", result);
                }
            }
            KademliaEvent::RoutingUpdated { peer, .. } => {
                debug!("Routing table updated with peer: {}", peer);
            }
            KademliaEvent::UnroutablePeer { peer } => {
                warn!("Unroutable peer: {}", peer);
            }
            KademliaEvent::RoutablePeer { peer, address } => {
                info!("Discovered routable peer: {} at {}", peer, address);
            }
            KademliaEvent::PendingRoutablePeer { peer, address } => {
                debug!("Pending routable peer: {} at {}", peer, address);
            }
            _ => {}
        }
    }

    fn handle_job_offer_event(&mut self, event: request_response::Event<JobOffer, JobResponse>) {
        use request_response::{Event, Message};
        match event {
            Event::Message { message, .. } => match message {
                Message::Request {
                    request, channel, ..
                } => {
                    let response = self.evaluate_offer(request);
                    self.send_offer_response(channel, response);
                }
                Message::Response {
                    request_id,
                    response,
                } => {
                    if let Some(tx) = self.pending_offers.remove(&request_id) {
                        let _ = tx.send(Ok(response));
                    }
                }
            },
            Event::OutboundFailure {
                request_id, error, ..
            } => {
                if let Some(tx) = self.pending_offers.remove(&request_id) {
                    let _ = tx.send(Err(anyhow!("JobOffer outbound failure: {:?}", error)));
                }
            }
            Event::InboundFailure { error, .. } => {
                warn!("JobOffer inbound failure: {:?}", error);
            }
            Event::ResponseSent { .. } => {
                // Response delivery confirmed; nothing to do.
            }
        }
    }

    fn send_offer_response(
        &mut self,
        channel: ResponseChannel<JobResponse>,
        response: JobResponse,
    ) {
        if self
            .swarm
            .behaviour_mut()
            .job_offer
            .send_response(channel, response)
            .is_err()
        {
            warn!("Failed to send JobResponse — peer connection lost?");
        }
    }

    /// SEC-06: ship a finished relay response over its `ResponseChannel`.
    /// Called both inline (no-handler refusal) and from the `relay_reply_rx`
    /// select arm (spawned handler completion).
    fn send_relay_response(
        &mut self,
        channel: ResponseChannel<JobRelayResponse>,
        response: JobRelayResponse,
    ) {
        if self
            .swarm
            .behaviour_mut()
            .job_relay
            .send_response(channel, response)
            .is_err()
        {
            warn!("Failed to send JobRelayResponse — peer connection lost?");
        }
    }

    /// SEC-06: handle a relay event WITHOUT blocking the swarm loop.
    ///
    /// The inbound `Request` branch no longer `await`s the handler inline.
    /// Instead it `tokio::spawn`s the (potentially slow, GPU-heavy) handler
    /// and routes the finished response back through `relay_reply_tx`, so the
    /// driver keeps polling other peers' events while a job runs. The
    /// delivering peer's `PeerId` is threaded into the handler for SEC-01's
    /// PeerID-bind authz path.
    fn handle_job_relay_event(
        &mut self,
        event: request_response::Event<JobRelayRequest, JobRelayResponse>,
    ) {
        use request_response::{Event, Message};
        match event {
            Event::Message { peer, message, .. } => match message {
                Message::Request {
                    request, channel, ..
                } => {
                    let reply_tx = self.relay_reply_tx.clone();
                    match self.job_relay_handler.clone() {
                        Some(handler) => {
                            // Off-driver: spawn the handler so a slow job
                            // can't stall the swarm event loop.
                            tokio::spawn(async move {
                                let response = handler(peer, request.payload).await;
                                let _ = reply_tx.send((channel, response)).await;
                            });
                        }
                        None => {
                            // No handler installed → refuse closed. Send
                            // inline (cheap, no await on user code).
                            self.send_relay_response(
                                channel,
                                JobRelayResponse::Err {
                                    reason: "no job-relay handler installed".to_string(),
                                },
                            );
                        }
                    }
                }
                Message::Response {
                    request_id,
                    response,
                } => {
                    if let Some(tx) = self.pending_relays.remove(&request_id) {
                        let _ = tx.send(Ok(response));
                    }
                }
            },
            Event::OutboundFailure {
                request_id, error, ..
            } => {
                if let Some(tx) = self.pending_relays.remove(&request_id) {
                    let _ = tx.send(Err(anyhow!("JobRelay outbound failure: {:?}", error)));
                }
            }
            Event::InboundFailure { error, .. } => {
                warn!("JobRelay inbound failure: {:?}", error);
            }
            Event::ResponseSent { .. } => {}
        }
    }

    /// Match a JobOffer against this node's capabilities. Same contract as
    /// the pre-M2 `Discovery::handle_job_offer`, with the wasm-runtime
    /// string mapped through to `JobSpecKind::Wasm`.
    fn evaluate_offer(&self, offer: JobOffer) -> JobResponse {
        info!(
            "Received job offer: {} (module: {})",
            offer.job_id, offer.module_hash
        );

        // Architecture check.
        if offer.requirements.arch != self.capabilities.arch {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::ArchMismatch {
                    required: offer.requirements.arch,
                    available: self.capabilities.arch.clone(),
                },
            };
        }

        // Workload-kind check. The wire still carries the legacy
        // `wasm_runtime` string from the November 2025 MVP — `"wasmtime-27"`
        // and the like — so phase-net maps any runtime that starts with a
        // recognised prefix onto the corresponding `JobSpecKind` and checks
        // that this node advertises support for it. Unrecognised runtimes
        // come back as `None` and are rejected, which is conservative.
        let requested_kind = classify_runtime(&offer.requirements.wasm_runtime);
        match requested_kind {
            Some(kind) if !self.capabilities.supported_kinds.contains(&kind) => {
                return JobResponse::Rejected {
                    job_id: offer.job_id,
                    reason: RejectionReason::RuntimeNotSupported {
                        required: offer.requirements.wasm_runtime,
                    },
                };
            }
            None => {
                return JobResponse::Rejected {
                    job_id: offer.job_id,
                    reason: RejectionReason::RuntimeNotSupported {
                        required: offer.requirements.wasm_runtime,
                    },
                };
            }
            _ => {}
        }

        // CPU check.
        if offer.requirements.cpu_cores > self.capabilities.cpu_count {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::InsufficientResources {
                    missing: format!(
                        "CPU: need {}, have {}",
                        offer.requirements.cpu_cores, self.capabilities.cpu_count
                    ),
                },
            };
        }

        // Memory check.
        if offer.requirements.memory_mb > self.capabilities.memory_mb {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::InsufficientResources {
                    missing: format!(
                        "Memory: need {} MB, have {} MB",
                        offer.requirements.memory_mb, self.capabilities.memory_mb
                    ),
                },
            };
        }

        let estimated_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        JobResponse::Accepted {
            job_id: offer.job_id,
            estimated_start,
            node_peer_id: self.local_peer_id.to_string(),
        }
    }
}

/// Map a wire `wasm_runtime` string onto a `JobSpecKind`. Conservative —
/// anything we don't recognise comes back as `None`, which the caller
/// treats as `RuntimeNotSupported`.
///
/// This bridge keeps the November 2025 wire format intact while letting
/// `PeerCapabilities` advertise capabilities in the new workload-agnostic
/// `JobSpecKind` vocabulary.
fn classify_runtime(runtime: &str) -> Option<phase_protocol::JobSpecKind> {
    use phase_protocol::JobSpecKind;
    let prefix = runtime.split('-').next().unwrap_or("");
    match prefix {
        "wasmtime" | "wasm" | "wasm3" => Some(JobSpecKind::Wasm),
        "llama" | "mlx" | "ollama" | "inference" => Some(JobSpecKind::Inference),
        _ => None,
    }
}

fn parse_rendezvous_namespace(namespace: &str) -> Result<rendezvous::Namespace> {
    if namespace.is_empty() {
        return Err(anyhow!("rendezvous namespace cannot be empty"));
    }
    rendezvous::Namespace::new(namespace.to_owned()).map_err(|error| anyhow!(error))
}

fn reachability_path_rank(path: ReachabilityPath) -> u8 {
    match path {
        ReachabilityPath::Unknown => 0,
        ReachabilityPath::Relayed => 1,
        ReachabilityPath::Direct => 2,
        ReachabilityPath::Dcutr => 3,
    }
}

/// Conservatively accept only address shapes this swarm can actually dial.
/// Loopback and private IPs remain valid for LAN peers and deterministic tests;
/// unspecified, multicast, memory-only, zero-port, and transport-less addresses
/// are never advertised as reachability candidates.
fn is_observable_address(address: &Multiaddr) -> bool {
    let mut has_host = false;
    let mut has_tcp = false;
    let mut has_udp = false;
    let mut has_quic = false;
    let mut has_circuit = false;

    for protocol in address.iter() {
        match protocol {
            Protocol::Ip4(ip) => {
                if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
                    return false;
                }
                has_host = true;
            }
            Protocol::Ip6(ip) => {
                if ip.is_unspecified() || ip.is_multicast() {
                    return false;
                }
                has_host = true;
            }
            Protocol::Dns(name) | Protocol::Dns4(name) | Protocol::Dns6(name) => {
                if name.is_empty() {
                    return false;
                }
                has_host = true;
            }
            Protocol::Tcp(port) => {
                if port == 0 {
                    return false;
                }
                has_tcp = true;
            }
            Protocol::Udp(port) => {
                if port == 0 {
                    return false;
                }
                has_udp = true;
            }
            Protocol::Quic | Protocol::QuicV1 => has_quic = true,
            Protocol::P2pCircuit => has_circuit = true,
            Protocol::Memory(_) | Protocol::Unix(_) => return false,
            _ => {}
        }
    }

    has_host && (has_tcp || (has_udp && has_quic) || has_circuit)
}

// ---------------------------------------------------------------------------
// hex shim — phase-net doesn't want a direct hex dependency, but ed25519-dalek
// produces raw bytes we need to surface in hex for the daemon's "node public
// key" log line. This tiny encoder avoids the extra crate.
// ---------------------------------------------------------------------------
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = bytes.as_ref();
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
mod tests;
