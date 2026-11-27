// SPDX-License-Identifier: AGPL-3.0-or-later

//! Coordinator side of the `/phase/shard-rpc/1.0.0` tunnel: Tier-1
//! Trusted-Circle layer offload to *enrolled peers* instead of raw sockets.
//!
//! `llama-server --rpc` only speaks plain TCP, and `ggml-rpc` has no
//! authentication or encryption of its own. For every `llama-server` spawn the
//! worker binds one ephemeral loopback listener (`127.0.0.1:0`) per
//! `--llama-rpc-peers` PeerId and hands those addresses to `llama-server --rpc`
//! like any other loopback RPC endpoint. Each TCP connection `llama-server`
//! opens is carried over its own Noise-encrypted libp2p `shard-rpc` session to
//! exactly that peer, whose `lucidd` admits it only if this node is enrolled
//! there and its operator has `cluster_enabled = true`, then bridges it to a
//! loopback `ggml-rpc-server`.
//!
//! The proxy endpoints are ordinary loopback `--rpc` entries, so all of the
//! Wave 1 guardrails apply unchanged: the per-spawn API key and CORS lockdown
//! of `llama-server`, the live `cluster_enabled` gate on every job and respawn,
//! the `trusted-circle-sharded` route label, and the `X-Lucid-Local-Only`
//! refusal. In addition each proxy:
//!
//! - opens a tunnel only for a client whose first frame is a `ggml-rpc` HELLO,
//!   so browser-originated HTTP (or any other stray local connection) never
//!   reaches a provider and can never be mistaken for a lost RPC link;
//! - refuses new connections and drops live tunnels as soon as the
//!   coordinator's own `cluster_enabled` gate closes;
//! - bounds concurrent tunnels per peer;
//! - belongs to exactly one spawned model: a tunnel that ends from the remote
//!   side (provider revoked, idle/session bound, peer lost) retires *that*
//!   model only, whose `llama-server` would otherwise abort on the next use of
//!   its dead `ggml-rpc` socket. Other resident models are unaffected.
//!
//! Residual exposure: a proxy port is still reachable by local processes that
//! speak `ggml-rpc` themselves. They reach only the enrolled provider's
//! `ggml-rpc-server`, through its own protocol checks, and can at worst disturb
//! the one model that owns the proxy. Clients that have not sent their HELLO
//! wait in a small separate pool with a short timeout and never occupy a
//! tunnel slot; a local process flooding that pool can still delay the owning
//! model's load, which loopback alone cannot prevent. Tunnels need a direct libp2p connection
//! to the peer: relayed circuits are byte- and time-limited and will cut a
//! weight upload short, failing the load. A single-client `ggml-rpc-server`
//! serves one tunnel at a time; further connections wait in its backlog.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use phase_net::{
    Discovery, PeerId, ShardRpcOpen, ShardRpcSessionEnd, MAX_INBOUND_SHARD_RPC_SESSIONS_PER_PEER,
    SHARD_RPC_DEFAULT_IDLE_TIMEOUT_MS, SHARD_RPC_DEFAULT_SESSION_MS, SHARD_RPC_SCHEMA_VERSION,
};
use ring::rand::SecureRandom;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use super::{
    validate_loopback_rpc_servers, validate_rpc_offload_flags, validate_tensor_split,
    RpcClusterGate, MAX_RPC_ENDPOINTS,
};

/// How often a live tunnel re-checks the coordinator's `cluster_enabled` gate.
pub const RPC_PEER_GATE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `ggml-rpc` frames start with a one-byte command and a little-endian `u64`
/// payload size. A client's first command is always HELLO (`0x0e`, verified
/// against the pinned llama.cpp build, which sends a 24-byte payload).
const GGML_RPC_FRAME_HEADER_BYTES: usize = 9;
const GGML_RPC_CMD_HELLO: u8 = 0x0e;
const GGML_RPC_MAX_HELLO_PAYLOAD_BYTES: u64 = 4096;

/// Connections per proxy that may be waiting for their HELLO at once. They
/// never count against the tunnel cap, so stray or silent local clients
/// cannot crowd out `llama-server`'s own connection.
const MAX_PENDING_RPC_HANDSHAKES: usize = 16;

