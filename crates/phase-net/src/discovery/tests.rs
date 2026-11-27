// SPDX-License-Identifier: Apache-2.0

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
        if snapshot
            .connections
            .iter()
            .any(|connection| connection.peer_id == peer_id && accepted.contains(&connection.path))
        {
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
    assert_eq!(classify_runtime("wasm"), Some(JobSpecKind::Wasm));
    assert_eq!(classify_runtime("python-3.11"), None);
    assert_eq!(classify_runtime(""), None);
}

#[test]
fn classify_runtime_uses_workload_kinds_not_backend_names() {
    use phase_protocol::JobSpecKind;
    // Substrate neutrality: kinds come from the protocol vocabulary.
    assert_eq!(classify_runtime("inference"), Some(JobSpecKind::Inference));
    assert_eq!(
        classify_runtime("inference-v1"),
        Some(JobSpecKind::Inference)
    );
    assert_eq!(classify_runtime("embedding"), Some(JobSpecKind::Embedding));
    // Backend product names are not workload kinds and are not recognised.
    for backend in [
        "llama-cpp-b3000",
        "mlx-0.31",
        "ollama",
        "Inference",
        "-inference",
    ] {
        assert_eq!(classify_runtime(backend), None, "{backend}");
    }
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
        registrant.register_rendezvous(server_peer, "phase-workers", Some(rendezvous::DEFAULT_TTL)),
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

#[tokio::test]
async fn outbound_only_refuses_direct_tcp_and_udp_listeners() {
    let node = Discovery::new_with_reachability(
        DiscoveryConfig::default(),
        ReachabilityConfig {
            outbound_only: true,
            ..ReachabilityConfig::default()
        },
    )
    .unwrap();
    for address in [
        "/ip4/0.0.0.0/tcp/0",
        "/ip6/::/tcp/0",
        "/ip4/0.0.0.0/udp/0/quic-v1",
        "/ip6/::/udp/0/quic-v1",
    ] {
        assert!(node
            .listen(address)
            .await
            .unwrap_err()
            .to_string()
            .contains("outbound-only"));
    }
    let snapshot = node.reachability_snapshot().await.unwrap();
    assert!(snapshot.outbound_only);
    assert!(snapshot.listen_addresses.is_empty());
    assert!(ReachabilityConfig {
        role: ReachabilityRole::Infrastructure,
        outbound_only: true,
        ..ReachabilityConfig::default()
    }
    .validate()
    .is_err());
}

#[tokio::test]
async fn provider_lookup_deduplicates_and_bounds_untrusted_results() {
    let (reply, receiver) = oneshot::channel();
    let mut pending = PendingProviderQuery {
        reply: ProviderReply::Lookup(reply),
        peers: Vec::new(),
        deadline: tokio::time::Instant::now() + DISCOVERY_OPERATION_TIMEOUT,
    };
    let peer = PeerId::random();
    pending.insert(peer);
    pending.insert(peer);
    assert_eq!(pending.peers.len(), 1);
    for _ in 0..MAX_PROVIDER_PEERS_PER_QUERY * 2 {
        pending.insert(PeerId::random());
    }
    assert_eq!(pending.peers.len(), MAX_PROVIDER_PEERS_PER_QUERY);
    pending.complete(None);
    let peers = receiver.await.unwrap().unwrap();
    assert_eq!(peers.len(), MAX_PROVIDER_PEERS_PER_QUERY);
    assert!(peers.windows(2).all(|peers| peers[0] < peers[1]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opaque_provider_keys_find_a_real_peer_without_model_semantics() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let provider = Discovery::new(DiscoveryConfig::default()).unwrap();
    provider.listen("/ip4/127.0.0.1/tcp/0").await.unwrap();
    let peer = *provider.local_peer_id();
    let mut address = loopback_dial_addr(&provider).await;
    address.push(Protocol::P2p(peer));
    let requester = Discovery::new_with_reachability(
        DiscoveryConfig {
            bootstrap_peers: vec![address.to_string()],
            ..DiscoveryConfig::default()
        },
        ReachabilityConfig {
            outbound_only: true,
            ..ReachabilityConfig::default()
        },
    )
    .unwrap();
    wait_for_peer_path(&requester, peer, &[ReachabilityPath::Direct]).await;
    let key = b"test/opaque-directory/v1".to_vec();
    provider.start_providing_key(key.clone()).await.unwrap();
    let peers = requester.get_provider_peers(key).await.unwrap();
    assert!(peers.contains(&peer), "provider missing from {peers:?}");
    assert!(requester.get_provider_peers(Vec::new()).await.is_err());
    assert!(provider
        .start_providing_key(vec![0; MAX_KAD_RECORD_KEY_BYTES + 1])
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn circuit_relay_survives_bootstrap_dial_with_outbound_only_destination() {
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
    let dialer = Discovery::new_with_reachability(DiscoveryConfig::default(), relay_only_peer)
        .expect("dialer");
    let relay_peer = *relay_node.local_peer_id();

    relay_node
        .listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("relay listen");
    let relay_addr = loopback_dial_addr(&relay_node).await;
    relay_node
        .add_external_address(relay_addr.clone())
        .await
        .expect("confirm relay address");
    let mut bootstrap = relay_addr.clone();
    bootstrap.push(Protocol::P2p(relay_peer));
    // This is the production startup order that lost its reservation:
    // the constructor starts a bootstrap dial, then listen() asks the
    // relay transport for a circuit before that dial has completed.
    let destination = Discovery::new_with_reachability(
        DiscoveryConfig {
            bootstrap_peers: vec![bootstrap.to_string()],
            ..DiscoveryConfig::default()
        },
        ReachabilityConfig {
            outbound_only: true,
            dcutr: false,
            ..ReachabilityConfig::default()
        },
    )
    .expect("outbound-only destination");
    let destination_peer = *destination.local_peer_id();
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
    assert!(destination
        .listen_addrs()
        .await
        .unwrap()
        .iter()
        .all(|address| {
            address
                .iter()
                .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
        }));

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
    // That the minimum idle timeout never bounds negotiation itself is
    // covered against a genuinely stalled peer in
    // `stalled_peer_negotiation_does_not_block_streams_to_healthy_peers`.
    let mut resumed = requester
        .open_blob_stream(server_peer, resume_request)
        .await
        .expect("open resumed blob");
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
        .set_job_relay_stream_handler(Some(Arc::new(move |peer, open, mut controls, frames| {
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
        })))
        .unwrap();

    let job_id = [21; 32];
    // That the minimum idle timeout never bounds negotiation itself is
    // covered against a genuinely stalled peer in
    // `stalled_peer_negotiation_does_not_block_streams_to_healthy_peers`.
    let mut stream = node_a
        .open_job_relay_stream(
            b_peer,
            JobRelayStreamOpen {
                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                job_id,
                payload: b"signed-manifest".to_vec(),
                deadline_unix_ms: unix_time_ms() + 5_000,
                idle_timeout_ms: crate::protocol::JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS,
            },
        )
        .await
        .expect("open live relay stream");

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
        .set_job_relay_stream_handler(Some(Arc::new(move |_peer, open, mut controls, frames| {
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
        })))
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
    // Either endpoint may reach its idle deadline first. The requester
    // reports a transport error, while the serving peer sends a validated
    // Failed frame. Both paths must terminate without successful output.
    if let Some(Ok(frame)) = liveness_failure {
        assert!(matches!(
            frame.kind,
            JobRelayStreamFrameKind::Failed { ref reason }
                if reason == "live relay idle timeout reached"
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), silent_after_acceptance.next_frame(),)
                .await
                .expect("failed peer stream must close")
                .is_none()
        );
    }
    tokio::time::timeout(Duration::from_secs(2), cancel_seen_rx)
        .await
        .expect("server handler receives cancellation and releases resources")
        .expect("cancel signal sender remains live");
}

/// A4: outbound stream negotiation must not be serialized across peers. A
/// peer whose transport handshake stalls (here: a TCP listener that accepts
/// and never speaks) must not delay opening a stream to a healthy peer.
#[tokio::test]
async fn stalled_peer_negotiation_does_not_block_streams_to_healthy_peers() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let node_a = Arc::new(Discovery::new(DiscoveryConfig::default()).expect("node A"));
    let node_b = Discovery::new(DiscoveryConfig::default()).expect("node B");
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
    node_b
        .set_job_relay_stream_handler(Some(Arc::new(move |_peer, open, _controls, frames| {
            Box::pin(async move {
                let _ = frames
                    .send(JobRelayStreamFrame {
                        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                        job_id: open.job_id,
                        sequence: 0,
                        kind: JobRelayStreamFrameKind::Accepted,
                    })
                    .await;
            })
        })))
        .unwrap();

    // A "dead" peer: TCP connects, but the security handshake never answers.
    let blackhole = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind blackhole");
    let blackhole_port = blackhole.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = blackhole.accept().await {
            sockets.push(socket);
        }
    });
    let dead_peer = PeerId::random();
    node_a
        .dial_peer(&format!(
            "/ip4/127.0.0.1/tcp/{blackhole_port}/p2p/{dead_peer}"
        ))
        .await
        .expect("dial starts");

    let open = |job_id: [u8; 32]| JobRelayStreamOpen {
        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
        job_id,
        payload: b"signed-manifest".to_vec(),
        deadline_unix_ms: unix_time_ms() + 6_000,
        idle_timeout_ms: crate::protocol::JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS,
    };
    let stalled_node = node_a.clone();
    let stalled = tokio::spawn(async move {
        stalled_node
            .open_job_relay_stream(dead_peer, open([31; 32]))
            .await
    });
    let stalled_blob_node = node_a.clone();
    let stalled_blob = tokio::spawn(async move {
        let mut request = blob_request([33; 32], 0, b"stalled".to_vec());
        request.idle_timeout_ms = crate::protocol::BLOB_STREAM_MIN_IDLE_TIMEOUT_MS;
        stalled_blob_node.open_blob_stream(dead_peer, request).await
    });
    // Let the stalled negotiations get in first. Both use the minimum idle
    // timeout, so still being pending here also proves the idle budget does
    // not bound negotiation (that is the total deadline's job).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !stalled.is_finished() && !stalled_blob.is_finished(),
        "precondition: negotiation with the dead peer must still be pending"
    );

    let healthy = tokio::time::timeout(
        Duration::from_secs(2),
        node_a.open_job_relay_stream(b_peer, open([32; 32])),
    )
    .await
    .expect("a stalled peer must not hold up negotiation with a healthy peer")
    .expect("healthy stream opens");
    drop(healthy);

    assert!(
        !stalled.is_finished(),
        "the healthy open completed while the dead peer was still negotiating"
    );
    stalled.abort();
    stalled_blob.abort();
    held.abort();
}

