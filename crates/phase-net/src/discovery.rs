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
use futures::StreamExt;
use libp2p::{
    identity::Keypair,
    kad::{
        store::MemoryStore, Behaviour as KademliaBehaviour, Event as KademliaEvent,
        GetRecordOk, Mode as KademliaMode, QueryId, QueryResult,
    },
    mdns,
    request_response::{self, cbor, json, OutboundRequestId, ProtocolSupport, ResponseChannel},
    swarm::{NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
};
use phase_identity::NodeIdentity;
use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::peer::PeerCapabilities;
use crate::protocol::{JobOffer, JobRelayRequest, JobRelayResponse, JobResponse, RejectionReason};

/// Wire protocol identifier for the JobOffer request/response exchange.
const JOB_OFFER_PROTOCOL: &str = "/phase/job-offer/1.0.0";

/// LUCID M5 peer-relay protocol identifier. Carries a CBOR-encoded
/// `JobRelayRequest` and gets back a `JobRelayResponse`. The inner payload
/// is bincode owned by lucidd; phase-net stays inference-agnostic.
const JOB_RELAY_PROTOCOL: &str = "/phase/job-relay/1.0.0";

/// Callback the daemon registers to serve inbound JobRelay requests.
///
/// phase-net knows nothing about `SignedManifest<JobSpec>` or `JobEvent` —
/// the bytes flow through unmodified. lucidd installs a handler that
/// decodes the bincode payload, runs its local worker, and re-encodes the
/// resulting stream as a batch.
pub type JobRelayHandler = std::sync::Arc<
    dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = JobRelayResponse> + Send>>
        + Send
        + Sync
        + 'static,
>;

/// Combined network behaviour: Kademlia DHT + mDNS local discovery +
/// JSON-coded request/response for JobOffer.
#[derive(NetworkBehaviour)]
struct CombinedBehaviour {
    kademlia: KademliaBehaviour<MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    job_offer: json::Behaviour<JobOffer, JobResponse>,
    /// LUCID M5 peer-relay request/response. CBOR-encoded so the binary
    /// payload (bincode `SignedManifest<JobSpec>` / `Vec<JobEvent>`) doesn't
    /// suffer the 4-5× blow-up JSON's u8 arrays cause.
    job_relay: cbor::Behaviour<JobRelayRequest, JobRelayResponse>,
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
    SetJobRelayHandler { handler: Option<JobRelayHandler> },
}

/// Peer discovery service. Owns no swarm directly — instead holds a handle
/// to the background driver task that does.
pub struct Discovery {
    local_peer_id: PeerId,
    signing_key: SigningKey,
    capabilities: PeerCapabilities,
    cmd_tx: mpsc::Sender<Command>,
    /// Background driver task. `run()` takes this to await shutdown; if the
    /// daemon never calls `run()`, the task is torn down when Discovery
    /// drops (the cmd_rx side sees its last Sender go and exits).
    driver: Option<tokio::task::JoinHandle<()>>,
}

impl Discovery {
    /// Create a new discovery service. Spawns the background swarm-driver task.
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
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
        let mut kad_behaviour = KademliaBehaviour::new(local_peer_id, store);
        kad_behaviour.set_mode(Some(KademliaMode::Server));

        // Add bootstrap peers.
        for peer_addr in &config.bootstrap_peers {
            if let Ok(_addr) = peer_addr.parse::<Multiaddr>() {
                // Bootstrap peer format should be:
                //   /ip4/x.x.x.x/tcp/port/p2p/PeerID
                // The previous implementation logged but never actually
                // added them; the parsing-and-routing path lands in a
                // follow-up task on the M2 backlog.
                debug!("Adding bootstrap peer: {}", peer_addr);
            }
        }

        // Build swarm with tokio executor.
        let swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_quic()
            .with_behaviour(|key| {
                // Create mDNS behaviour for local network discovery.
                let mdns_behaviour = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

                // JSON-coded request/response for JobOffer.
                let job_offer = json::Behaviour::<JobOffer, JobResponse>::new(
                    [(
                        StreamProtocol::new(JOB_OFFER_PROTOCOL),
                        ProtocolSupport::Full,
                    )],
                    request_response::Config::default(),
                );

                // CBOR-coded request/response for the LUCID M5 job relay.
                // The default per-request timeout is generous enough for a
                // batch-shaped inference response (we set a wall-clock cap
                // on the requesting side anyway).
                let job_relay = cbor::Behaviour::<JobRelayRequest, JobRelayResponse>::new(
                    [(
                        StreamProtocol::new(JOB_RELAY_PROTOCOL),
                        ProtocolSupport::Full,
                    )],
                    request_response::Config::default()
                        .with_request_timeout(Duration::from_secs(5 * 60)),
                );

                Ok(CombinedBehaviour {
                    kademlia: kad_behaviour,
                    mdns: mdns_behaviour,
                    job_offer,
                    job_relay,
                })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        // Channel sized large enough for typical bursts (one command per
        // public method call). Backpressure here would mean the daemon is
        // calling Discovery faster than libp2p can keep up — fine to block.
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);

        let driver_caps = config.capabilities.clone();
        let driver_peer_id = local_peer_id;
        let driver = tokio::spawn(async move {
            Driver::run(swarm, cmd_rx, driver_caps, driver_peer_id).await;
        });

        Ok(Self {
            local_peer_id,
            signing_key,
            capabilities: config.capabilities,
            cmd_tx,
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
    /// Issues a Kademlia `get_record` query, accumulates every peer-supplied
    /// record value until the query reports completion, and returns the
    /// distinct raw payloads. Decoding (and signature verification, in the
    /// case of `SignedModelAdvertisement` from LUCID's model registry) is
    /// the caller's responsibility.
    ///
    /// Returns an empty `Vec` if no peer holds a record under `key` — that
    /// is *not* an error, just a normal "miss".
    pub async fn get_kad_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
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
    pub async fn send_job_offer(
        &self,
        peer: PeerId,
        offer: JobOffer,
    ) -> Result<JobResponse> {
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
            .send(Command::EvaluateOffer { offer: offer.clone(), reply: tx })
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

// ---------------------------------------------------------------------------
// Driver — owns the swarm, processes commands and behaviour events.
// ---------------------------------------------------------------------------

struct Driver {
    swarm: Swarm<CombinedBehaviour>,
    capabilities: PeerCapabilities,
    local_peer_id: PeerId,
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
}

/// Accumulator for an outstanding `GetKadRecord` query.
///
/// libp2p emits multiple `OutboundQueryProgressed` events as the iterative
/// query walks the kbucket tree — one per peer that responded — and the
/// driver folds each value into `values` before sending the final reply
/// when `step.last` is set.
struct PendingGetRecord {
    reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
    values: Vec<Vec<u8>>,
}

impl Driver {
    async fn run(
        swarm: Swarm<CombinedBehaviour>,
        mut cmd_rx: mpsc::Receiver<Command>,
        capabilities: PeerCapabilities,
        local_peer_id: PeerId,
    ) {
        let mut driver = Driver {
            swarm,
            capabilities,
            local_peer_id,
            pending_offers: HashMap::new(),
            pending_get_records: HashMap::new(),
            pending_relays: HashMap::new(),
            job_relay_handler: None,
        };

        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(c) => driver.handle_command(c),
                        None => break, // All Discovery handles dropped.
                    }
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
            Command::Bootstrap { reply } => {
                let res = match self.swarm.behaviour_mut().kademlia.bootstrap() {
                    Ok(_) => {
                        info!("DHT bootstrap initiated");
                        Ok(())
                    }
                    Err(e) => {
                        warn!(
                            "DHT bootstrap failed (normal for standalone nodes): {}",
                            e
                        );
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
            Command::DiscoverPeers { arch, kind_label, reply } => {
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
                let query_id =
                    self.swarm.behaviour_mut().kademlia.get_record(record_key);
                self.pending_get_records.insert(
                    query_id,
                    PendingGetRecord {
                        reply,
                        values: Vec::new(),
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
            Command::SendJobRelay { peer, request, reply } => {
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

    async fn handle_swarm_event(
        &mut self,
        event: SwarmEvent<CombinedBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Kademlia(kad)) => {
                self.handle_kad_event(kad);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::Mdns(mdns_ev)) => {
                self.handle_mdns_event(mdns_ev);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::JobOffer(rr)) => {
                self.handle_job_offer_event(rr);
            }
            SwarmEvent::Behaviour(CombinedBehaviourEvent::JobRelay(rr)) => {
                self.handle_job_relay_event(rr).await;
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on new address: {}", address);
                let s = address.to_string();
                if s.contains("127.0.0.1") || s.contains("localhost") {
                    debug!("Local address - no NAT traversal needed");
                } else if s.contains("0.0.0.0") {
                    info!(
                        "Listening on all interfaces - configure port forwarding for NAT traversal"
                    );
                    info!("Note: QUIC transport assists with NAT traversal");
                } else {
                    info!("External address detected: {}", address);
                }
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!("Connected to peer: {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
                debug!("Connection closed to {}: {:?}", peer_id, cause);
            }
            other => {
                debug!("Other swarm event: {:?}", other);
            }
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
                                // De-dupe by raw payload — the same record
                                // can come back from several peers.
                                if !pending.values.contains(&rec.record.value) {
                                    pending.values.push(rec.record.value);
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

    fn handle_job_offer_event(
        &mut self,
        event: request_response::Event<JobOffer, JobResponse>,
    ) {
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

    async fn handle_job_relay_event(
        &mut self,
        event: request_response::Event<JobRelayRequest, JobRelayResponse>,
    ) {
        use request_response::{Event, Message};
        match event {
            Event::Message { message, .. } => match message {
                Message::Request {
                    request, channel, ..
                } => {
                    // Default behaviour: no handler installed → refuse
                    // closed. Daemons that want to serve peers must call
                    // `set_job_relay_handler`.
                    let response = match self.job_relay_handler.clone() {
                        Some(handler) => handler(request.payload).await,
                        None => JobRelayResponse::Err {
                            reason: "no job-relay handler installed".to_string(),
                        },
                    };
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
}