/// Tunables for the per-peer loopback proxies.
#[derive(Debug, Clone)]
pub struct RpcPeerTunnelConfig {
    /// Requested silence bound per tunnel (enforced by both ends).
    pub idle_timeout: Duration,
    /// Requested total lifetime per tunnel (enforced by both ends).
    pub max_session: Duration,
    /// Concurrent tunnels (connections past their HELLO) per peer and
    /// spawned model. One `llama-server` normally holds a single connection
    /// per `--rpc` endpoint.
    pub max_connections_per_peer: usize,
    /// How long a new local connection may take to send its HELLO header
    /// before it is dropped without a tunnel ever being opened.
    /// `llama-server` sends it immediately after connecting.
    pub handshake_timeout: Duration,
}

impl Default for RpcPeerTunnelConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_millis(SHARD_RPC_DEFAULT_IDLE_TIMEOUT_MS),
            max_session: Duration::from_millis(SHARD_RPC_DEFAULT_SESSION_MS),
            max_connections_per_peer: MAX_INBOUND_SHARD_RPC_SESSIONS_PER_PEER,
            handshake_timeout: Duration::from_secs(1),
        }
    }
}

/// Parse `--llama-rpc-peers`: canonical PeerIds, no duplicates, at most
/// [`MAX_RPC_ENDPOINTS`].
pub fn parse_rpc_peer_ids(raw: &[String]) -> std::io::Result<Vec<PeerId>> {
    if raw.len() > MAX_RPC_ENDPOINTS {
        return Err(invalid_input(format!(
            "at most {MAX_RPC_ENDPOINTS} --llama-rpc-peers are supported"
        )));
    }
    let mut peers = Vec::with_capacity(raw.len());
    for value in raw {
        let peer: PeerId = value
            .trim()
            .parse()
            .map_err(|_| invalid_input(format!("invalid --llama-rpc-peers PeerId {value:?}")))?;
        if peers.contains(&peer) {
            return Err(invalid_input(format!(
                "duplicate --llama-rpc-peers PeerId {peer}"
            )));
        }
        peers.push(peer);
    }
    Ok(peers)
}

/// Validate the complete startup RPC offload flag set, before any identity
/// or network is touched: loopback-only `rpc_servers`, well-formed PeerIds,
/// a well-formed `tensor_split` that has some RPC endpoint to split across,
/// and at most [`MAX_RPC_ENDPOINTS`] endpoints in total.
pub fn validate_rpc_offload_cli(
    rpc_servers: &[String],
    rpc_peers: &[String],
    tensor_split: Option<&str>,
) -> std::io::Result<Vec<PeerId>> {
    let peers = parse_rpc_peer_ids(rpc_peers)?;
    if peers.is_empty() {
        validate_rpc_offload_flags(rpc_servers, tensor_split)?;
    } else {
        validate_loopback_rpc_servers(rpc_servers)?;
        if let Some(split) = tensor_split {
            validate_tensor_split(split)?;
        }
    }
    if rpc_servers.len().saturating_add(peers.len()) > MAX_RPC_ENDPOINTS {
        return Err(invalid_input(format!(
            "at most {MAX_RPC_ENDPOINTS} combined --llama-rpc-servers/--llama-rpc-peers endpoints are supported"
        )));
    }
    Ok(peers)
}