// ---------------------------------------------------------------------------
// /phase/shard-rpc/1.0.0 — authorized opaque byte tunnel over real swarms.
// ---------------------------------------------------------------------------

/// Two real swarms over loopback TCP (Noise + yamux), `requester` dialled
/// into `server`.
async fn connected_shard_rpc_pair() -> (Arc<Discovery>, Arc<Discovery>) {
    let requester = Arc::new(Discovery::new(DiscoveryConfig::default()).expect("requester"));
    let server = Arc::new(Discovery::new(DiscoveryConfig::default()).expect("server"));
    let server_peer = *server.local_peer_id();
    server
        .listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("server listen");
    let mut server_addr = loopback_dial_addr(&server).await;
    server_addr.push(Protocol::P2p(server_peer));
    requester
        .dial_peer(&server_addr.to_string())
        .await
        .expect("requester dials server");
    wait_for_peer_path(&requester, server_peer, &[ReachabilityPath::Direct]).await;
    (requester, server)
}

fn shard_rpc_open(session: u8, idle_timeout_ms: u64, max_session_ms: u64) -> ShardRpcOpen {
    ShardRpcOpen {
        schema_version: crate::protocol::SHARD_RPC_SCHEMA_VERSION,
        session_id: [session; 32],
        idle_timeout_ms,
        max_session_ms,
    }
}

