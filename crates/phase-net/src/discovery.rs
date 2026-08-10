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
        store::MemoryStore, Behaviour as KademliaBehaviour, Event as KademliaEvent, GetRecordOk,
        Mode as KademliaMode, QueryId, QueryResult,
    },
    mdns,
    multiaddr::Protocol,
    relay, rendezvous,
    request_response::{self, cbor, json, OutboundRequestId, ProtocolSupport, ResponseChannel},
    swarm::{behaviour::toggle::Toggle, ConnectionId, NetworkBehaviour, SwarmEvent},
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
    JobResponse, RejectionReason, BLOB_STREAM_MAX_CHUNK_BYTES, BLOB_STREAM_MAX_METADATA_BYTES,
    BLOB_STREAM_PROTOCOL, JOB_RELAY_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
    JOB_RELAY_STREAM_MAX_EVENT_BYTES, JOB_RELAY_STREAM_MAX_OPEN_BYTES, JOB_RELAY_STREAM_PROTOCOL,
    JOB_RELAY_STREAM_SCHEMA_VERSION,
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

/// Combined network behaviour: Kademlia DHT + mDNS local discovery +
/// JSON-coded request/response for JobOffer.
#[derive(NetworkBehaviour)]
struct CombinedBehaviour {
    /// Hard swarm-level bounds apply before any application protocol is
    /// negotiated. This protects rendezvous/relay deployments as well as the
    /// LUCID-specific substreams below.
    connection_limits: connection_limits::Behaviour,
    kademlia: KademliaBehaviour<MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    relay_client: Toggle<relay::client::Behaviour>,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: Toggle<dcutr::Behaviour>,
    identify: Toggle<identify::Behaviour>,
    autonat_client: Toggle<autonat::v2::client::Behaviour>,
    autonat_server: Toggle<autonat::v2::server::Behaviour>,
    rendezvous_client: Toggle<rendezvous::client::Behaviour>,
    rendezvous_server: Toggle<rendezvous::server::Behaviour>,
    job_offer: json::Behaviour<JobOffer, JobResponse>,
    /// LUCID M5 peer-relay request/response. CBOR-encoded so the binary
    /// payload (bincode `SignedManifest<JobSpec>` / `Vec<JobEvent>`) doesn't
    /// suffer the 4-5× blow-up JSON's u8 arrays cause.
    job_relay: cbor::Behaviour<JobRelayRequest, JobRelayResponse>,
    /// Distinct v2 bidirectional substreams. The behaviour only negotiates
    /// and opens streams; Phase's bounded frame codec and state machine live
    /// in this module and `protocol.rs`.
    job_relay_stream: libp2p_stream::Behaviour,
}

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
                let mdns_behaviour = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

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
                let dcutr = reachability
                    .dcutr
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
                let autonat_client = reachability
                    .autonat_client
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
                    mdns: mdns_behaviour,
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
        let job_relay_stream_handler: Arc<RwLock<Option<JobRelayStreamHandler>>> =
            Arc::new(RwLock::new(None));
        let blob_stream_handler: Arc<RwLock<Option<BlobStreamHandler>>> =
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
            pending_rendezvous_registrations: HashMap::new(),
            pending_rendezvous_discoveries: HashMap::new(),
            pending_offers: HashMap::new(),
            pending_get_records: HashMap::new(),
            pending_relays: HashMap::new(),
            job_relay_handler: None,
            relay_reply_tx,
        };

        loop {
            tokio::select! {
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
                let res = (|| -> Result<()> {
                    let listen_addr: Multiaddr =
                        addr.parse().context("Failed to parse listen address")?;
                    self.swarm.listen_on(listen_addr.clone())?;
                    info!("Listening on: {}", listen_addr);
                    Ok(())
                })();
                let _ = reply.send(res);
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
                        info!("mDNS is active for local network peer discovery");
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
mod tests {
    use super::*;

    #[test]
    fn inbound_stream_admission_is_combined_global_and_per_peer_bounded() {
        let admission = InboundStreamAdmission::new(2, 1);
        let peer_a = PeerId::random();
        let peer_b = PeerId::random();
        let peer_c = PeerId::random();

        let a = admission.try_acquire(peer_a).expect("first peer admitted");
        assert!(
            admission.try_acquire(peer_a).is_none(),
            "same peer cannot exceed its cross-protocol stream budget"
        );
        let b = admission.try_acquire(peer_b).expect("second peer admitted");
        assert!(
            admission.try_acquire(peer_c).is_none(),
            "relay and blob streams share the same global budget"
        );

        drop(a);
        let c = admission
            .try_acquire(peer_c)
            .expect("dropping a task releases global and per-peer state");
        drop((b, c));
        assert!(admission.per_peer.lock().unwrap().is_empty());
        assert_eq!(admission.global.available_permits(), 2);
    }

    #[tokio::test]
    async fn blob_bandwidth_budget_is_shared_across_same_peer_streams() {
        let admission = InboundStreamAdmission::new(4, 2);
        let peer = PeerId::random();
        let first = admission.try_acquire(peer).expect("first blob stream");
        let second = admission.try_acquire(peer).expect("second blob stream");

        first
            .wait_for_blob_budget(BLOB_STREAM_MAX_CHUNK_BYTES)
            .await;
        second
            .wait_for_blob_budget(BLOB_STREAM_MAX_CHUNK_BYTES)
            .await;

        let windows = admission.blob_bandwidth.lock().expect("bandwidth state");
        let window = windows.get(&peer).expect("shared peer window");
        assert_eq!(window.bytes, BLOB_STREAM_MAX_CHUNK_BYTES * 2);
        drop(windows);
        drop((first, second));
        assert!(admission.blob_bandwidth.lock().unwrap().is_empty());
    }

    struct PendingAfterBytes {
        bytes: Vec<u8>,
        offset: usize,
    }

    impl PendingAfterBytes {
        fn silent() -> Self {
            Self {
                bytes: Vec::new(),
                offset: 0,
            }
        }

        fn with_frame(frame: &JobRelayStreamFrame) -> Self {
            let encoded = postcard::to_stdvec(frame).expect("encode test frame");
            let mut bytes = Vec::with_capacity(4 + encoded.len());
            bytes.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&encoded);
            Self { bytes, offset: 0 }
        }
    }

    impl AsyncRead for PendingAfterBytes {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            output: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.offset == self.bytes.len() {
                return std::task::Poll::Pending;
            }
            let count = output.len().min(self.bytes.len() - self.offset);
            output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            std::task::Poll::Ready(Ok(count))
        }
    }

    #[derive(Clone)]
    struct RecordingWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl AsyncWrite for RecordingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn recording_writer() -> (RecordingWriter, Arc<std::sync::Mutex<Vec<u8>>>) {
        let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
        (RecordingWriter(bytes.clone()), bytes)
    }

    fn recorded_control(bytes: &Arc<std::sync::Mutex<Vec<u8>>>) -> JobRelayStreamControl {
        let bytes = bytes.lock().unwrap();
        assert!(bytes.len() >= 4, "recorded control has a length prefix");
        let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), 4 + length);
        postcard::from_bytes(&bytes[4..]).expect("decode recorded control")
    }

    #[tokio::test]
    async fn outbound_live_relay_silence_fails_and_sends_cancel() {
        let reader = PendingAfterBytes::silent();
        let (writer, recorded) = recording_writer();
        let (frames_tx, mut frames_rx) = mpsc::channel(2);
        let (_controls_tx, controls_rx) = mpsc::channel(1);
        let job_id = [0x51; 32];

        let task = tokio::spawn(drive_outbound_job_relay_stream(
            reader,
            writer,
            job_id,
            frames_tx,
            controls_rx,
            Duration::from_millis(30),
            tokio::time::Instant::now() + Duration::from_secs(1),
        ));

        let failure = tokio::time::timeout(Duration::from_secs(1), frames_rx.recv())
            .await
            .expect("silent peer must fail within its idle budget")
            .expect("driver must report a terminal error")
            .expect_err("silence cannot be a successful EOF");
        assert_eq!(failure, "live relay idle timeout reached");

        task.await.expect("driver exits and releases its resources");
        let cancel = recorded_control(&recorded);
        assert_eq!(cancel.job_id, job_id);
        assert!(matches!(
            cancel.kind,
            JobRelayStreamControlKind::Cancel { ref reason }
                if reason == "live relay idle timeout reached"
        ));
    }

    #[tokio::test]
    async fn outbound_live_relay_total_deadline_wins_over_idle_budget() {
        let reader = PendingAfterBytes::silent();
        let (writer, recorded) = recording_writer();
        let (frames_tx, mut frames_rx) = mpsc::channel(2);
        let (_controls_tx, controls_rx) = mpsc::channel(1);
        let job_id = [0x52; 32];

        let task = tokio::spawn(drive_outbound_job_relay_stream(
            reader,
            writer,
            job_id,
            frames_tx,
            controls_rx,
            Duration::from_secs(1),
            tokio::time::Instant::now() + Duration::from_millis(30),
        ));

        let failure = tokio::time::timeout(Duration::from_secs(1), frames_rx.recv())
            .await
            .expect("silent peer must fail at the total deadline")
            .expect("driver must report a terminal error")
            .expect_err("deadline cannot be a successful EOF");
        assert_eq!(failure, "live relay deadline reached");

        task.await.expect("driver exits and releases its resources");
        let cancel = recorded_control(&recorded);
        assert!(matches!(
            cancel.kind,
            JobRelayStreamControlKind::Cancel { ref reason }
                if reason == "live relay deadline reached"
        ));
    }

    #[tokio::test]
    async fn outbound_live_relay_enforces_idle_timeout_after_acceptance() {
        let (frames_tx, mut frames_rx) = mpsc::channel(2);
        let (_controls_tx, controls_rx) = mpsc::channel(1);
        let job_id = [0x53; 32];
        let reader = PendingAfterBytes::with_frame(&JobRelayStreamFrame {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id,
            sequence: 0,
            kind: JobRelayStreamFrameKind::Accepted,
        });
        let (writer, recorded) = recording_writer();

        let task = tokio::spawn(drive_outbound_job_relay_stream(
            reader,
            writer,
            job_id,
            frames_tx,
            controls_rx,
            Duration::from_millis(30),
            tokio::time::Instant::now() + Duration::from_secs(1),
        ));

        let accepted = frames_rx
            .recv()
            .await
            .expect("accepted frame")
            .expect("valid accepted frame");
        assert!(matches!(accepted.kind, JobRelayStreamFrameKind::Accepted));
        let failure = tokio::time::timeout(Duration::from_secs(1), frames_rx.recv())
            .await
            .expect("accepted-but-silent peer must hit idle timeout")
            .expect("driver reports timeout")
            .expect_err("silence after acceptance cannot succeed");
        assert_eq!(failure, "live relay idle timeout reached");

        task.await.expect("driver exits");
        let cancel = recorded_control(&recorded);
        assert!(matches!(
            cancel.kind,
            JobRelayStreamControlKind::Cancel { .. }
        ));
    }

    #[tokio::test]
    async fn outbound_live_relay_enforces_total_deadline_after_acceptance() {
        let (frames_tx, mut frames_rx) = mpsc::channel(2);
        let (_controls_tx, controls_rx) = mpsc::channel(1);
        let job_id = [0x54; 32];
        let reader = PendingAfterBytes::with_frame(&JobRelayStreamFrame {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id,
            sequence: 0,
            kind: JobRelayStreamFrameKind::Accepted,
        });
        let (writer, recorded) = recording_writer();

        let task = tokio::spawn(drive_outbound_job_relay_stream(
            reader,
            writer,
            job_id,
            frames_tx,
            controls_rx,
            Duration::from_secs(1),
            tokio::time::Instant::now() + Duration::from_millis(30),
        ));

        let accepted = frames_rx
            .recv()
            .await
            .expect("accepted frame")
            .expect("valid accepted frame");
        assert!(matches!(accepted.kind, JobRelayStreamFrameKind::Accepted));
        let failure = tokio::time::timeout(Duration::from_secs(1), frames_rx.recv())
            .await
            .expect("accepted peer must hit the total deadline")
            .expect("driver reports deadline")
            .expect_err("post-acceptance deadline cannot succeed");
        assert_eq!(failure, "live relay deadline reached");

        task.await.expect("driver exits");
        let cancel = recorded_control(&recorded);
        assert!(matches!(
            cancel.kind,
            JobRelayStreamControlKind::Cancel { ref reason }
                if reason == "live relay deadline reached"
        ));
    }

    #[test]
    fn kad_record_accumulator_bounds_count_bytes_values_and_keys() {
        assert!(validate_kad_record_key(&[]).is_err());
        assert!(validate_kad_record_key(&vec![0; MAX_KAD_RECORD_KEY_BYTES + 1]).is_err());
        assert!(validate_kad_record_key(&[1]).is_ok());

        let (reply, _receiver) = oneshot::channel();
        let mut by_count = PendingGetRecord {
            reply,
            values: Vec::new(),
            total_bytes: 0,
            truncated: false,
        };
        for index in 0..=MAX_KAD_RECORD_VALUES_PER_QUERY {
            by_count.insert_bounded((index as u32).to_be_bytes().to_vec());
        }
        assert_eq!(by_count.values.len(), MAX_KAD_RECORD_VALUES_PER_QUERY);
        assert!(by_count.truncated);

        let (reply, _receiver) = oneshot::channel();
        let mut by_bytes = PendingGetRecord {
            reply,
            values: Vec::new(),
            total_bytes: 0,
            truncated: false,
        };
        for index in 0..=MAX_KAD_RECORD_BYTES_PER_QUERY / MAX_KAD_RECORD_VALUE_BYTES {
            let mut value = vec![0_u8; MAX_KAD_RECORD_VALUE_BYTES];
            value[..8].copy_from_slice(&(index as u64).to_be_bytes());
            by_bytes.insert_bounded(value);
        }
        assert_eq!(by_bytes.total_bytes, MAX_KAD_RECORD_BYTES_PER_QUERY);
        assert!(by_bytes.truncated);

        let (reply, _receiver) = oneshot::channel();
        let mut oversized = PendingGetRecord {
            reply,
            values: Vec::new(),
            total_bytes: 0,
            truncated: false,
        };
        oversized.insert_bounded(vec![0; MAX_KAD_RECORD_VALUE_BYTES + 1]);
        assert!(oversized.values.is_empty());
        assert!(oversized.truncated);
    }

    async fn loopback_dial_addr(discovery: &Discovery) -> Multiaddr {
        for _ in 0..50 {
            if let Some(addr) = discovery.listen_addrs().await.unwrap().into_iter().next() {
                return addr;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("swarm never reported its loopback listen address")
    }

    async fn wait_for_peer_path(
        discovery: &Discovery,
        peer_id: PeerId,
        accepted: &[ReachabilityPath],
    ) -> ReachabilitySnapshot {
        for _ in 0..250 {
            let snapshot = discovery.reachability_snapshot().await.unwrap();
            if snapshot.connections.iter().any(|connection| {
                connection.peer_id == peer_id && accepted.contains(&connection.path)
            }) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("peer {peer_id} never reached one of the expected paths: {accepted:?}")
    }

    /// The loopback integration tests each build several independent
    /// multi-threaded swarms. Running all of them concurrently on a bounded
    /// four-CPU service creates artificial protocol-negotiation starvation
    /// that cannot occur within one daemon. Serialize only these heavyweight
    /// tests; unit tests remain fully parallel.
    async fn acquire_loopback_test_gate() -> tokio::sync::SemaphorePermit<'static> {
        static GATE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
        GATE.get_or_init(|| tokio::sync::Semaphore::new(1))
            .acquire()
            .await
            .expect("loopback test gate remains open")
    }

    fn blob_request(
        content_id: [u8; 32],
        offset: u64,
        metadata: impl Into<Vec<u8>>,
    ) -> BlobStreamRequest {
        BlobStreamRequest {
            schema_version: crate::protocol::BLOB_STREAM_SCHEMA_VERSION,
            content_id,
            offset,
            deadline_unix_ms: unix_time_ms() + 5_000,
            idle_timeout_ms: 1_000,
            metadata: metadata.into(),
        }
    }

    fn blob_frame(content_id: [u8; 32], kind: BlobStreamFrameKind) -> BlobStreamFrame {
        BlobStreamFrame {
            schema_version: crate::protocol::BLOB_STREAM_SCHEMA_VERSION,
            content_id,
            kind,
        }
    }

    #[tokio::test]
    async fn test_discovery_creation() {
        let config = DiscoveryConfig::default();
        let discovery = Discovery::new(config);

        // mDNS may fail in restricted test environments due to netlink
        // permissions. That's expected; the daemon still works in production.
        match discovery {
            Ok(_) => {
                // Success — full functionality available.
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);
                if error_msg.contains("Permission denied") {
                    eprintln!("Note: mDNS disabled in test (needs network permissions)");
                } else {
                    panic!("Unexpected error creating discovery: {:?}", e);
                }
            }
        }
    }

    #[test]
    fn classify_runtime_recognises_wasmtime_prefix() {
        use phase_protocol::JobSpecKind;
        assert_eq!(classify_runtime("wasmtime-27"), Some(JobSpecKind::Wasm));
        assert_eq!(classify_runtime("wasmtime"), Some(JobSpecKind::Wasm));
        assert_eq!(classify_runtime("wasm3-0.5"), Some(JobSpecKind::Wasm));
        assert_eq!(
            classify_runtime("llama-cpp-b3000"),
            Some(JobSpecKind::Inference)
        );
        assert_eq!(classify_runtime("python-3.11"), None);
        assert_eq!(classify_runtime(""), None);
    }

    #[test]
    fn ordinary_reachability_role_never_enables_servers() {
        let config = ReachabilityConfig::default();
        assert_eq!(config.role, ReachabilityRole::Peer);
        assert!(config.relay_client);
        assert!(config.dcutr);
        assert!(config.identify);
        assert!(config.autonat_client);
        assert!(config.rendezvous_client);
        assert!(!config.autonat_server);
        assert!(config.relay_server.is_none());
        assert!(config.rendezvous_server.is_none());
        config.validate().unwrap();

        let peer_server = ReachabilityConfig {
            relay_server: Some(RelayServerLimits::default()),
            ..config.clone()
        };
        assert!(peer_server.validate().is_err());

        let missing_dcutr_dependency = ReachabilityConfig {
            relay_client: false,
            ..config
        };
        assert!(missing_dcutr_dependency.validate().is_err());
    }

    #[test]
    fn infrastructure_limits_reject_unbounded_values() {
        let config = ReachabilityConfig {
            role: ReachabilityRole::Infrastructure,
            relay_server: Some(RelayServerLimits {
                max_circuits: MAX_RELAY_CIRCUITS + 1,
                ..RelayServerLimits::default()
            }),
            ..ReachabilityConfig::default()
        };
        assert!(config.validate().is_err());

        let config = ReachabilityConfig {
            role: ReachabilityRole::Infrastructure,
            rendezvous_server: Some(RendezvousServerLimits {
                min_ttl_seconds: rendezvous::MIN_TTL,
                max_ttl_seconds: rendezvous::MAX_TTL + 1,
            }),
            ..ReachabilityConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn observable_address_filter_accepts_real_loopback_and_relay_transports() {
        for address in [
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/192.168.1.8/udp/4001/quic-v1",
            "/ip6/::1/tcp/4001",
            "/dns4/relay.example/tcp/443/p2p/12D3KooWJwbyF4d4so1sqd1qsSF24dTDtc3DduLYX7v9VruBfpH7/p2p-circuit",
        ] {
            let address: Multiaddr = address.parse().unwrap();
            assert!(is_observable_address(&address), "rejected {address}");
        }

        for address in [
            "/ip4/0.0.0.0/tcp/4001",
            "/ip4/224.0.0.1/tcp/4001",
            "/ip6/::/tcp/4001",
            "/ip6/ff02::1/tcp/4001",
            "/ip4/127.0.0.1/tcp/0",
            "/ip4/127.0.0.1/udp/4001",
            "/ip4/127.0.0.1",
            "/memory/42",
        ] {
            let address: Multiaddr = address.parse().unwrap();
            assert!(!is_observable_address(&address), "accepted {address}");
        }
    }

    #[test]
    fn rendezvous_namespaces_are_bounded_and_nonempty() {
        assert!(parse_rendezvous_namespace("phase-workers").is_ok());
        assert!(parse_rendezvous_namespace("").is_err());
        assert!(parse_rendezvous_namespace(&"x".repeat(rendezvous::MAX_NAMESPACE + 1)).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn direct_loopback_connection_is_reported_from_swarm_state() {
        let _loopback_test = acquire_loopback_test_gate().await;
        let node_a = Discovery::new(DiscoveryConfig::default()).expect("node A");
        let node_b = Discovery::new(DiscoveryConfig::default()).expect("node B");
        let b_peer = *node_b.local_peer_id();

        node_b
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("node B listen");
        let mut address = loopback_dial_addr(&node_b).await;
        address.push(Protocol::P2p(b_peer));
        node_a
            .dial_peer(&address.to_string())
            .await
            .expect("node A dial node B");

        let snapshot = wait_for_peer_path(&node_a, b_peer, &[ReachabilityPath::Direct]).await;
        assert_eq!(snapshot.role, ReachabilityRole::Peer);
        assert_eq!(snapshot.active_path, ReachabilityPath::Direct);
        assert_eq!(snapshot.nat, NatReachability::Unknown);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rendezvous_loopback_registers_and_discovers_signed_peer_record() {
        let _loopback_test = acquire_loopback_test_gate().await;
        let server = Discovery::new_with_reachability(
            DiscoveryConfig::default(),
            ReachabilityConfig {
                role: ReachabilityRole::Infrastructure,
                rendezvous_server: Some(RendezvousServerLimits::default()),
                ..ReachabilityConfig::default()
            },
        )
        .expect("rendezvous server");
        let registrant = Discovery::new(DiscoveryConfig::default()).expect("registrant");
        let discoverer = Discovery::new(DiscoveryConfig::default()).expect("discoverer");
        let server_peer = *server.local_peer_id();
        let registrant_peer = *registrant.local_peer_id();

        server
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("server listen");
        registrant
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("registrant listen");
        let registrant_addr = loopback_dial_addr(&registrant).await;
        registrant
            .add_external_address(registrant_addr.clone())
            .await
            .expect("confirm registrant address");

        let mut server_addr = loopback_dial_addr(&server).await;
        server_addr.push(Protocol::P2p(server_peer));
        registrant
            .dial_peer(&server_addr.to_string())
            .await
            .expect("registrant dial rendezvous server");
        discoverer
            .dial_peer(&server_addr.to_string())
            .await
            .expect("discoverer dial rendezvous server");
        wait_for_peer_path(&registrant, server_peer, &[ReachabilityPath::Direct]).await;
        wait_for_peer_path(&discoverer, server_peer, &[ReachabilityPath::Direct]).await;

        tokio::time::timeout(
            Duration::from_secs(5),
            registrant.register_rendezvous(
                server_peer,
                "phase-workers",
                Some(rendezvous::DEFAULT_TTL),
            ),
        )
        .await
        .expect("rendezvous register timed out")
        .expect("rendezvous register failed");

        let peers = tokio::time::timeout(
            Duration::from_secs(5),
            discoverer.discover_rendezvous(server_peer, Some("phase-workers"), 16),
        )
        .await
        .expect("rendezvous discover timed out")
        .expect("rendezvous discover failed");
        let discovered = peers
            .iter()
            .find(|peer| peer.peer_id == registrant_peer)
            .expect("registered peer missing from discovery response");
        assert_eq!(discovered.namespace, "phase-workers");
        assert!(discovered.addresses.contains(&registrant_addr));
        assert_eq!(discovered.ttl_seconds, rendezvous::DEFAULT_TTL);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn circuit_relay_loopback_reports_real_relayed_connection() {
        let _loopback_test = acquire_loopback_test_gate().await;
        let relay_node = Discovery::new_with_reachability(
            DiscoveryConfig::default(),
            ReachabilityConfig {
                role: ReachabilityRole::Infrastructure,
                relay_server: Some(RelayServerLimits::default()),
                ..ReachabilityConfig::default()
            },
        )
        .expect("relay node");
        // This test proves the relay circuit itself. Disable DCUtR so an
        // immediate direct-path upgrade cannot replace the relayed connection
        // before the assertion observes it; DCUtR has separate behavior tests.
        let relay_only_peer = ReachabilityConfig {
            dcutr: false,
            ..ReachabilityConfig::default()
        };
        let destination =
            Discovery::new_with_reachability(DiscoveryConfig::default(), relay_only_peer.clone())
                .expect("destination");
        let dialer = Discovery::new_with_reachability(DiscoveryConfig::default(), relay_only_peer)
            .expect("dialer");
        let relay_peer = *relay_node.local_peer_id();
        let destination_peer = *destination.local_peer_id();

        relay_node
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("relay listen");
        let relay_addr = loopback_dial_addr(&relay_node).await;
        relay_node
            .add_external_address(relay_addr.clone())
            .await
            .expect("confirm relay address");
        let mut reservation_addr = relay_addr.clone();
        reservation_addr.push(Protocol::P2p(relay_peer));
        reservation_addr.push(Protocol::P2pCircuit);
        destination
            .listen(&reservation_addr.to_string())
            .await
            .expect("destination relay reservation");

        let relayed_listen_addr = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(address) =
                    destination
                        .listen_addrs()
                        .await
                        .unwrap()
                        .into_iter()
                        .find(|address| {
                            address
                                .iter()
                                .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
                        })
                {
                    break address;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("relay reservation timed out");

        // Establish and observe the dialer's control connection to the relay
        // before asking libp2p to negotiate the circuit. This separates relay
        // readiness from the behavior under test (the relayed destination
        // path) instead of racing both handshakes in one dial command.
        let mut relay_dial_addr = relay_addr;
        relay_dial_addr.push(Protocol::P2p(relay_peer));
        dialer
            .dial_peer(&relay_dial_addr.to_string())
            .await
            .expect("dialer connects to relay");
        wait_for_peer_path(&dialer, relay_peer, &[ReachabilityPath::Direct]).await;

        dialer
            .dial_peer(&relayed_listen_addr.to_string())
            .await
            .expect("dial destination through relay");
        let snapshot =
            wait_for_peer_path(&dialer, destination_peer, &[ReachabilityPath::Relayed]).await;
        assert!(snapshot.connections.iter().any(|connection| {
            connection.peer_id == destination_peer && connection.path == ReachabilityPath::Relayed
        }));

        let relay_snapshot = relay_node.reachability_snapshot().await.unwrap();
        assert_eq!(relay_snapshot.role, ReachabilityRole::Infrastructure);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn blob_stream_loopback_resumes_streams_rejects_and_bounds_frames() {
        let _loopback_test = acquire_loopback_test_gate().await;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct CancellationNotice(Arc<AtomicBool>);
        impl Drop for CancellationNotice {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let requester = Discovery::new(DiscoveryConfig::default()).expect("requester");
        let server = Discovery::new(DiscoveryConfig::default()).expect("server");
        let requester_peer = *requester.local_peer_id();
        let server_peer = *server.local_peer_id();
        let content_id = [41; 32];
        let content = Arc::new(b"0123456789".to_vec());
        let cancellation_seen = Arc::new(AtomicBool::new(false));
        let resume_release = Arc::new(tokio::sync::Semaphore::new(0));
        let waiting_after_first_chunk = Arc::new(AtomicBool::new(false));

        server
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("server listen");
        let mut server_addr = loopback_dial_addr(&server).await;
        server_addr.push(Protocol::P2p(server_peer));
        requester
            .dial_peer(&server_addr.to_string())
            .await
            .expect("requester dial server");
        wait_for_peer_path(&requester, server_peer, &[ReachabilityPath::Direct]).await;

        let served_content = content.clone();
        let handler_cancellation = cancellation_seen.clone();
        let handler_resume_release = resume_release.clone();
        let handler_waiting = waiting_after_first_chunk.clone();
        server
            .set_blob_stream_handler(Some(Arc::new(move |peer, request, frames| {
                let content = served_content.clone();
                let cancellation = handler_cancellation.clone();
                let resume_release = handler_resume_release.clone();
                let waiting_after_first_chunk = handler_waiting.clone();
                Box::pin(async move {
                    assert_eq!(peer, requester_peer);
                    if request.metadata == b"wrong-offset" {
                        let _ = frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Accepted {
                                    total_size: content.len() as u64,
                                    offset: request.offset + 1,
                                },
                            ))
                            .await;
                        return;
                    }
                    if request.metadata == b"oversized" {
                        let _ = frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Accepted {
                                    total_size: (BLOB_STREAM_MAX_CHUNK_BYTES + 1) as u64,
                                    offset: request.offset,
                                },
                            ))
                            .await;
                        let _ = frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Chunk {
                                    offset: request.offset,
                                    bytes: vec![0; BLOB_STREAM_MAX_CHUNK_BYTES + 1],
                                },
                            ))
                            .await;
                        return;
                    }
                    if request.offset > content.len() as u64 {
                        frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Rejected {
                                    reason: "requested offset exceeds content size".to_string(),
                                },
                            ))
                            .await
                            .unwrap();
                        return;
                    }

                    frames
                        .send(blob_frame(
                            request.content_id,
                            BlobStreamFrameKind::Accepted {
                                total_size: content.len() as u64,
                                offset: request.offset,
                            },
                        ))
                        .await
                        .unwrap();
                    if request.metadata == b"cancel" {
                        let _notice = CancellationNotice(cancellation);
                        futures::future::pending::<()>().await;
                        return;
                    }

                    let start = request.offset as usize;
                    let first_end = (start + 3).min(content.len());
                    if start < first_end {
                        frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Chunk {
                                    offset: request.offset,
                                    bytes: content[start..first_end].to_vec(),
                                },
                            ))
                            .await
                            .unwrap();
                    }
                    waiting_after_first_chunk.store(true, Ordering::Release);
                    let _permit = resume_release
                        .acquire()
                        .await
                        .expect("test gate remains open");
                    if first_end < content.len() {
                        frames
                            .send(blob_frame(
                                request.content_id,
                                BlobStreamFrameKind::Chunk {
                                    offset: first_end as u64,
                                    bytes: content[first_end..].to_vec(),
                                },
                            ))
                            .await
                            .unwrap();
                    }
                    frames
                        .send(blob_frame(
                            request.content_id,
                            BlobStreamFrameKind::Eof {
                                offset: content.len() as u64,
                            },
                        ))
                        .await
                        .unwrap();
                })
            })))
            .unwrap();

        let mut resume_request = blob_request(content_id, 3, b"resume".to_vec());
        resume_request.idle_timeout_ms = crate::protocol::BLOB_STREAM_MIN_IDLE_TIMEOUT_MS;
        let control_guard = requester.job_relay_stream_control.lock().await;
        let open_future = requester.open_blob_stream(server_peer, resume_request);
        tokio::pin!(open_future);
        assert!(
            tokio::time::timeout(Duration::from_millis(350), open_future.as_mut())
                .await
                .is_err(),
            "valid blob idle timeout must not bound control-lock or transport negotiation"
        );
        drop(control_guard);
        let mut resumed = open_future.await.expect("open resumed blob");
        assert_eq!(resumed.content_id(), &content_id);
        assert!(matches!(
            resumed.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 3,
            }
        ));
        let first = resumed.next_frame().await.unwrap();
        assert_eq!(
            first.kind,
            BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: b"345".to_vec(),
            }
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !waiting_after_first_chunk.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("server reaches deterministic post-chunk gate");
        assert_eq!(resume_release.available_permits(), 0);
        resume_release.add_permits(1);
        assert_eq!(
            resumed.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Chunk {
                offset: 6,
                bytes: b"6789".to_vec(),
            }
        );
        assert_eq!(
            resumed.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Eof { offset: 10 }
        );

        let mut rejected = requester
            .open_blob_stream(server_peer, blob_request(content_id, 11, Vec::new()))
            .await
            .expect("open out-of-range request");
        assert!(matches!(
            rejected.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Rejected { .. }
        ));

        let mut wrong_offset = requester
            .open_blob_stream(
                server_peer,
                blob_request(content_id, 0, b"wrong-offset".to_vec()),
            )
            .await
            .expect("open wrong-offset stream");
        assert!(wrong_offset.next_frame().await.is_err());

        let mut oversized = requester
            .open_blob_stream(
                server_peer,
                blob_request(content_id, 0, b"oversized".to_vec()),
            )
            .await
            .expect("open malformed stream");
        assert!(matches!(
            oversized.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Accepted { .. }
        ));
        assert!(oversized.next_frame().await.is_err());

        let mut cancelled = requester
            .open_blob_stream(server_peer, blob_request(content_id, 0, b"cancel".to_vec()))
            .await
            .expect("open cancellable stream");
        assert!(matches!(
            cancelled.next_frame().await.unwrap().kind,
            BlobStreamFrameKind::Accepted { .. }
        ));
        cancelled.cancel().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !cancellation_seen.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("server handler was not cancelled after stream close");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn live_relay_is_real_ordered_substream_with_cancellation() {
        let _loopback_test = acquire_loopback_test_gate().await;
        let node_a = Discovery::new(DiscoveryConfig::default()).expect("node A");
        let node_b = Discovery::new(DiscoveryConfig::default()).expect("node B");
        let a_peer = *node_a.local_peer_id();
        let b_peer = *node_b.local_peer_id();

        node_b
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("node B listen");
        let mut b_addr = loopback_dial_addr(&node_b).await;
        b_addr.push(libp2p::multiaddr::Protocol::P2p(b_peer));
        node_a
            .dial_peer(&b_addr.to_string())
            .await
            .expect("node A dials node B");
        wait_for_peer_path(&node_a, b_peer, &[ReachabilityPath::Direct]).await;

        let terminal_release = Arc::new(tokio::sync::Semaphore::new(0));
        let waiting_before_terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handler_release = terminal_release.clone();
        let handler_waiting = waiting_before_terminal.clone();
        node_b
            .set_job_relay_stream_handler(Some(Arc::new(
                move |peer, open, mut controls, frames| {
                    let release = handler_release.clone();
                    let waiting = handler_waiting.clone();
                    Box::pin(async move {
                        assert_eq!(peer, a_peer);
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 0,
                                kind: JobRelayStreamFrameKind::Accepted,
                            })
                            .await
                            .unwrap();
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 1,
                                kind: JobRelayStreamFrameKind::Event {
                                    payload: b"first".to_vec(),
                                    terminal: false,
                                },
                            })
                            .await
                            .unwrap();
                        waiting.store(true, std::sync::atomic::Ordering::Release);
                        let _permit = release.acquire().await.expect("test gate remains open");
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 2,
                                kind: JobRelayStreamFrameKind::Event {
                                    payload: b"final".to_vec(),
                                    terminal: true,
                                },
                            })
                            .await
                            .unwrap();
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 3,
                                kind: JobRelayStreamFrameKind::Receipt {
                                    payload: b"signed-receipt".to_vec(),
                                },
                            })
                            .await
                            .unwrap();
                        let _ = controls.recv().await;
                    })
                },
            )))
            .unwrap();

        let job_id = [21; 32];
        let control_guard = node_a.job_relay_stream_control.lock().await;
        let open_future = node_a.open_job_relay_stream(
            b_peer,
            JobRelayStreamOpen {
                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                job_id,
                payload: b"signed-manifest".to_vec(),
                deadline_unix_ms: unix_time_ms() + 5_000,
                idle_timeout_ms: crate::protocol::JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS,
            },
        );
        tokio::pin!(open_future);
        assert!(
            tokio::time::timeout(Duration::from_millis(350), open_future.as_mut())
                .await
                .is_err(),
            "valid idle timeout must not bound control-lock or transport negotiation"
        );
        drop(control_guard);
        let mut stream = open_future.await.expect("open live relay stream");

        assert!(matches!(
            stream.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Accepted
        ));
        assert!(matches!(
            stream.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Event {
                terminal: false,
                ..
            }
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !waiting_before_terminal.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("server reaches deterministic pre-terminal gate");
        assert_eq!(terminal_release.available_permits(), 0);
        terminal_release.add_permits(1);
        assert!(matches!(
            stream.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Event { terminal: true, .. }
        ));
        assert!(matches!(
            stream.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Receipt { .. }
        ));
        stream.acknowledge_receipt().await.unwrap();

        node_b
            .set_job_relay_stream_handler(Some(Arc::new(
                move |_peer, open, mut controls, frames| {
                    Box::pin(async move {
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 0,
                                kind: JobRelayStreamFrameKind::Accepted,
                            })
                            .await
                            .unwrap();
                        let control = controls.recv().await.expect("cancel control");
                        assert!(matches!(
                            control.kind,
                            JobRelayStreamControlKind::Cancel { .. }
                        ));
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 1,
                                kind: JobRelayStreamFrameKind::Failed {
                                    reason: "cancelled".to_string(),
                                },
                            })
                            .await
                            .unwrap();
                    })
                },
            )))
            .unwrap();

        let mut cancelled = node_a
            .open_job_relay_stream(
                b_peer,
                JobRelayStreamOpen {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id: [22; 32],
                    payload: b"signed-manifest".to_vec(),
                    deadline_unix_ms: unix_time_ms() + 5_000,
                    idle_timeout_ms: 1_000,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            cancelled.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Accepted
        ));
        cancelled.cancel("test cancellation").await.unwrap();
        assert!(matches!(
            cancelled.next_frame().await.unwrap().unwrap().kind,
            JobRelayStreamFrameKind::Failed { .. }
        ));

        let (cancel_seen_tx, cancel_seen_rx) = oneshot::channel();
        let cancel_seen_tx = Arc::new(std::sync::Mutex::new(Some(cancel_seen_tx)));
        node_b
            .set_job_relay_stream_handler(Some(Arc::new({
                let cancel_seen_tx = cancel_seen_tx.clone();
                move |_peer, open, mut controls, frames| {
                    let cancel_seen_tx = cancel_seen_tx.clone();
                    Box::pin(async move {
                        frames
                            .send(JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence: 0,
                                kind: JobRelayStreamFrameKind::Accepted,
                            })
                            .await
                            .unwrap();
                        let control = controls.recv().await.expect("idle cancel control");
                        assert!(matches!(
                            control.kind,
                            JobRelayStreamControlKind::Cancel { .. }
                        ));
                        if let Some(tx) = cancel_seen_tx.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                    })
                }
            })))
            .unwrap();

        let mut silent_after_acceptance = node_a
            .open_job_relay_stream(
                b_peer,
                JobRelayStreamOpen {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id: [23; 32],
                    payload: b"signed-manifest".to_vec(),
                    deadline_unix_ms: unix_time_ms() + 5_000,
                    idle_timeout_ms: 250,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            silent_after_acceptance
                .next_frame()
                .await
                .unwrap()
                .unwrap()
                .kind,
            JobRelayStreamFrameKind::Accepted
        ));
        let liveness_failure =
            tokio::time::timeout(Duration::from_secs(2), silent_after_acceptance.next_frame())
                .await
                .expect("silence after Accepted must terminate deterministically");
        assert!(matches!(liveness_failure, Some(Err(_)) | None));
        tokio::time::timeout(Duration::from_secs(2), cancel_seen_rx)
            .await
            .expect("server handler receives cancellation and releases resources")
            .expect("cancel signal sender remains live");
    }
}