fn invalid_input(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

/// Enrolled-peer RPC layer offload for [`super::LlamaCppConfig`]: which peers
/// each spawned `llama-server` offloads layers to, over which transport.
#[derive(Clone)]
pub struct RpcPeerOffload {
    discovery: Arc<Discovery>,
    peers: Vec<PeerId>,
    tunnel: RpcPeerTunnelConfig,
}

impl RpcPeerOffload {
    pub fn new(
        discovery: Arc<Discovery>,
        peers: Vec<PeerId>,
        tunnel: RpcPeerTunnelConfig,
    ) -> std::io::Result<Self> {
        if peers.is_empty() || peers.len() > MAX_RPC_ENDPOINTS {
            return Err(invalid_input(format!(
                "RPC peer offload needs 1..={MAX_RPC_ENDPOINTS} peers"
            )));
        }
        for (index, peer) in peers.iter().enumerate() {
            if peers[..index].contains(peer) {
                return Err(invalid_input(format!(
                    "duplicate --llama-rpc-peers PeerId {peer}"
                )));
            }
        }
        if peers.contains(discovery.local_peer_id()) {
            return Err(invalid_input(
                "--llama-rpc-peers must not name this node's own PeerId".to_string(),
            ));
        }
        if tunnel.max_connections_per_peer == 0 {
            return Err(invalid_input(
                "RPC peer proxies need at least one connection slot per peer".to_string(),
            ));
        }
        Ok(Self {
            discovery,
            peers,
            tunnel,
        })
    }

    pub fn peers(&self) -> &[PeerId] {
        &self.peers
    }

    /// Bind one `127.0.0.1:0` proxy per peer, in order, for one spawned
    /// model. Each accepted connection is admitted only while `gate` is open.
    pub(crate) async fn bind(&self, gate: RpcClusterGate) -> std::io::Result<RpcPeerProxySet> {
        let (losses_tx, remote_losses) = watch::channel(0_u64);
        let losses_tx = Arc::new(losses_tx);
        let live = Arc::new(AtomicUsize::new(0));
        let mut proxies = Vec::with_capacity(self.peers.len());
        for &peer in &self.peers {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            let endpoint = listener.local_addr()?;
            let accept_task = tokio::spawn(run_rpc_peer_proxy(
                listener,
                ProxyContext {
                    discovery: self.discovery.clone(),
                    peer,
                    gate: gate.clone(),
                    config: self.tunnel.clone(),
                    losses: losses_tx.clone(),
                    live: live.clone(),
                    pending: Arc::new(Semaphore::new(MAX_PENDING_RPC_HANDSHAKES)),
                    tunnel_slots: Arc::new(Semaphore::new(self.tunnel.max_connections_per_peer)),
                },
            ));
            tracing::debug!(%peer, %endpoint, "bound trusted-circle RPC peer proxy");
            proxies.push(RpcPeerProxy {
                endpoint,
                accept_task,
            });
        }
        Ok(RpcPeerProxySet {
            proxies,
            remote_losses,
            #[cfg(test)]
            live,
        })
    }
}

impl std::fmt::Debug for RpcPeerOffload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcPeerOffload")
            .field("peers", &self.peers)
            .field("tunnel", &self.tunnel)
            .finish_non_exhaustive()
    }
}

/// The loopback proxies of one spawned model. Dropping the set closes every
/// listener and tears down every tunnel it carries.
pub(crate) struct RpcPeerProxySet {
    proxies: Vec<RpcPeerProxy>,
    remote_losses: watch::Receiver<u64>,
    #[cfg(test)]
    live: Arc<AtomicUsize>,
}

impl RpcPeerProxySet {
    /// Loopback `--rpc` endpoints, one per peer, in peer order.
    pub(crate) fn rpc_endpoints(&self) -> Vec<String> {
        self.proxies
            .iter()
            .map(|proxy| proxy.endpoint.to_string())
            .collect()
    }

    /// Ticks once per tunnel that ended other than by `llama-server` closing
    /// its own socket (remote side, local gate, or a bound).
    pub(crate) fn remote_losses(&self) -> watch::Receiver<u64> {
        self.remote_losses.clone()
    }