/// Server handler that admits every caller onto a fresh in-memory duplex and
/// hands the far end of that duplex (plus the authenticated caller) to the
/// test, so the test plays the server-side local endpoint.
fn duplex_shard_rpc_handler(
    revoked: Option<Arc<tokio::sync::Notify>>,
) -> (
    ShardRpcHandler,
    mpsc::UnboundedReceiver<(PeerId, ShardRpcOpen, tokio::io::DuplexStream)>,
) {
    let (admitted_tx, admitted_rx) = mpsc::unbounded_channel();
    let handler = ShardRpcHandler::new(move |peer, open| {
        let admitted_tx = admitted_tx.clone();
        let revoked = revoked.clone();
        async move {
            let (endpoint_io, test_io) = tokio::io::duplex(256 * 1024);
            admitted_tx
                .send((peer, open, test_io))
                .expect("test keeps the admission channel open");
            let endpoint = ShardRpcLocalEndpoint::new(endpoint_io);
            ShardRpcAdmission::Accept(match revoked {
                Some(revoked) => endpoint.with_revocation(async move { revoked.notified().await }),
                None => endpoint,
            })
        }
    });
    (handler, admitted_rx)
}

fn pseudo_random_bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.max(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_tunnel_carries_bidirectional_bytes_between_real_peers() {
    let _loopback_test = acquire_loopback_test_gate().await;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (requester, server) = connected_shard_rpc_pair().await;
    let requester_peer = *requester.local_peer_id();
    let server_peer = *server.local_peer_id();
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();

    let open = shard_rpc_open(1, 30_000, 60_000);
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, open.clone())
        .await
        .expect("enrolled requester is admitted");
    assert_eq!(tunnel.peer(), server_peer);
    assert_eq!(tunnel.session_id(), open.session_id);
    let (caller, admitted_open, mut server_side) = admitted.recv().await.expect("admission");
    assert_eq!(
        caller, requester_peer,
        "handler sees the authenticated PeerId"
    );
    assert_eq!(admitted_open, open);

    let (splice_io, mut requester_side) = tokio::io::duplex(256 * 1024);
    let requester_session = tokio::spawn(tunnel.splice(splice_io, None));

    // Multi-MiB in both directions at once proves streaming with backpressure
    // (neither duplex nor the yamux window can hold the whole payload).
    let upload = pseudo_random_bytes(3 * 1024 * 1024 + 17, 0x5eed);
    let download = pseudo_random_bytes(2 * 1024 * 1024 + 5, 0xfeed);
    let (upload_len, download_len) = (upload.len(), download.len());
    let (mut requester_read, mut requester_write) = tokio::io::split(requester_side);
    let (mut server_read, mut server_write) = tokio::io::split(server_side);
    let expected_upload = upload.clone();
    let expected_download = download.clone();
    let (sent_up, sent_down, got_up, got_down) =
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                async move {
                    requester_write.write_all(&upload).await.unwrap();
                    requester_write
                },
                async move {
                    server_write.write_all(&download).await.unwrap();
                    server_write
                },
                async move {
                    let mut got = vec![0_u8; upload_len];
                    server_read.read_exact(&mut got).await.unwrap();
                    (server_read, got)
                },
                async move {
                    let mut got = vec![0_u8; download_len];
                    requester_read.read_exact(&mut got).await.unwrap();
                    (requester_read, got)
                },
            )
        })
        .await
        .expect("bidirectional transfer completes");
    assert!(
        got_up.1 == expected_upload,
        "requester -> server bytes arrive intact and in order"
    );
    assert!(
        got_down.1 == expected_download,
        "server -> requester bytes arrive intact and in order"
    );

    // Small request/response round trips after the bulk transfer.
    requester_side = got_down.0.unsplit(sent_up);
    server_side = got_up.0.unsplit(sent_down);
    for round in 0_u8..8 {
        requester_side.write_all(&[round; 9]).await.unwrap();
        let mut request = [0_u8; 9];
        server_side.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [round; 9]);
        server_side.write_all(&[round ^ 0xff; 3]).await.unwrap();
        let mut response = [0_u8; 3];
        requester_side.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [round ^ 0xff; 3]);
    }

    // Closing the requester's local endpoint half-closes the tunnel: the
    // server-side endpoint sees EOF, then closes in turn, ending both sessions.
    requester_side.shutdown().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), server_side.read_to_end(&mut rest))
        .await
        .expect("server endpoint observes EOF")
        .unwrap();
    assert!(rest.is_empty());
    drop(server_side);
    let report = tokio::time::timeout(Duration::from_secs(5), requester_session)
        .await
        .expect("requester session ends")
        .unwrap();
    assert_eq!(report.end, ShardRpcSessionEnd::LocalClosed, "{report:?}");
    assert_eq!(report.bytes_to_remote, upload_len as u64 + 8 * 9);
    assert_eq!(report.bytes_from_remote, download_len as u64 + 8 * 3);
    assert_eq!(report.error, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_tunnel_fails_closed_without_handler_or_on_rejection() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();

    // No handler installed: the stream is dropped before any handshake, so
    // no tunnel can ever be established.
    let error = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(2, 30_000, 60_000))
        .await
        .expect_err("no handler means no tunnel");
    assert!(
        error.to_string().contains("shard rpc"),
        "unexpected error: {error:#}"
    );

    // A refusing handler: the requester gets the bounded reason and no bytes
    // ever reach a local endpoint.
    let handler = ShardRpcHandler::new(|_peer, _open| async {
        ShardRpcAdmission::reject("caller is not enrolled")
    });
    server.set_shard_rpc_handler(Some(handler)).unwrap();
    let error = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(3, 30_000, 60_000))
        .await
        .expect_err("rejected caller gets no tunnel");
    assert!(
        error.to_string().contains("rejected")
            && error.to_string().contains("caller is not enrolled"),
        "unexpected error: {error:#}"
    );

    // Out-of-bounds limits are refused locally, before any stream is opened.
    let error = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(4, 0, 60_000))
        .await
        .expect_err("invalid idle bound");
    assert!(error.to_string().contains("idle timeout"), "{error:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_revocation_and_handler_withdrawal_drop_active_sessions() {
    let _loopback_test = acquire_loopback_test_gate().await;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();
    let revoke = Arc::new(tokio::sync::Notify::new());
    let (handler, mut admitted) = duplex_shard_rpc_handler(Some(revoke.clone()));
    server.set_shard_rpc_handler(Some(handler)).unwrap();

    // 1. The admitting handler's revocation signal fires mid-session.
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(5, 30_000, 60_000))
        .await
        .unwrap();
    let (_, _, mut server_side) = admitted.recv().await.unwrap();
    let (splice_io, mut requester_side) = tokio::io::duplex(64 * 1024);
    let session = tokio::spawn(tunnel.splice(splice_io, None));
    requester_side.write_all(b"ping").await.unwrap();
    let mut ping = [0_u8; 4];
    server_side.read_exact(&mut ping).await.unwrap();
    assert_eq!(&ping, b"ping");

    revoke.notify_waiters();
    // The server-side endpoint is closed and the requester's endpoint sees
    // EOF; a real local endpoint then closes, which ends the session at once.
    let mut server_rest = Vec::new();
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        server_side.read_to_end(&mut server_rest),
    )
    .await
    .expect("server endpoint is closed on revocation");
    let mut rest = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        requester_side.read_to_end(&mut rest),
    )
    .await
    .expect("requester endpoint observes the end of the tunnel")
    .unwrap();
    assert!(rest.is_empty());
    drop(requester_side);
    let report = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("revocation tears the requester session down promptly")
        .unwrap();
    assert_eq!(report.end, ShardRpcSessionEnd::RemoteClosed, "{report:?}");
    assert_eq!(report.bytes_to_remote, 4);

    // 2. Withdrawing the handler revokes every session it admitted, and new
    //    tunnels are refused.
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(6, 30_000, 60_000))
        .await
        .unwrap();
    let (_, _, _server_side) = admitted.recv().await.unwrap();
    let (splice_io, _requester_side) = tokio::io::duplex(64 * 1024);
    let session = tokio::spawn(tunnel.splice(splice_io, None));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!session.is_finished(), "precondition: session is live");
    server.set_shard_rpc_handler(None).unwrap();
    // Even if the local endpoint never closes, the half-close linger bounds
    // how long the remote teardown takes to end this session.
    let report = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("handler withdrawal ends the session")
        .unwrap();
    assert_eq!(report.end, ShardRpcSessionEnd::RemoteClosed, "{report:?}");
    assert!(requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(7, 30_000, 60_000))
        .await
        .is_err());

    // 3. A requester-side revocation ends the session from that end too.
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(8, 30_000, 60_000))
        .await
        .unwrap();
    let (_, _, mut server_side) = admitted.recv().await.unwrap();
    let local_revoke = Arc::new(tokio::sync::Notify::new());
    let waiter = local_revoke.clone();
    let (splice_io, _requester_side) = tokio::io::duplex(64 * 1024);
    let session = tokio::spawn(tunnel.splice(
        splice_io,
        Some(Box::pin(async move { waiter.notified().await })),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    local_revoke.notify_waiters();
    let report = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.end, ShardRpcSessionEnd::Revoked);
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), server_side.read_to_end(&mut rest))
        .await
        .expect("server endpoint sees the requester-side revocation")
        .unwrap();
}