    /// Connections currently being handled (handshaking or tunnelled).
    #[cfg(test)]
    pub(crate) fn live_connections(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

struct RpcPeerProxy {
    endpoint: SocketAddr,
    accept_task: JoinHandle<()>,
}

impl Drop for RpcPeerProxy {
    fn drop(&mut self) {
        // The accept task owns the per-connection `JoinSet`; aborting it drops
        // the set, which aborts every live tunnel and closes its sockets.
        self.accept_task.abort();
    }
}

#[derive(Clone)]
struct ProxyContext {
    discovery: Arc<Discovery>,
    peer: PeerId,
    gate: RpcClusterGate,
    config: RpcPeerTunnelConfig,
    losses: Arc<watch::Sender<u64>>,
    live: Arc<AtomicUsize>,
    /// Connections still waiting for their HELLO.
    pending: Arc<Semaphore>,
    /// Established tunnels (past HELLO), capped per peer and model.
    tunnel_slots: Arc<Semaphore>,
}

async fn run_rpc_peer_proxy(listener: TcpListener, context: ProxyContext) {
    let peer = context.peer;
    let mut tunnels = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (local, from) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::warn!(%peer, %error, "RPC peer proxy accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                if !from.ip().is_loopback() {
                    tracing::warn!(%peer, %from, "dropping non-loopback RPC peer proxy connection");
                    continue;
                }
                if !context.gate.is_open() {
                    tracing::warn!(%peer, "refusing RPC peer tunnel: cluster_enabled is off");
                    continue;
                }
                let Ok(pending) = context.pending.clone().try_acquire_owned() else {
                    tracing::warn!(%peer, "refusing RPC peer proxy connection: too many clients awaiting HELLO");
                    continue;
                };
                while tunnels.try_join_next().is_some() {}
                context.live.fetch_add(1, Ordering::SeqCst);
                let context = context.clone();
                tunnels.spawn(async move {
                    serve_rpc_peer_connection(&context, local, pending).await;
                    context.live.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Some(_) = tunnels.join_next(), if !tunnels.is_empty() => {}
        }
    }
}

fn is_ggml_rpc_hello(header: &[u8; GGML_RPC_FRAME_HEADER_BYTES]) -> bool {
    let mut size = [0_u8; 8];
    size.copy_from_slice(&header[1..]);
    header[0] == GGML_RPC_CMD_HELLO && u64::from_le_bytes(size) <= GGML_RPC_MAX_HELLO_PAYLOAD_BYTES
}

async fn serve_rpc_peer_connection(
    context: &ProxyContext,
    mut local: TcpStream,
    pending: tokio::sync::OwnedSemaphorePermit,
) {
    let peer = context.peer;
    // `ggml-rpc` issues one small command per graph step; never batch them.
    let _ = local.set_nodelay(true);

    // Nothing is dialled until the client proves it is a ggml-rpc client.
    let mut header = [0_u8; GGML_RPC_FRAME_HEADER_BYTES];
    match tokio::time::timeout(
        context.config.handshake_timeout,
        local.read_exact(&mut header),
    )
    .await
    {
        Ok(Ok(_)) if is_ggml_rpc_hello(&header) => {}
        Ok(Ok(_)) => {
            tracing::warn!(%peer, "refusing RPC peer tunnel: client did not open with a ggml-rpc HELLO");
            return;
        }
        Ok(Err(_)) | Err(_) => {
            tracing::debug!(%peer, "RPC peer proxy client sent no ggml-rpc handshake");
            return;
        }
    }
    drop(pending);
    // Only clients past their HELLO count toward the tunnel cap.
    let Ok(_tunnel_slot) = context.tunnel_slots.clone().try_acquire_owned() else {
        tracing::warn!(%peer, limit = context.config.max_connections_per_peer, "refusing RPC peer tunnel at the per-peer connection cap");
        return;
    };

    let Some(session_id) = random_session_id() else {
        tracing::error!(%peer, "OS CSPRNG unavailable; refusing RPC peer tunnel");
        return;
    };
    let open = ShardRpcOpen {
        schema_version: SHARD_RPC_SCHEMA_VERSION,
        session_id,
        idle_timeout_ms: duration_ms(context.config.idle_timeout),
        max_session_ms: duration_ms(context.config.max_session),
    };
    let tunnel = match context.discovery.open_shard_rpc_tunnel(peer, open).await {
        Ok(tunnel) => tunnel,
        Err(error) => {
            // Dropping `local` fails `llama-server`'s RPC handshake, so the
            // load fails instead of silently running elsewhere.
            tracing::warn!(%peer, error = %format!("{error:#}"), "trusted-circle RPC tunnel was not established");
            return;
        }
    };
    // The opt-in may have been withdrawn while the tunnel was negotiated.
    if !context.gate.is_open() {
        tracing::warn!(%peer, "cluster_enabled turned off during tunnel setup; dropping it");
        return;
    }

    // Replay the HELLO header we consumed, then splice the rest.
    let (reader, writer) = local.into_split();
    let local = tokio::io::join(std::io::Cursor::new(header).chain(reader), writer);
    let report = tunnel
        .splice(local, Some(Box::pin(gate_closed(context.gate.clone()))))
        .await;
    if report.end == ShardRpcSessionEnd::LocalClosed {
        tracing::debug!(%peer, ?report, "trusted-circle RPC tunnel closed by llama-server");
    } else {
        tracing::warn!(%peer, ?report, "trusted-circle RPC tunnel ended outside llama-server's control");
        context
            .losses
            .send_modify(|count| *count = count.wrapping_add(1));
    }
}

async fn gate_closed(gate: RpcClusterGate) {
    // `interval` ticks immediately, so the gate is checked before the first
    // sleep rather than after it.
    let mut checks = tokio::time::interval(RPC_PEER_GATE_POLL_INTERVAL);
    loop {
        checks.tick().await;
        if !gate.is_open() {
            tracing::warn!("cluster_enabled turned off; dropping trusted-circle RPC tunnel");
            return;
        }
    }
}

fn random_session_id() -> Option<[u8; 32]> {
    let mut session_id = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut session_id)
        .ok()
        .map(|()| session_id)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use phase_identity::NodeIdentity;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::shard_worker::make_opt_in_inbound_shard_rpc_handler;
    use crate::shard_worker::shard_rpc_test_support::*;

    /// The HELLO frame the pinned llama.cpp client opens with: command
    /// `0x0e`, a 24-byte payload.
    fn hello() -> Vec<u8> {
        let mut frame = vec![GGML_RPC_CMD_HELLO];
        frame.extend_from_slice(&24_u64.to_le_bytes());
        frame.extend_from_slice(&[0_u8; 24]);
        frame
    }

    #[test]
    fn rpc_peer_flags_are_validated_before_any_network_is_touched() {
        let a = PeerId::random().to_string();
        let b = PeerId::random().to_string();
        assert_eq!(
            parse_rpc_peer_ids(&[a.clone(), format!(" {b} ")])
                .unwrap()
                .len(),
            2
        );
        assert!(parse_rpc_peer_ids(&["not-a-peer".to_string()]).is_err());
        let duplicate = parse_rpc_peer_ids(&[a.clone(), a.clone()]).unwrap_err();
        assert!(duplicate.to_string().contains("duplicate"), "{duplicate}");
        let too_many: Vec<String> = (0..=MAX_RPC_ENDPOINTS)
            .map(|_| PeerId::random().to_string())
            .collect();
        assert!(parse_rpc_peer_ids(&too_many).is_err());

        let loopback = vec!["127.0.0.1:50052".to_string()];
        let one_peer = std::slice::from_ref(&a);
        // A tensor split needs some RPC endpoint; enrolled peers count.
        assert!(validate_rpc_offload_cli(&[], one_peer, Some("30,70")).is_ok());
        let missing = validate_rpc_offload_cli(&[], &[], Some("30,70")).unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("requires --llama-rpc-servers or --llama-rpc-peers"),
            "{missing}"
        );
        // Peers never relax the loopback-only or split-syntax rules.
        assert!(
            validate_rpc_offload_cli(&["198.51.100.7:50052".to_string()], one_peer, None).is_err()
        );
        assert!(validate_rpc_offload_cli(&loopback, one_peer, Some("0,0")).is_err());
        // Explicit servers plus peers share one endpoint budget.
        let fifteen_peers: Vec<String> = (0..MAX_RPC_ENDPOINTS - 1)
            .map(|_| PeerId::random().to_string())
            .collect();
        assert!(validate_rpc_offload_cli(&loopback, &fifteen_peers, None).is_ok());
        let two_servers = vec!["127.0.0.1:50052".to_string(), "127.0.0.1:50053".to_string()];
        assert!(validate_rpc_offload_cli(&two_servers, &fifteen_peers, None).is_err());
    }

    #[test]
    fn only_a_bounded_ggml_rpc_hello_opens_a_tunnel() {
        let header = |command: u8, size: u64| {
            let mut frame = [0_u8; GGML_RPC_FRAME_HEADER_BYTES];
            frame[0] = command;
            frame[1..].copy_from_slice(&size.to_le_bytes());
            frame
        };
        assert!(is_ggml_rpc_hello(&header(GGML_RPC_CMD_HELLO, 24)));
        assert!(is_ggml_rpc_hello(&header(GGML_RPC_CMD_HELLO, 0)));
        assert!(!is_ggml_rpc_hello(&header(
            GGML_RPC_CMD_HELLO,
            GGML_RPC_MAX_HELLO_PAYLOAD_BYTES + 1
        )));
        assert!(!is_ggml_rpc_hello(&header(0x00, 24)));
        // Anything a browser can send starts with an HTTP method token.
        let mut http = [0_u8; GGML_RPC_FRAME_HEADER_BYTES];
        http.copy_from_slice(b"GET / HTT");
        assert!(!is_ggml_rpc_hello(&http));
    }

    struct Pair {
        _provider: Arc<Discovery>,
        coordinator: Arc<Discovery>,
        provider_peer: PeerId,
        provider_config: Arc<crate::node_config::NodeConfigStore>,
        backend: EchoBackend,
    }