/// `true` once `stream` reports EOF or an error within `within`.
async fn raw_stream_closed(stream: &mut libp2p::swarm::Stream, within: Duration) -> bool {
    let mut byte = [0_u8; 1];
    matches!(
        tokio::time::timeout(within, stream.read(&mut byte)).await,
        Ok(Ok(0)) | Ok(Err(_))
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_serving_side_enforces_bounds_without_requester_cooperation() {
    let _loopback_test = acquire_loopback_test_gate().await;
    use tokio::io::AsyncReadExt as _;

    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();

    // Idle bound. The requester takes the raw substream and never runs a
    // splice (so it enforces nothing): the server alone must reap the silent
    // tunnel, closing both its local endpoint and the substream.
    let idle_ms = crate::protocol::SHARD_RPC_MIN_IDLE_TIMEOUT_MS;
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(9, idle_ms, 60_000))
        .await
        .unwrap();
    let mut raw = tunnel.stream;
    let (_, _, mut server_side) = admitted.recv().await.unwrap();
    let started = tokio::time::Instant::now();
    let mut sink = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), server_side.read_to_end(&mut sink))
        .await
        .expect("server reaps the idle tunnel's local endpoint")
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(idle_ms - 50));
    assert!(
        raw_stream_closed(&mut raw, Duration::from_secs(5)).await,
        "server closes the idle substream"
    );

    // Session bound. The raw requester keeps the tunnel busy; the server must
    // still end it at the lifetime bound.
    let session_ms = crate::protocol::SHARD_RPC_MIN_SESSION_MS;
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(10, 30_000, session_ms))
        .await
        .unwrap();
    let raw = tunnel.stream;
    let (_, _, server_side) = admitted.recv().await.unwrap();
    let started = tokio::time::Instant::now();
    let drain = tokio::spawn(async move {
        let mut server_side = server_side;
        let mut sink = Vec::new();
        let _ = server_side.read_to_end(&mut sink).await;
        sink.len()
    });
    let chatter = tokio::spawn(async move {
        let mut raw = raw;
        while raw.write_all(b"busy").await.is_ok() && raw.flush().await.is_ok() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let drained = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("server ends a busy tunnel at its session bound")
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(session_ms - 50));
    assert!(
        drained > 0,
        "the tunnel was carrying traffic when it expired"
    );
    tokio::time::timeout(Duration::from_secs(5), chatter)
        .await
        .expect("requester writes fail once the server has ended the session")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_traffic_in_either_direction_postpones_the_idle_bound() {
    let _loopback_test = acquire_loopback_test_gate().await;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();
    let idle_ms = crate::protocol::SHARD_RPC_MIN_IDLE_TIMEOUT_MS;
    let tunnel = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(11, idle_ms, 60_000))
        .await
        .unwrap();
    let (_, _, mut server_side) = admitted.recv().await.unwrap();
    let (splice_io, mut requester_side) = tokio::io::duplex(64 * 1024);
    let session = tokio::spawn(tunnel.splice(splice_io, None));

    // 2.5 idle bounds of steady traffic, alternating direction.
    let started = tokio::time::Instant::now();
    let mut round = 0_u8;
    while started.elapsed() < Duration::from_millis(idle_ms * 5 / 2) {
        let mut byte = [0_u8; 1];
        if round.is_multiple_of(2) {
            requester_side.write_all(&[round]).await.unwrap();
            server_side.read_exact(&mut byte).await.unwrap();
        } else {
            server_side.write_all(&[round]).await.unwrap();
            requester_side.read_exact(&mut byte).await.unwrap();
        }
        assert_eq!(byte[0], round);
        round = round.wrapping_add(1);
        tokio::time::sleep(Duration::from_millis(idle_ms / 4)).await;
    }
    assert!(
        !session.is_finished(),
        "an active tunnel must outlive its idle bound"
    );

    // Silence: now it is reaped.
    let report = tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("tunnel is reaped once traffic stops")
        .unwrap();
    assert!(
        matches!(
            report.end,
            ShardRpcSessionEnd::IdleTimeout | ShardRpcSessionEnd::RemoteClosed
        ),
        "{report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_global_session_and_outbound_caps_fail_closed() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();

    // Global inbound session cap: other peers hold every session slot. The
    // caller is admitted by the handler but refused a session, and the
    // endpoint the handler produced is dropped.
    let held: Vec<_> = (0..MAX_INBOUND_SHARD_RPC_SESSIONS)
        .map(|_| {
            server
                .shard_rpc_sessions
                .try_acquire(PeerId::random())
                .expect("slot")
        })
        .collect();
    let refused = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(12, 30_000, 60_000))
        .await
        .expect_err("no session slot left");
    assert!(
        refused.to_string().contains("session limit reached"),
        "{refused:#}"
    );
    let (_, _, mut orphan) = admitted.recv().await.unwrap();
    let mut rest = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::AsyncReadExt::read_to_end(&mut orphan, &mut rest),
    )
    .await
    .expect("refused endpoint is dropped")
    .unwrap();
    drop(held);
    requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(13, 30_000, 60_000))
        .await
        .expect("a freed slot admits the session");

    // Outbound cap: with every outbound slot taken, no stream is opened.
    let all = requester
        .outbound_shard_rpc_sessions
        .clone()
        .try_acquire_many_owned(MAX_OUTBOUND_SHARD_RPC_SESSIONS as u32)
        .expect("drain outbound slots");
    let error = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(14, 30_000, 60_000))
        .await
        .expect_err("outbound cap");
    assert!(
        error
            .to_string()
            .contains("outbound shard rpc session limit"),
        "{error:#}"
    );
    drop(all);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_withdrawal_during_admission_and_peer_filter_fail_closed() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();

    // A handler that admits only after the test releases it; the handler is
    // withdrawn in between, so the admitted endpoint must not be used.
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let (gate_release, gate_entered) = (release.clone(), entered.clone());
    server
        .set_shard_rpc_handler(Some(ShardRpcHandler::new(move |_peer, _open| {
            let (release, entered) = (gate_release.clone(), gate_entered.clone());
            async move {
                entered.notify_one();
                release.notified().await;
                ShardRpcAdmission::Accept(ShardRpcLocalEndpoint::new(tokio::io::duplex(1024).0))
            }
        })))
        .unwrap();
    let opening = {
        let requester = requester.clone();
        tokio::spawn(async move {
            requester
                .open_shard_rpc_tunnel(server_peer, shard_rpc_open(15, 30_000, 60_000))
                .await
        })
    };
    entered.notified().await;
    let (replacement, _replacement_admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(replacement)).unwrap();
    release.notify_one();
    let error = opening
        .await
        .unwrap()
        .expect_err("withdrawn handler's admission must not stand");
    assert!(
        error.to_string().contains("withdrawn during admission"),
        "{error:#}"
    );

    // Peer filter: a filtered peer is dropped before anything is read, so the
    // admission callback never runs and nothing is revealed.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let requester_peer = *requester.local_peer_id();
    server
        .set_shard_rpc_handler(Some(
            ShardRpcHandler::new(move |_peer, _open| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { ShardRpcAdmission::reject("unreachable") }
            })
            .with_peer_filter(move |peer| *peer != requester_peer),
        ))
        .unwrap();
    let error = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(16, 30_000, 60_000))
        .await
        .expect_err("filtered peer gets no tunnel");
    assert!(
        !error.to_string().contains("unreachable"),
        "filtered peers never reach admission: {error:#}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shard_rpc_inbound_sessions_are_bounded_per_peer() {
    let _loopback_test = acquire_loopback_test_gate().await;
    let (requester, server) = connected_shard_rpc_pair().await;
    let server_peer = *server.local_peer_id();
    let (handler, mut admitted) = duplex_shard_rpc_handler(None);
    server.set_shard_rpc_handler(Some(handler)).unwrap();

    let mut sessions = Vec::new();
    for index in 0..MAX_INBOUND_SHARD_RPC_SESSIONS_PER_PEER {
        let tunnel = requester
            .open_shard_rpc_tunnel(
                server_peer,
                shard_rpc_open(20 + index as u8, 30_000, 60_000),
            )
            .await
            .unwrap_or_else(|error| panic!("session {index} within the cap: {error:#}"));
        let (_, _, server_side) = admitted.recv().await.unwrap();
        let (splice_io, requester_side) = tokio::io::duplex(1024);
        sessions.push((
            tokio::spawn(tunnel.splice(splice_io, None)),
            requester_side,
            server_side,
        ));
    }
    let over_cap = requester
        .open_shard_rpc_tunnel(server_peer, shard_rpc_open(99, 30_000, 60_000))
        .await;
    assert!(
        over_cap.is_err(),
        "the per-peer inbound cap admits no extra session"
    );

    // Releasing one session frees exactly one slot.
    let (session, requester_side, server_side) = sessions.pop().unwrap();
    drop(requester_side);
    drop(server_side);
    tokio::time::timeout(Duration::from_secs(5), session)
        .await
        .expect("released session ends")
        .unwrap();
    let mut reopened = None;
    for _ in 0..50 {
        match requester
            .open_shard_rpc_tunnel(server_peer, shard_rpc_open(98, 30_000, 60_000))
            .await
        {
            Ok(tunnel) => {
                reopened = Some(tunnel);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(reopened.is_some(), "a freed slot admits a new session");
    for (session, _, _) in sessions {
        session.abort();
    }
}

/// `phase-net` stays a workload-neutral substrate: no inference-backend or
/// model-format names in its non-test sources, and unsafe code stays denied.
#[test]
fn phase_net_sources_stay_workload_neutral_and_deny_unsafe_code() {
    let sources = [
        ("lib.rs", include_str!("../lib.rs")),
        ("peer.rs", include_str!("../peer.rs")),
        ("protocol.rs", include_str!("../protocol.rs")),
        ("discovery/mod.rs", include_str!("mod.rs")),
    ];
    // Assembled at runtime so this file does not itself contain the needles.
    let forbidden: Vec<String> = [["lla", "ma"], ["gg", "ml"], ["ml", "x"], ["gg", "uf"]]
        .iter()
        .map(|parts| parts.concat())
        .collect();
    for (name, source) in sources {
        let lowered = source.to_ascii_lowercase();
        for needle in &forbidden {
            assert!(
                !lowered.contains(needle.as_str()),
                "phase-net/src/{name} names workload backend {needle:?}"
            );
        }
    }
    assert!(
        include_str!("../lib.rs").contains("#![deny(unsafe_code)]"),
        "phase-net must keep #![deny(unsafe_code)]"
    );
}