    /// Provider (real shard-rpc bridge onto an echo backend, enrolling
    /// `coordinator` iff `enrolled`) connected to a coordinator node.
    async fn provider_and_coordinator(enrolled: bool) -> Pair {
        let provider_identity = NodeIdentity::generate();
        let coordinator_identity = NodeIdentity::generate();
        let provider = discovery_for(&provider_identity);
        let coordinator = discovery_for(&coordinator_identity);
        connect(&coordinator, &provider).await;
        let backend = EchoBackend::start().await;
        let provider_config = cluster_config(true);
        let enrolled_identities: Vec<&NodeIdentity> = if enrolled {
            vec![&coordinator_identity]
        } else {
            Vec::new()
        };
        provider
            .set_shard_rpc_handler(Some(
                make_opt_in_inbound_shard_rpc_handler(
                    policy_enrolling(&enrolled_identities),
                    provider_config.clone(),
                    backend.port,
                )
                .unwrap(),
            ))
            .unwrap();
        Pair {
            provider_peer: *provider.local_peer_id(),
            _provider: provider,
            coordinator,
            provider_config,
            backend,
        }
    }

    fn switch_gate(open: bool) -> (RpcClusterGate, Arc<AtomicBool>) {
        let state = Arc::new(AtomicBool::new(open));
        let reader = state.clone();
        (
            RpcClusterGate::new(move || reader.load(Ordering::SeqCst)),
            state,
        )
    }

    async fn bind_one(
        pair: &Pair,
        gate: RpcClusterGate,
        config: RpcPeerTunnelConfig,
    ) -> (RpcPeerProxySet, SocketAddr) {
        let proxies =
            RpcPeerOffload::new(pair.coordinator.clone(), vec![pair.provider_peer], config)
                .unwrap()
                .bind(gate)
                .await
                .unwrap();
        let endpoints = proxies.rpc_endpoints();
        assert_eq!(endpoints.len(), 1);
        let endpoint = endpoints[0].parse().unwrap();
        (proxies, endpoint)
    }

    /// `true` once the connection's read side reports EOF or an error.
    async fn closed_within(stream: &mut TcpStream, within: Duration) -> bool {
        let mut byte = [0_u8; 1];
        matches!(
            tokio::time::timeout(within, stream.read(&mut byte)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    }

    async fn wait_until(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let started = tokio::time::Instant::now();
        while started.elapsed() < within {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        condition()
    }

    /// Connect as llama-server does: HELLO first, then (echoed) traffic.
    async fn rpc_client(endpoint: SocketAddr) -> TcpStream {
        let mut stream = TcpStream::connect(endpoint).await.unwrap();
        let hello = hello();
        stream.write_all(&hello).await.unwrap();
        let mut echoed = vec![0_u8; hello.len()];
        tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut echoed))
            .await
            .expect("HELLO reaches the provider backend and comes back")
            .unwrap();
        assert_eq!(
            echoed, hello,
            "the consumed HELLO header is replayed intact"
        );
        stream
    }

    #[test]
    fn offload_config_rejects_self_duplicates_and_empty_peer_sets() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let coordinator = discovery_for(&NodeIdentity::generate());
            let other = PeerId::random();
            let own = *coordinator.local_peer_id();
            let config = RpcPeerTunnelConfig::default;
            assert!(RpcPeerOffload::new(coordinator.clone(), vec![other], config()).is_ok());
            assert!(RpcPeerOffload::new(coordinator.clone(), Vec::new(), config()).is_err());
            assert!(RpcPeerOffload::new(coordinator.clone(), vec![own], config()).is_err());
            assert!(
                RpcPeerOffload::new(coordinator.clone(), vec![other, other], config()).is_err()
            );
            let no_slots = RpcPeerTunnelConfig {
                max_connections_per_peer: 0,
                ..config()
            };
            assert!(RpcPeerOffload::new(coordinator, vec![other], no_slots).is_err());
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxy_tunnels_loopback_tcp_to_the_enrolled_providers_backend() {
        let _gate = two_node_gate().await;
        let pair = provider_and_coordinator(true).await;
        let (gate, _) = switch_gate(true);
        let (proxies, endpoint) = bind_one(&pair, gate, RpcPeerTunnelConfig::default()).await;
        assert!(endpoint.ip().is_loopback() && endpoint.port() != 0);
        super::super::validate_loopback_rpc_servers(&proxies.rpc_endpoints())
            .expect("proxy endpoints satisfy the Wave 1 loopback rule");

        // What llama-server does: connect to the --rpc endpoint and speak.
        let rpc = rpc_client(endpoint).await;
        let payload = pattern(3 * 1024 * 1024 + 1, 0x77);
        let expected = payload.clone();
        let (mut reader, mut writer) = rpc.into_split();
        let write = tokio::spawn(async move {
            writer.write_all(&payload).await.unwrap();
            writer
        });
        let mut echoed = vec![0_u8; expected.len()];
        tokio::time::timeout(Duration::from_secs(30), reader.read_exact(&mut echoed))
            .await
            .expect("round trip completes")
            .unwrap();
        assert!(
            echoed == expected,
            "bytes survive coordinator -> provider -> backend -> back"
        );
        let rpc = reader.reunite(write.await.unwrap()).unwrap();
        assert!(pair.backend.wait_live(1, Duration::from_secs(2)).await);

        // llama-server closing its socket is a clean local close: the bridge
        // connection is released and no loss is reported.
        let losses = proxies.remote_losses();
        drop(rpc);
        assert!(pair.backend.wait_live(0, Duration::from_secs(5)).await);
        assert!(wait_until(Duration::from_secs(5), || proxies.live_connections() == 0).await);
        assert_eq!(*losses.borrow(), 0);

        // Dropping the proxy set closes the listener.
        drop(proxies);
        assert!(
            wait_until(Duration::from_secs(2), || std::net::TcpStream::connect(
                endpoint
            )
            .is_err())
            .await
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxy_dials_nothing_for_non_rpc_or_silent_clients() {
        let _gate = two_node_gate().await;
        let pair = provider_and_coordinator(true).await;
        let (gate, _) = switch_gate(true);
        let config = RpcPeerTunnelConfig {
            handshake_timeout: Duration::from_millis(300),
            ..RpcPeerTunnelConfig::default()
        };
        let (proxies, endpoint) = bind_one(&pair, gate, config).await;

        // A browser-style HTTP request, a silent client, and an oversized
        // HELLO are all dropped without a tunnel or a reported loss.
        let mut browser = TcpStream::connect(endpoint).await.unwrap();
        browser
            .write_all(b"POST /x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        assert!(closed_within(&mut browser, Duration::from_secs(2)).await);
        let mut silent = TcpStream::connect(endpoint).await.unwrap();
        assert!(closed_within(&mut silent, Duration::from_secs(2)).await);
        let mut oversized = TcpStream::connect(endpoint).await.unwrap();
        let mut frame = vec![GGML_RPC_CMD_HELLO];
        frame.extend_from_slice(&u64::MAX.to_le_bytes());
        oversized.write_all(&frame).await.unwrap();
        assert!(closed_within(&mut oversized, Duration::from_secs(2)).await);

        assert!(wait_until(Duration::from_secs(5), || proxies.live_connections() == 0).await);
        assert_eq!(pair.backend.accepted.load(Ordering::SeqCst), 0);
        assert_eq!(*proxies.remote_losses().borrow(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxy_fails_closed_on_local_gate_and_reports_remote_revocation() {
        let _gate = two_node_gate().await;
        let pair = provider_and_coordinator(true).await;
        let (gate, open) = switch_gate(false);
        let (proxies, endpoint) = bind_one(&pair, gate, RpcPeerTunnelConfig::default()).await;
        let losses = proxies.remote_losses();

        // Coordinator gate closed: connections are dropped and nothing ever
        // reaches the provider.
        let mut refused = TcpStream::connect(endpoint).await.unwrap();
        assert!(closed_within(&mut refused, Duration::from_secs(2)).await);
        assert_eq!(pair.backend.accepted.load(Ordering::SeqCst), 0);

        // Gate open: tunnel established. Closing the gate drops it.
        open.store(true, Ordering::SeqCst);
        let mut live = rpc_client(endpoint).await;
        open.store(false, Ordering::SeqCst);
        assert!(closed_within(&mut live, Duration::from_secs(3)).await);
        assert!(pair.backend.wait_live(0, Duration::from_secs(3)).await);
        assert!(wait_until(Duration::from_secs(3), || *losses.borrow() >= 1).await);

        // The provider withdrawing cluster_enabled mid-session also drops the
        // tunnel, and is reported as a loss.
        open.store(true, Ordering::SeqCst);
        let mut live = rpc_client(endpoint).await;
        let before = *losses.borrow();
        set_cluster(&pair.provider_config, false);
        assert!(closed_within(&mut live, Duration::from_secs(3)).await);
        drop(live);
        assert!(wait_until(Duration::from_secs(5), || *losses.borrow() > before).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxy_refuses_unenrolled_coordinators_and_excess_connections() {
        let _gate = two_node_gate().await;
        // The provider does not enroll this coordinator: llama-server's
        // connection is dropped and the backend is never reached.
        let pair = provider_and_coordinator(false).await;
        let (gate, _) = switch_gate(true);
        let (proxies, endpoint) =
            bind_one(&pair, gate.clone(), RpcPeerTunnelConfig::default()).await;
        let mut refused = TcpStream::connect(endpoint).await.unwrap();
        refused.write_all(&hello()).await.unwrap();
        assert!(closed_within(&mut refused, Duration::from_secs(5)).await);
        assert!(wait_until(Duration::from_secs(5), || proxies.live_connections() == 0).await);
        assert_eq!(pair.backend.accepted.load(Ordering::SeqCst), 0);
        assert_eq!(*proxies.remote_losses().borrow(), 0);

        // Per-peer connection cap.
        let pair = provider_and_coordinator(true).await;
        let (proxies, endpoint) = bind_one(
            &pair,
            gate,
            RpcPeerTunnelConfig {
                max_connections_per_peer: 1,
                ..RpcPeerTunnelConfig::default()
            },
        )
        .await;
        // Silent clients waiting for HELLO never take the only tunnel slot.
        let mut silent = Vec::new();
        for _ in 0..3 {
            silent.push(TcpStream::connect(endpoint).await.unwrap());
        }
        let _first = rpc_client(endpoint).await;
        // A second real client is refused at the cap after its HELLO, well
        // before any handshake timeout could explain the close.
        let mut second = TcpStream::connect(endpoint).await.unwrap();
        second.write_all(&hello()).await.unwrap();
        assert!(closed_within(&mut second, Duration::from_millis(500)).await);
        assert_eq!(pair.backend.accepted.load(Ordering::SeqCst), 1);
        for mut stream in silent {
            assert!(closed_within(&mut stream, Duration::from_secs(3)).await);
        }
        assert!(wait_until(Duration::from_secs(3), || proxies.live_connections() == 1).await);
    }
}
