// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::echo::EchoWorker;
use crate::policy::{PolicyConfig, PolicyState};
use crate::registry::{DhtTransport, ModelCapabilities, ModelCid, ModelRegistry};
use crate::reputation::OperatorOverride;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

/// In-memory DHT mock identical in spirit to the one in `registry.rs`.
/// Used here to exercise `Router::route` without spinning up libp2p.
#[derive(Default)]
struct MockDht {
    store: StdMutex<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
}
#[async_trait]
impl DhtTransport for MockDht {
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(value);
        Ok(())
    }
    async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default())
    }
}

fn sample_caps(model_id: &str, cid_byte: u8) -> ModelCapabilities {
    ModelCapabilities::now(
        model_id,
        ModelCid([cid_byte; 32]),
        "Q4_K_M",
        32_768,
        4,
        "llama.cpp",
    )
}

fn loaded_test_model_cid() -> String {
    ModelCid([1; 32]).to_hex()
}

/// Build a router with a local registry that knows about one model
/// (`qwen3-mini`) and an EchoWorker as the local backend. The
/// `phase_net` handle is unused on routing decisions when the model
/// is local — we can build a real `Discovery` for it but the unit
/// tests stay fast by side-stepping libp2p entirely. For these
/// tests we need *some* `Arc<Discovery>`; we use `Discovery::new`
/// with the default config but never actually drive any commands
/// across it (mDNS may fail in CI; routing decisions don't touch
/// the swarm).
async fn make_router_with_local_model() -> (Router, Arc<ModelRegistry>) {
    let identity = NodeIdentity::generate();
    let transport: Arc<dyn DhtTransport> = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport));
    registry
        .advertise_loaded(sample_caps("qwen3-mini", 1))
        .await
        .expect("advertise");

    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let worker: Arc<dyn DynWorker> = Arc::new(EchoWorker::new());

    let phase_net = build_test_discovery();
    let router = Router::new(Some(worker), registry.clone(), policy, identity, phase_net);
    (router, registry)
}

/// Construct a Discovery handle that tests can hold without driving
/// any commands through it. mDNS may be denied in restricted CI
/// environments — fall back to a sentinel that we never actually
/// touch (routing decisions don't call into phase-net unless we
/// hit the peer-relay path).
fn build_test_discovery() -> Arc<Discovery> {
    match Discovery::new(phase_net::DiscoveryConfig::default()) {
        Ok(d) => Arc::new(d),
        Err(_) => {
            // Tests that only exercise `route()` don't touch the
            // swarm. Make a second attempt with no-op behaviour
            // disabled. If even this fails, the host environment
            // is unsuitable for tests that need a Discovery handle;
            // the tests assert on routing logic so we panic late.
            panic!("Discovery::new failed twice; libp2p stack unavailable in this env");
        }
    }
}

#[tokio::test]
async fn route_local_when_model_loaded_locally() {
    let (router, _registry) = make_router_with_local_model().await;
    let decision = router.route("qwen3-mini", false).await;
    assert!(
        matches!(decision.via, RouteVia::Local),
        "expected Local, got {:?}",
        decision.via
    );
    assert_eq!(decision.header_value().as_deref(), Some("local"));
}

#[tokio::test]
async fn latest_and_bare_aliases_select_the_exact_local_cid_before_discovery() {
    let identity = NodeIdentity::generate();
    let transport = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport.clone()));
    let remote = ModelRegistry::new(NodeIdentity::generate(), transport);
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let router = Router::new(
        Some(Arc::new(EchoWorker::new())),
        registry.clone(),
        policy,
        identity,
        build_test_discovery(),
    )
    .with_remote_access(None, true);
    let tagged = sample_caps("local-tag:latest", 2);
    let bare = sample_caps("local-tag", 3);
    remote.advertise_loaded(bare.clone()).await.unwrap();
    registry.advertise_loaded(tagged.clone()).await.unwrap();
    registry
        .advertise_loaded(sample_caps("qwen3-mini", 1))
        .await
        .unwrap();

    for local_only in [true, false] {
        for (requested, cid) in [
            ("local-tag", tagged.model_cid),
            ("LOCAL-TAG:latest", tagged.model_cid),
            ("qwen3-mini:latest", ModelCid([1; 32])),
        ] {
            let (decision, selected) = router.route_with_cid(requested, local_only).await;
            assert!(matches!(decision.via, RouteVia::Local));
            assert_eq!(selected, Some(cid));
        }
    }

    registry.advertise_loaded(bare.clone()).await.unwrap();
    for (requested, cid) in [
        ("local-tag", bare.model_cid),
        ("local-tag:latest", tagged.model_cid),
    ] {
        let (decision, selected) = router.route_with_cid(requested, false).await;
        assert!(matches!(decision.via, RouteVia::Local));
        assert_eq!(selected, Some(cid));
    }
}

#[tokio::test(start_paused = true)]
async fn enrollment_allows_its_issuer_to_serve_the_consumers_requests() {
    let temp = tempfile::tempdir().unwrap();
    let consumer_identity = NodeIdentity::generate();
    let issuer_identity = NodeIdentity::generate();
    let issuer_peer = peer_id_of(&issuer_identity);
    let transport = Arc::new(MockDht::default());
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let consumer = ConsumerControl::open(
        consumer_identity.clone(),
        transport.clone(),
        policy.clone(),
        temp.path().join("consumer.json"),
        temp.path().join("consumer.token"),
    )
    .unwrap();
    let issuer = ConsumerControl::open(
        issuer_identity.clone(),
        transport.clone(),
        Arc::new(PolicyEngine::new_for_tests(
            PolicyConfig::default(),
            PolicyState::default(),
        )),
        temp.path().join("issuer.json"),
        temp.path().join("issuer.token"),
    )
    .unwrap();
    let issuer_registry = ModelRegistry::new(issuer_identity, transport.clone());
    let caps = sample_caps("enrolled-model", 4);
    issuer_registry
        .advertise_loaded(caps.clone())
        .await
        .unwrap();
    let registry = Arc::new(ModelRegistry::new(consumer_identity.clone(), transport));
    let router = Router::new(
        None,
        registry,
        policy,
        consumer_identity,
        build_test_discovery(),
    )
    .with_remote_access(Some(consumer.clone()), false);
    let cid = caps.model_cid.to_hex();
    assert!(matches!(
        router.route(&cid, false).await.via,
        RouteVia::Refused { .. }
    ));

    let invitation = issuer.create_invitation(900).await.unwrap();
    let server = issuer.spawn_server();
    let status = consumer.enroll(&invitation.invitation).await.unwrap();
    assert_eq!(status.state, "pending");
    tokio::time::timeout(Duration::from_secs(30), async {
        while consumer.enrollment_status().state != "authorized" {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .expect("issuer approves enrollment");
    server.abort();

    let decision = router.route(&cid, false).await;
    assert!(matches!(decision.via, RouteVia::Peer { peer_id } if peer_id == issuer_peer));
    assert!(decision.fallback_peers.is_empty());
}

#[tokio::test]
async fn route_refused_when_local_only_and_model_not_local() {
    let (router, _registry) = make_router_with_local_model().await;
    let decision = router.route("qwen3-big", true).await;
    match &decision.via {
        RouteVia::Refused { reason } => {
            assert!(reason.contains("local-only"), "reason: {reason}");
        }
        other => panic!("expected Refused, got {:?}", other),
    }
    assert!(decision.header_value().is_none());
}

#[tokio::test]
async fn route_refused_when_policy_pauses() {
    let identity = NodeIdentity::generate();
    let transport: Arc<dyn DhtTransport> = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport));
    // Add the model locally so the only reason to refuse is policy.
    registry
        .advertise_loaded(sample_caps("qwen3-mini", 1))
        .await
        .unwrap();

    let config = PolicyConfig {
        manual_pause: true,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let worker: Arc<dyn DynWorker> = Arc::new(EchoWorker::new());
    let router = Router::new(
        Some(worker),
        registry,
        policy,
        identity,
        build_test_discovery(),
    );

    let decision = router.route("qwen3-mini", false).await;
    match &decision.via {
        RouteVia::Refused { reason } => {
            assert!(reason.contains("manual"), "reason: {reason}");
        }
        other => panic!("expected Refused, got {:?}", other),
    }
}

#[tokio::test]
async fn open_network_opt_in_allows_dht_advertisers_that_are_refused_by_default() {
    // Build a registry whose DHT mock contains a third-party
    // advertisement for "qwen3-big" but does NOT have the model
    // locally loaded. The router should pick the peer.
    let identity = NodeIdentity::generate();
    let transport = Arc::new(MockDht::default());

    // Pre-publish a foreign advertisement: a second identity signs
    // an advertisement for "qwen3-big" and we drop it into the mock
    // DHT under the right key.
    let foreign = NodeIdentity::generate();
    let foreign_caps = sample_caps("qwen3-big", 9);
    let cid = foreign_caps.model_cid;
    let ad = crate::registry::SignedModelAdvertisement::sign(foreign_caps, &foreign).unwrap();
    let bytes = ad.encode().unwrap();
    transport
        .store
        .lock()
        .unwrap()
        .entry(cid.dht_key())
        .or_default()
        .push(bytes);

    // But the LOCAL registry needs to know about *some* model with
    // id "qwen3-big" so the name→CID mapping resolves. The current
    // `find_peers_by_model_id` only resolves names through the local
    // loaded set (documented limitation in registry.rs). For this
    // test we load "qwen3-big" locally under the same CID so the
    // name resolves, and we explicitly DON'T configure a local
    // worker — that's what consume-only mode looks like.
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport.clone() as _));
    let mut caps_for_local = sample_caps("qwen3-big", 9);
    caps_for_local.model_cid = cid;
    registry.advertise_loaded(caps_for_local).await.unwrap();

    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    // No local worker — consume-only daemon. This forces the router
    // through the peer branch even though the registry knows about
    // the model.
    let router = Router::new(None, registry, policy, identity, build_test_discovery());
    let refused = router.route("qwen3-big", false).await;
    assert!(matches!(
        refused.via,
        RouteVia::Refused { reason } if reason.contains("no authorized peers")
    ));
    assert!(refused.fallback_peers.is_empty());

    let expected = peer_id_of(&foreign);
    let stale_decision = RouteDecision {
        via: RouteVia::Peer { peer_id: expected },
        model_id: "qwen3-big".to_string(),
        fallback_peers: Vec::new(),
        explanation: String::new(),
    };
    let job = inference_manifest(&foreign, &cid.to_hex(), Some(4));
    assert!(matches!(
        router.execute(&stale_decision, job).await,
        Err(RouterError::Refused { .. })
    ));

    let router = router.with_remote_access(None, true);
    let decision = router.route("qwen3-big", false).await;
    match &decision.via {
        RouteVia::Peer { peer_id } => {
            assert_eq!(*peer_id, expected);
            // Header value should be "peer:<short>".
            let hv = decision.header_value().unwrap();
            assert!(hv.starts_with("peer:"), "header: {hv}");
        }
        other => panic!("expected Peer, got {:?}", other),
    }
}

#[tokio::test]
async fn route_carries_fallback_peers() {
    // Multiple peers advertise the SAME model id (different signer
    // identities → different PeerIds). The router must pick the first as
    // primary and carry the rest as ranked fallbacks for relay failover.
    let identity = NodeIdentity::generate();
    let transport = Arc::new(MockDht::default());

    // Two foreign nodes both advertise "qwen3-big" under the same CID.
    let foreign_a = NodeIdentity::generate();
    let foreign_b = NodeIdentity::generate();
    let unapproved = NodeIdentity::generate();
    let caps = sample_caps("qwen3-big", 9);
    let cid = caps.model_cid;
    for foreign in [&foreign_a, &foreign_b, &unapproved] {
        let ad =
            crate::registry::SignedModelAdvertisement::sign(sample_caps("qwen3-big", 9), foreign)
                .unwrap();
        let bytes = ad.encode().unwrap();
        transport
            .store
            .lock()
            .unwrap()
            .entry(cid.dht_key())
            .or_default()
            .push(bytes);
    }

    // The local registry needs the name→CID mapping (resolved through the
    // loaded set, a documented registry.rs limitation), so load the model
    // locally under the same CID. No local worker → consume-only mode
    // forces the peer branch.
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport.clone() as _));
    let mut caps_for_local = sample_caps("qwen3-big", 9);
    caps_for_local.model_cid = cid;
    registry.advertise_loaded(caps_for_local).await.unwrap();

    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let temp = tempfile::tempdir().unwrap();
    let evidence = Arc::new(
        EvidenceRuntime::open(temp.path().join("evidence.log"), peer_id_of(&identity)).unwrap(),
    );
    let approved = [peer_id_of(&foreign_a), peer_id_of(&foreign_b)];
    for peer in approved {
        evidence
            .set_operator_override(
                peer,
                OperatorOverride {
                    pinned: true,
                    blocked: false,
                },
            )
            .unwrap();
    }
    let router = Router::new(None, registry, policy, identity, build_test_discovery())
        .with_evidence_runtime(evidence.clone());

    let decision = router.route("qwen3-big", false).await;
    match &decision.via {
        RouteVia::Peer { peer_id } => {
            assert!(approved.contains(peer_id));
            assert_eq!(
                decision.fallback_peers,
                approved
                    .into_iter()
                    .filter(|id| id != peer_id)
                    .collect::<Vec<_>>()
            );
        }
        other => panic!("expected Peer, got {:?}", other),
    }
    evidence
        .set_operator_override(
            approved[0],
            OperatorOverride {
                pinned: true,
                blocked: true,
            },
        )
        .unwrap();
    let decision = router.route("qwen3-big", false).await;
    assert!(matches!(decision.via, RouteVia::Peer { peer_id } if peer_id == approved[1]));
    assert!(decision.fallback_peers.is_empty());
}

#[tokio::test]
async fn evidence_order_filters_blocks_and_preserves_one_cold_start_slot() {
    let identity = NodeIdentity::generate();
    let observer = peer_id_of(&identity);
    let transport: Arc<dyn DhtTransport> = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport));
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let temp = tempfile::tempdir().unwrap();
    let runtime =
        Arc::new(EvidenceRuntime::open(temp.path().join("route-evidence.log"), observer).unwrap());
    let strong = PeerId::random();
    let weak = PeerId::random();
    let cold = PeerId::random();
    let blocked = PeerId::random();
    runtime
        .set_operator_override(
            blocked,
            OperatorOverride {
                pinned: false,
                blocked: true,
            },
        )
        .unwrap();
    let now = unix_time_ms();
    for index in 0..8_u8 {
        for (peer, outcome, discriminator) in [
            (strong, EvidenceOutcome::VerifiedSuccessfulCompletion, index),
            (
                weak,
                EvidenceOutcome::VerifiedWorkerError,
                index.saturating_add(32),
            ),
        ] {
            runtime
                .record(
                    EvidenceContext {
                        observer_peer_id: observer,
                        remote_peer_id: peer,
                        job_spec_hash: [discriminator.saturating_add(1); 32],
                        job_class: phase_protocol::JobSpecKind::Inference,
                        model_cid: ModelCid([9; 32]),
                        protocol_version: EVIDENCE_PROTOCOL_BATCH.to_string(),
                        software_version: "router-test/1".to_string(),
                        observed_at_unix_ms: now.saturating_sub(u64::from(index)),
                    },
                    outcome,
                    Some([discriminator.saturating_add(1); 32]),
                )
                .await
                .unwrap();
        }
    }
    let router = Router::new(None, registry, policy, identity, build_test_discovery())
        .with_evidence_runtime(runtime);
    let candidates = vec![
        (blocked, sample_caps("qwen3-big", 9)),
        (weak, sample_caps("qwen3-big", 9)),
        (cold, sample_caps("qwen3-big", 9)),
        (strong, sample_caps("qwen3-big", 9)),
    ];

    let (ranked, explanation) = router.rank_remote_candidates(candidates).await;
    let peers = ranked.iter().map(|(peer, _)| *peer).collect::<Vec<_>>();
    assert_eq!(peers, vec![strong, cold, weak]);
    assert!(!peers.contains(&blocked));
    assert!(explanation.contains("filtered 1 operator-blocked"));
    assert!(explanation.contains("cold-start fallback"));
}

#[tokio::test]
async fn route_refused_when_no_peers_and_not_local() {
    let identity = NodeIdentity::generate();
    let transport: Arc<dyn DhtTransport> = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport));
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let router = Router::new(None, registry, policy, identity, build_test_discovery());
    let decision = router.route("unknown-model", false).await;
    match &decision.via {
        RouteVia::Refused { reason } => {
            assert!(reason.contains("no peers"), "reason: {reason}");
        }
        other => panic!("expected Refused, got {:?}", other),
    }
}

#[tokio::test]
async fn execute_local_streams_events_through_router() {
    // Integration-style: route + execute against the EchoWorker and
    // make sure we see Output frames and a Final.
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{ChatMessage, ChatRole, InferenceJobSpec, JobSpec, SamplingParams};

    let (router, _reg) = make_router_with_local_model().await;
    let decision = router.route("qwen3-mini", false).await;
    assert!(matches!(decision.via, RouteVia::Local));

    let client = NodeIdentity::generate();
    let spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: "qwen3-mini".to_string(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "abc".to_string(),
            images: vec![],
        }],
        prompt: None,
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: Some(512),
        stream: true,
    });
    let manifest = ManifestBuilder::new(spec).sign_with(&client).unwrap();
    let (_handle, mut stream, verification) = router.execute(&decision, manifest).await.unwrap();
    assert_eq!(verification, ReceiptVerification::Local);
    let mut saw_output = false;
    let mut saw_final = false;
    while let Some(ev) = futures::StreamExt::next(&mut stream).await {
        match ev {
            JobEvent::Output(_) => saw_output = true,
            JobEvent::Final { .. } => saw_final = true,
            _ => {}
        }
    }
    assert!(saw_output, "expected at least one Output event");
    assert!(saw_final, "expected a terminal Final event");
}

// --- SEC-01: inbound relay handler authorization ----------------------

/// A spy worker that records how many times `execute` was invoked. Used
/// to prove the authz gate rejects *before* any worker dispatch.
#[derive(Clone)]
struct SpyWorker {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    inner: EchoWorker,
}
impl SpyWorker {
    fn new() -> Self {
        Self {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            inner: EchoWorker::new(),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl phase_protocol::Worker for SpyWorker {
    fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
        // Mirror EchoWorker — the inner backend serves both kinds, and the
        // relay tests now feed it embedding jobs too.
        &[
            phase_protocol::JobSpecKind::Inference,
            phase_protocol::JobSpecKind::Embedding,
        ]
    }
    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.execute(job).await
    }
}

fn inference_manifest(
    client: &NodeIdentity,
    model_id: &str,
    max_tokens: Option<u32>,
) -> SignedManifest<JobSpec> {
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{ChatMessage, ChatRole, InferenceJobSpec, SamplingParams};
    let spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: model_id.to_string(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "hi".to_string(),
            images: vec![],
        }],
        prompt: None,
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: max_tokens.or(Some(512)),
        stream: true,
    });
    ManifestBuilder::new(spec)
        .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
        .sign_with(client)
        .unwrap()
}

fn deterministic_inference_manifest(
    client: &NodeIdentity,
    model_id: &str,
) -> SignedManifest<JobSpec> {
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{ChatMessage, ChatRole, InferenceJobSpec, SamplingParams};
    let mut sampling = SamplingParams::default();
    sampling.params.insert("seed".to_string(), "7".to_string());
    sampling
        .params
        .insert("temperature".to_string(), "0".to_string());
    ManifestBuilder::new(JobSpec::Inference(InferenceJobSpec {
        model_cid: model_id.to_string(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "deterministic".to_string(),
            images: vec![],
        }],
        prompt: None,
        resume_from: None,
        sampling,
        max_tokens: Some(16),
        stream: true,
    }))
    .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
    .sign_with(client)
    .unwrap()
}

#[tokio::test]
async fn automatic_redundancy_is_disabled_by_default_and_strictly_bounded_when_enabled() {
    let identity = NodeIdentity::generate();
    let observer = peer_id_of(&identity);
    let registry = Arc::new(ModelRegistry::new(
        identity.clone(),
        Arc::new(MockDht::default()) as Arc<dyn DhtTransport>,
    ));
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let temp = tempfile::tempdir().unwrap();
    let runtime = Arc::new(
        EvidenceRuntime::open(temp.path().join("redundant-evidence.log"), observer).unwrap(),
    );
    let router = Router::new(None, registry, policy, identity, build_test_discovery())
        .with_evidence_runtime(runtime.clone())
        .with_remote_access(None, true);
    let client = NodeIdentity::generate();
    let deterministic = deterministic_inference_manifest(&client, &loaded_test_model_cid());
    let primary = PeerId::random();
    let duplicate = PeerId::random();

    assert!(router
        .try_begin_redundant_probe(&deterministic, primary, duplicate)
        .is_none());
    assert_eq!(router.redundant_gate.available_permits(), 1);

    let enabled = router
        .clone()
        .with_redundant_verification(RedundantVerificationConfig {
            enabled: true,
            sample_cap_permille: 1_000,
        });
    let nondeterministic = inference_manifest(&client, &loaded_test_model_cid(), Some(16));
    assert!(enabled
        .try_begin_redundant_probe(&nondeterministic, primary, duplicate)
        .is_none());
    assert_eq!(enabled.redundant_gate.available_permits(), 1);

    let embedding = embedding_manifest(&client, &loaded_test_model_cid());
    assert!(enabled
        .try_begin_redundant_probe(&embedding, primary, duplicate)
        .is_none());
    let embedding_error = validate_redundant_job_eligibility(&embedding.payload)
        .expect_err("embedding byte equality is not numeric equivalence");
    assert!(embedding_error
        .to_string()
        .contains("numeric-tolerance and backend-equivalence"));

    assert!(enabled
        .try_begin_redundant_probe(&deterministic, primary, duplicate)
        .is_none());
    for peer in [primary, duplicate] {
        runtime
            .set_operator_override(
                peer,
                OperatorOverride {
                    pinned: true,
                    blocked: false,
                },
            )
            .unwrap();
    }
    assert!(enabled
        .try_begin_redundant_probe(&nondeterministic, primary, duplicate)
        .is_none());
    let permit = enabled
        .try_begin_redundant_probe(&deterministic, primary, duplicate)
        .expect("eligible job gets the sole non-queueing permit");
    assert_eq!(enabled.redundant_gate.available_permits(), 0);
    assert!(enabled
        .try_begin_redundant_probe(&deterministic, primary, duplicate)
        .is_none());
    drop(permit);
    assert_eq!(enabled.redundant_gate.available_permits(), 1);
    assert!(enabled
        .try_begin_redundant_probe(&deterministic, primary, primary)
        .is_none());
}

#[test]
fn redundant_sampling_is_keyed_and_bounded() {
    let identity = NodeIdentity::generate();
    assert!(!redundant_sample_selected(&identity, [0; 32], 0));
    assert!(redundant_sample_selected(&identity, [0; 32], 1_000));
    let selected = (0..=u16::MAX)
        .filter(|prefix| {
            let mut hash = [0; 32];
            hash[..2].copy_from_slice(&prefix.to_be_bytes());
            redundant_sample_selected(&identity, hash, 25)
        })
        .count();
    assert!(
        (1_000..=2_500).contains(&selected),
        "25permille keyed sample should remain near its operator cap; got {selected}"
    );
}

#[test]
fn redundant_disagreement_requires_equivalent_signed_backend_capabilities() {
    let primary = sample_caps("qwen3-mini", 1);
    let mut different_backend = primary.clone();
    different_backend.backend = "mlx".to_string();
    assert!(!capabilities_are_redundancy_equivalent(
        &primary,
        &different_backend
    ));

    let mut different_quantization = primary.clone();
    different_quantization.quantization = "Q8_0".to_string();
    assert!(!capabilities_are_redundancy_equivalent(
        &primary,
        &different_quantization
    ));

    let equivalent = primary.clone();
    assert!(capabilities_are_redundancy_equivalent(
        &primary,
        &equivalent
    ));

    let mut different_context = primary.clone();
    different_context.context_length *= 2;
    assert!(!capabilities_are_redundancy_equivalent(
        &primary,
        &different_context
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_spot_check_compares_two_verified_text_streams() {
    #[derive(Clone)]
    struct ReplayWorker {
        identity: NodeIdentity,
        primary: bool,
        mode: Arc<AtomicUsize>,
    }

    impl phase_protocol::Worker for ReplayWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            &[phase_protocol::JobSpecKind::Inference]
        }

        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            let job_hash = job.manifest_hash().unwrap();
            let mode = self.mode.load(Ordering::Relaxed);
            let text: &[&str] = match (self.primary, mode) {
                (true, _) => &["deterministic ", "answer"],
                (false, 1) => &["different answer"],
                (false, _) => &["deterministic answer"],
            };
            let mut commitment = CommitmentAccumulator::new();
            let mut events = Vec::new();
            for (seq, text) in text.iter().enumerate() {
                let chunk = phase_protocol::OutputChunk {
                    kind: "token".into(),
                    data: bytes::Bytes::from_static(text.as_bytes()),
                    seq: seq as u64,
                };
                commitment.update(&chunk);
                events.push(JobEvent::Output(chunk));
            }
            let (output_commitment, output_chunk_count) = commitment.finalize();
            let result = JobResult {
                job_spec_hash: job_hash,
                output_commitment,
                output_chunk_count,
                completion: Completion::Stop,
                resumption: None,
                metrics: JobMetrics::default(),
            };
            let signer = if !self.primary && mode == 2 {
                NodeIdentity::generate()
            } else {
                self.identity.clone()
            };
            let receipt = phase_receipt::ReceiptBuilder::new(result.clone(), job_hash)
                .sign_with(&signer)
                .unwrap();
            events.push(JobEvent::Final {
                result,
                error: None,
            });
            let (handle, mut producer) = JobHandle::new(JobId(job_hash));
            producer.deliver_receipt(receipt);
            Ok((handle, Box::pin(futures::stream::iter(events))))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let evidence_path = dir.path().join("spot-check.log");
    let client = NodeIdentity::generate();
    let client_net = discovery_for(&client);
    let observer = *client_net.local_peer_id();
    let runtime = Arc::new(EvidenceRuntime::open(&evidence_path, observer).unwrap());
    let transport = Arc::new(MockDht::default());
    let cid = loaded_test_model_cid();
    let serving_policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            authorized_submitters: vec![hex::encode(client.peer_id_bytes())],
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let mode = Arc::new(AtomicUsize::new(0));
    let mut providers = Vec::new();
    for primary in [true, false] {
        let identity = NodeIdentity::generate();
        let net = discovery_for(&identity);
        let registry = Arc::new(ModelRegistry::new(identity.clone(), transport.clone()));
        registry
            .advertise_loaded(sample_caps("qwen3-mini", 1))
            .await
            .unwrap();
        net.set_job_relay_stream_handler(Some(make_inbound_relay_stream_handler(
            Arc::new(ReplayWorker {
                identity,
                primary,
                mode: mode.clone(),
            }),
            registry,
            serving_policy.clone(),
        )))
        .unwrap();
        dial_loopback_fixture(&client_net, &net).await;
        runtime
            .set_operator_override(
                *net.local_peer_id(),
                OperatorOverride {
                    pinned: true,
                    blocked: false,
                },
            )
            .unwrap();
        providers.push(net);
    }
    let primary = *providers[0].local_peer_id();
    let validator = *providers[1].local_peer_id();
    let router = Router::new(
        None,
        Arc::new(ModelRegistry::new(client.clone(), transport)),
        Arc::new(PolicyEngine::new_for_tests(
            PolicyConfig::default(),
            PolicyState::default(),
        )),
        client.clone(),
        client_net,
    )
    .with_evidence_runtime(runtime)
    .with_redundant_verification(RedundantVerificationConfig {
        enabled: true,
        sample_cap_permille: 1_000,
    });
    let decision = RouteDecision {
        via: RouteVia::Peer { peer_id: primary },
        model_id: cid.clone(),
        fallback_peers: vec![validator],
        explanation: "test providers".into(),
    };
    for case in 0..=2 {
        mode.store(case, Ordering::Relaxed);
        let job = deterministic_inference_manifest(&client, &cid);
        let ((handle, mut stream, verification), check) = router
            .execute_with_spot_check(&decision, job)
            .await
            .unwrap();
        assert_eq!(verification, ReceiptVerification::Pending);
        while futures::StreamExt::next(&mut stream).await.is_some() {}
        let receipt = handle.finish().await.unwrap();
        let verdict = timeout(Duration::from_secs(5), check.unwrap())
            .await
            .unwrap()
            .unwrap();
        match case {
            0 => assert_eq!(
                verdict,
                RedundantCheckResult::Agreement {
                    commitment: receipt.result.output_commitment
                }
            ),
            1 => assert!(matches!(verdict, RedundantCheckResult::Disagreement { .. })),
            _ => assert_eq!(verdict, RedundantCheckResult::Incomparable),
        }
    }
    drop(router);
    let store = crate::reputation::EvidenceStore::open(&evidence_path, observer).unwrap();
    let spot_checks: Vec<_> = store
        .records()
        .iter()
        .filter(|record| record.protocol_version() == SPOT_CHECK_METHOD)
        .collect();
    assert_eq!(spot_checks.len(), 2);
    let EvidenceOutcome::VerifiedSpotCheckAgreement(agreement) = spot_checks[0].outcome() else {
        panic!("expected paired agreement evidence");
    };
    assert_eq!(agreement.validator_peer, validator);
    assert_ne!(
        spot_checks[0].output_commitment(),
        Some(agreement.validator_commitment)
    );
    assert_eq!(
        agreement.primary_text_digest,
        agreement.validator_text_digest
    );
    assert!(matches!(
        spot_checks[1].outcome(),
        EvidenceOutcome::VerifiedSpotCheckDisagreement(_)
    ));
}

#[test]
fn remote_failure_classification_preserves_evidence_taxonomy() {
    let cases = [
        (
            "remote does not support live relay",
            EvidenceOutcome::PreOutputDiscoveryFailure,
        ),
        ("peer refused: busy", EvidenceOutcome::CapacityRefusal),
        (
            "peer refused: operator policy",
            EvidenceOutcome::PolicyRefusal,
        ),
        (
            "live relay decision timed out",
            EvidenceOutcome::DeadlineTimeout,
        ),
        (
            "peer returned no terminal receipt",
            EvidenceOutcome::MissingReceipt,
        ),
        (
            "receipt signature verification failed",
            EvidenceOutcome::InvalidReceiptSignature,
        ),
        (
            "receipt worker key does not match PeerId",
            EvidenceOutcome::SignerPeerIdMismatch,
        ),
        (
            "receipt job_id does not match",
            EvidenceOutcome::JobMismatch,
        ),
        (
            "receipt job_spec_hash does not match manifest",
            EvidenceOutcome::ManifestMismatch,
        ),
        (
            "delivered output chunk count does not match",
            EvidenceOutcome::ChunkCountMismatch,
        ),
        (
            "delivered output does not match terminal commitment",
            EvidenceOutcome::OutputCommitmentMismatch,
        ),
        (
            "duplicate terminal event",
            EvidenceOutcome::SequenceMismatch,
        ),
        ("decode peer events", EvidenceOutcome::SequenceMismatch),
        (
            "missing terminal event",
            EvidenceOutcome::MissingTerminalEvent,
        ),
    ];
    for (reason, expected) in cases {
        assert_eq!(
            classify_relay_failure(&RouterError::Relay(reason.to_string()), false),
            expected,
            "classification for {reason:?}"
        );
    }
    assert_eq!(
        classify_relay_failure(&RouterError::Relay("connection reset".to_string()), false),
        EvidenceOutcome::PreOutputTransportFailure
    );
    assert_eq!(
        classify_relay_failure(&RouterError::Relay("connection reset".to_string()), true),
        EvidenceOutcome::MidStreamTransportLoss
    );
}

/// Embedding analogue of `inference_manifest`: a signed `JobSpec::Embedding`
/// for `model_id` with a couple of short inputs.
fn embedding_manifest(client: &NodeIdentity, model_id: &str) -> SignedManifest<JobSpec> {
    use phase_manifest::ManifestBuilder;
    use phase_protocol::EmbeddingJobSpec;
    let spec = JobSpec::Embedding(EmbeddingJobSpec {
        model_cid: model_id.to_string(),
        input: vec!["hello".to_string(), "world".to_string()],
    });
    ManifestBuilder::new(spec)
        .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
        .sign_with(client)
        .unwrap()
}

async fn registry_with_model(model_id: &str) -> Arc<ModelRegistry> {
    let identity = NodeIdentity::generate();
    let transport: Arc<dyn DhtTransport> = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity, transport));
    registry
        .advertise_loaded(sample_caps(model_id, 1))
        .await
        .unwrap();
    registry
}

#[tokio::test]
async fn sec01_relay_rejects_unauthorized_signer_without_dispatch() {
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    // Default config: empty allowlist, allow_unauthenticated = false.
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let attacker = NodeIdentity::generate();
    let manifest = inference_manifest(&attacker, &loaded_test_model_cid(), None);
    let bytes = serde_json::to_vec(&manifest).unwrap();

    let resp = handler(peer_id_of(&attacker), bytes).await;
    match resp {
        JobRelayResponse::Err { reason } => {
            assert!(reason.contains("not authorized"), "reason: {reason}");
        }
        other => panic!("expected Err, got {other:?}"),
    }
    assert_eq!(spy.call_count(), 0, "worker must NOT be dispatched");
}

#[tokio::test]
async fn sec01_relay_accepts_allowlisted_signer() {
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;

    let client = NodeIdentity::generate();
    let manifest = inference_manifest(&client, &loaded_test_model_cid(), None);
    // signer_pubkey is the canonical lowercase-hex the manifest carries.
    let config = PolicyConfig {
        authorized_submitters: vec![manifest.signer_pubkey.clone()],
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let bytes = serde_json::to_vec(&manifest).unwrap();

    let resp = handler(peer_id_of(&client), bytes).await;
    assert!(matches!(resp, JobRelayResponse::Ok { .. }), "got {resp:?}");
    assert_eq!(spy.call_count(), 1, "allowlisted job should dispatch once");
}

#[tokio::test]
async fn sec01_relay_open_mode_accepts_any_verified_signer() {
    // allow_unauthenticated_jobs = true restores pre-SEC-01 open behavior
    // (local dev / demos). Any verified manifest dispatches.
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    let config = PolicyConfig {
        allow_unauthenticated_jobs: true,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let anyone = NodeIdentity::generate();
    let manifest = inference_manifest(&anyone, &loaded_test_model_cid(), None);
    let bytes = serde_json::to_vec(&manifest).unwrap();

    let resp = handler(PeerId::random(), bytes).await;
    assert!(matches!(resp, JobRelayResponse::Ok { .. }), "got {resp:?}");
    assert_eq!(spy.call_count(), 1);
}

#[tokio::test]
async fn relay_accepts_embedding_job() {
    // Embedding jobs relay just like inference: open mode, model loaded,
    // the inbound handler dispatches and returns Ok. Guards canonical CID
    // matching + prompt-cap arms that learned about JobSpec::Embedding.
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    let config = PolicyConfig {
        allow_unauthenticated_jobs: true,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let client = NodeIdentity::generate();
    let manifest = embedding_manifest(&client, &loaded_test_model_cid());
    let bytes = serde_json::to_vec(&manifest).unwrap();

    let resp = handler(PeerId::random(), bytes).await;
    assert!(
        matches!(resp, JobRelayResponse::Ok { .. }),
        "embedding job should relay, got {resp:?}"
    );
    assert_eq!(spy.call_count(), 1, "embedding job should dispatch once");
}

#[tokio::test]
async fn relay_cid_gate_rejects_alias_and_name_derived_fallback() {
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            allow_unauthenticated_jobs: true,
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let handler = make_inbound_relay_handler(worker, registry, policy);
    let client = NodeIdentity::generate();

    let cases = [
        ("qwen3-mini".to_string(), "invalid remote model_cid"),
        (
            ModelCid::development_name_hash("qwen3-mini").to_hex(),
            "not loaded on this peer",
        ),
    ];
    for (model_cid, expected_reason) in cases {
        let manifest = inference_manifest(&client, &model_cid, None);
        let response = handler(PeerId::random(), serde_json::to_vec(&manifest).unwrap()).await;
        match response {
            JobRelayResponse::Err { reason } => assert!(
                reason.contains(expected_reason),
                "expected {expected_reason:?}, got {reason:?}"
            ),
            other => panic!("expected CID-gate refusal, got {other:?}"),
        }
    }
    assert_eq!(spy.call_count(), 0, "CID failures must not dispatch");
}

#[tokio::test]
async fn sec01_relay_rejects_max_tokens_above_ceiling() {
    // A signed manifest cannot be safely mutated after verification. A
    // value above the operator ceiling is rejected before dispatch.
    let ceiling = 256u32;
    let captured: Arc<StdMutex<Option<u32>>> = Arc::new(StdMutex::new(None));

    #[derive(Clone)]
    struct CaptureWorker {
        captured: Arc<StdMutex<Option<u32>>>,
        inner: EchoWorker,
    }
    impl phase_protocol::Worker for CaptureWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            &[phase_protocol::JobSpecKind::Inference]
        }
        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            if let JobSpec::Inference(spec) = &job.payload {
                *self.captured.lock().unwrap() = Some(spec.max_tokens.unwrap_or(0));
            }
            self.inner.execute(job).await
        }
    }

    let worker: Arc<dyn DynWorker> = Arc::new(CaptureWorker {
        captured: captured.clone(),
        inner: EchoWorker::new(),
    });
    let registry = registry_with_model("qwen3-mini").await;
    let client = NodeIdentity::generate();
    let manifest = inference_manifest(&client, &loaded_test_model_cid(), Some(u32::MAX));
    let config = PolicyConfig {
        authorized_submitters: vec![manifest.signer_pubkey.clone()],
        max_tokens_ceiling: ceiling,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let bytes = serde_json::to_vec(&manifest).unwrap();
    let resp = handler(peer_id_of(&client), bytes).await;
    match resp {
        JobRelayResponse::Err { reason } => assert!(reason.contains("max_tokens")),
        other => panic!("expected max_tokens refusal, got {other:?}"),
    }
    assert_eq!(*captured.lock().unwrap(), None, "worker must not dispatch");
}

#[test]
fn header_value_local_and_peer_shapes() {
    let d = RouteDecision {
        via: RouteVia::Local,
        model_id: "x".into(),
        fallback_peers: Vec::new(),
        explanation: "test fixture".into(),
    };
    assert_eq!(d.header_value().as_deref(), Some("local"));

    let sharded = RouteDecision {
        via: RouteVia::TrustedCircleSharded,
        model_id: "x".into(),
        fallback_peers: Vec::new(),
        explanation: "test fixture".into(),
    };
    assert_eq!(
        sharded.header_value().as_deref(),
        Some("trusted-circle-sharded")
    );

    // Peer header should be `peer:<8 chars>`.
    let identity = NodeIdentity::generate();
    let pubkey = identity.verifying_key().to_bytes();
    use phase_net::libp2p_identity::{ed25519, PublicKey};
    let ed = ed25519::PublicKey::try_from_bytes(&pubkey).unwrap();
    let pk: PublicKey = ed.into();
    let peer = PeerId::from(pk);
    let d = RouteDecision {
        via: RouteVia::Peer { peer_id: peer },
        model_id: "x".into(),
        fallback_peers: Vec::new(),
        explanation: "test fixture".into(),
    };
    let hv = d.header_value().unwrap();
    assert!(hv.starts_with("peer:"), "got {hv}");
    // 5 for "peer:" + 8 short id = 13.
    assert_eq!(hv.len(), 13, "got {hv}");
}

#[tokio::test]
async fn rpc_offload_worker_refuses_local_only_and_reports_trusted_circle_sharded() {
    let identity = NodeIdentity::generate();
    let transport = Arc::new(MockDht::default());
    let registry = Arc::new(ModelRegistry::new(identity.clone(), transport));
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    registry
        .advertise_loaded(sample_caps("gemma-4-12b", 9))
        .await
        .unwrap();

    let router = Router::new(
        Some(Arc::new(EchoWorker::new())),
        registry,
        policy,
        identity,
        build_test_discovery(),
    )
    .with_local_rpc_offload(true);

    // X-Lucid-Local-Only must fail closed when the worker offloads layers over RPC.
    let local_only = router.route("gemma-4-12b", true).await;
    match &local_only.via {
        RouteVia::Refused { reason } => {
            assert!(
                reason.contains("trusted-circle RPC"),
                "expected RPC refusal reason, got: {reason}"
            );
        }
        other => panic!("expected Refused for local_only RPC worker, got {other:?}"),
    }
    assert!(local_only.header_value().is_none());

    // Normal request reports trusted-circle-sharded rather than local.
    let normal = router.route("gemma-4-12b", false).await;
    assert!(
        matches!(normal.via, RouteVia::TrustedCircleSharded),
        "expected TrustedCircleSharded, got {:?}",
        normal.via
    );
    assert_eq!(
        normal.header_value().as_deref(),
        Some("trusted-circle-sharded")
    );
}

// --- SEC-05: receipt verify + bind -----------------------------------

/// Derive the libp2p PeerId an identity's Ed25519 key maps to — the same
/// transform `worker_pubkey_to_peer_id` performs, used by tests to compute
/// the "dispatched-to" PeerId for the bind check.
fn peer_id_of(identity: &NodeIdentity) -> PeerId {
    use phase_net::libp2p_identity::{ed25519, PublicKey};
    let ed = ed25519::PublicKey::try_from_bytes(&identity.verifying_key().to_bytes()).unwrap();
    let pk: PublicKey = ed.into();
    PeerId::from(pk)
}

/// Run the inbound relay handler against an EchoWorker with a *known*
/// worker identity (so tests can compute its PeerId) in open mode, and
/// return the `(events, receipt)` byte vecs plus the dispatched
/// manifest_hash — exactly what `execute_via_peer` would receive.
async fn relay_round_trip(
    worker_identity: &NodeIdentity,
    client: &NodeIdentity,
    model_id: &str,
) -> (Vec<u8>, Vec<u8>, [u8; 32]) {
    let worker: Arc<dyn DynWorker> = Arc::new(crate::echo::EchoWorker {
        token_delay: std::time::Duration::from_millis(0),
        identity: worker_identity.clone(),
    });
    let registry = registry_with_model(model_id).await;
    let config = PolicyConfig {
        allow_unauthenticated_jobs: true,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let manifest = inference_manifest(client, &loaded_test_model_cid(), Some(8));
    let manifest_hash = manifest.manifest_hash().unwrap();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let resp = handler(PeerId::random(), bytes).await;
    match resp {
        JobRelayResponse::Ok { events, receipt } => (events, receipt, manifest_hash),
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn assert_batch_rejected(
    receipt: &[u8],
    events: &[JobEvent],
    manifest_hash: [u8; 32],
    worker_peer: PeerId,
    case: &str,
) {
    assert!(
        require_verified_peer_batch_receipt(receipt, events, manifest_hash, worker_peer).is_err(),
        "legacy execute boundary accepted {case}"
    );
}

#[tokio::test]
async fn sec05_verified_receipt_binds_to_job_and_peer() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();

    let dispatched_peer = peer_id_of(&worker_id);
    let v = verify_peer_receipt(&receipt_b, &events, manifest_hash, dispatched_peer);
    assert_eq!(
        v,
        ReceiptVerification::Verified,
        "honest round-trip must verify"
    );
}

#[tokio::test]
async fn sec05_wrong_job_id_is_detected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, _manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();

    // Bind against a DIFFERENT job id than the one the receipt was signed
    // over → must fail.
    let wrong_hash = [0x42u8; 32];
    let v = verify_peer_receipt(&receipt_b, &events, wrong_hash, peer_id_of(&worker_id));
    assert_eq!(
        v,
        ReceiptVerification::Failed,
        "job_id mismatch must be detected"
    );
}

#[tokio::test]
async fn sec05_wrong_worker_key_is_detected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();

    // The receipt is a valid signature by `worker_id`, but we claim we
    // dispatched to a DIFFERENT peer → worker-pubkey→PeerId bind fails.
    let impostor_peer = peer_id_of(&NodeIdentity::generate());
    let v = verify_peer_receipt(&receipt_b, &events, manifest_hash, impostor_peer);
    assert_eq!(
        v,
        ReceiptVerification::Failed,
        "receipt from a key not matching the dispatched PeerId must be detected"
    );
}

#[tokio::test]
async fn sec05_commitment_mismatch_is_detected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let mut events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();

    // Tamper with an Output chunk's bytes WITHOUT touching the signed
    // receipt → replayed commitment no longer matches the signature.
    let mut tampered = false;
    for ev in events.iter_mut() {
        if let JobEvent::Output(chunk) = ev {
            chunk.data = bytes::Bytes::from_static(b"tampered-output");
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "round-trip should have produced at least one Output chunk"
    );

    let v = verify_peer_receipt(&receipt_b, &events, manifest_hash, peer_id_of(&worker_id));
    assert_eq!(
        v,
        ReceiptVerification::Failed,
        "tampered output vs signed commitment must be detected"
    );
}

#[tokio::test]
async fn sec05_signed_receipt_result_must_exactly_match_final() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let mut events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();
    let final_result = events.iter_mut().find_map(|event| match event {
        JobEvent::Final { result, .. } => Some(result),
        _ => None,
    });
    final_result.expect("honest Final").completion = Completion::Length;

    assert_batch_rejected(
        &receipt_b,
        &events,
        manifest_hash,
        peer_id_of(&worker_id),
        "signed receipt / Final result mismatch",
    );
}

#[tokio::test]
async fn sec05_signed_receipt_with_wrong_job_spec_hash_is_rejected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();
    let honest_receipt: SignedReceipt<JobResult> =
        serde_json::from_slice(&receipt_b).expect("honest receipt");
    let mut wrong_result = honest_receipt.result;
    wrong_result.job_spec_hash = [0x5au8; 32];
    let wrong_receipt = phase_receipt::ReceiptBuilder::new(wrong_result, manifest_hash)
        .sign_with(&worker_id)
        .expect("sign adversarial receipt");
    let wrong_receipt_bytes = serde_json::to_vec(&wrong_receipt).unwrap();

    assert_batch_rejected(
        &wrong_receipt_bytes,
        &events,
        manifest_hash,
        peer_id_of(&worker_id),
        "signed receipt with wrong job_spec_hash",
    );
}

#[tokio::test]
async fn sec05_duplicate_and_missing_final_are_rejected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();
    let final_event = events
        .iter()
        .find(|event| matches!(event, JobEvent::Final { .. }))
        .expect("honest Final")
        .clone();
    let mut duplicate_final = events.clone();
    duplicate_final.push(final_event);
    assert_batch_rejected(
        &receipt_b,
        &duplicate_final,
        manifest_hash,
        peer_id_of(&worker_id),
        "duplicate Final",
    );

    let mut missing_final = events;
    missing_final.retain(|event| !matches!(event, JobEvent::Final { .. }));
    assert_batch_rejected(
        &receipt_b,
        &missing_final,
        manifest_hash,
        peer_id_of(&worker_id),
        "missing Final",
    );
}

#[tokio::test]
async fn sec05_post_final_output_is_rejected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let mut events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();
    let output = events
        .iter()
        .find(|event| matches!(event, JobEvent::Output(_)))
        .expect("honest Output")
        .clone();
    assert!(
        matches!(events.last(), Some(JobEvent::Final { .. })),
        "fixture must end in Final"
    );
    events.push(output);

    assert_batch_rejected(
        &receipt_b,
        &events,
        manifest_hash,
        peer_id_of(&worker_id),
        "post-Final Output",
    );
}

#[tokio::test]
async fn sec05_gapped_reversed_and_duplicate_output_sequences_are_rejected() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();
    let output_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| matches!(event, JobEvent::Output(_)).then_some(index))
        .collect();
    assert!(
        output_indices.len() >= 2,
        "fixture needs at least two outputs"
    );
    let worker_peer = peer_id_of(&worker_id);

    let mut gapped = events.clone();
    if let JobEvent::Output(chunk) = &mut gapped[output_indices[0]] {
        chunk.seq = 1;
    }
    assert_batch_rejected(
        &receipt_b,
        &gapped,
        manifest_hash,
        worker_peer,
        "gapped output sequence",
    );

    let mut reversed = events.clone();
    reversed.swap(output_indices[0], output_indices[1]);
    assert_batch_rejected(
        &receipt_b,
        &reversed,
        manifest_hash,
        worker_peer,
        "reversed output sequence",
    );

    let mut duplicate = events;
    let duplicate_output = duplicate[output_indices[0]].clone();
    duplicate.insert(output_indices[0] + 1, duplicate_output);
    assert_batch_rejected(
        &receipt_b,
        &duplicate,
        manifest_hash,
        worker_peer,
        "duplicate output sequence",
    );
}

#[tokio::test]
async fn sec05_missing_receipt_is_unverifiable() {
    // Classification preserves the distinction for observability; the
    // execute boundary below still rejects the batch.
    let v = verify_peer_receipt(&[], &[], [0u8; 32], PeerId::random());
    assert_eq!(v, ReceiptVerification::Unverifiable);
}

#[tokio::test]
async fn sec05_missing_receipt_is_a_hard_failure_at_legacy_execute_boundary() {
    let worker_id = NodeIdentity::generate();
    let client = NodeIdentity::generate();
    let (events_b, _receipt_b, manifest_hash) =
        relay_round_trip(&worker_id, &client, "qwen3-mini").await;
    let events: Vec<JobEvent> = serde_json::from_slice(&events_b).unwrap();

    assert_batch_rejected(
        &[],
        &events,
        manifest_hash,
        peer_id_of(&worker_id),
        "missing receipt",
    );
}

// --- SEC-06: DoS caps + PeerID-bind authz ----------------------------

#[tokio::test]
async fn sec06_peer_id_bind_does_not_replace_operator_authorization() {
    // Peer binding proves attribution, not operator authorization. An
    // empty default allowlist must reject even a self-bound signer.
    let client = NodeIdentity::generate();
    let worker: Arc<dyn DynWorker> = Arc::new(crate::echo::EchoWorker {
        token_delay: std::time::Duration::from_millis(0),
        identity: NodeIdentity::generate(),
    });
    let registry = registry_with_model("qwen3-mini").await;
    // Default config: empty allowlist, allow_unauthenticated = false.
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let manifest = inference_manifest(&client, &loaded_test_model_cid(), None);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    // Deliver from the client's own PeerId; default-deny still wins.
    let delivering = peer_id_of(&client);
    let resp = handler(delivering, bytes).await;
    match resp {
        JobRelayResponse::Err { reason } => assert!(reason.contains("not authorized")),
        other => panic!("expected default-deny refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn sec06_peer_id_bind_rejects_mismatched_peer() {
    // Same manifest, but delivered from a DIFFERENT PeerId and not on the
    // allowlist → rejected (no bind, no allowlist).
    let client = NodeIdentity::generate();
    let worker: Arc<dyn DynWorker> = Arc::new(EchoWorker::new());
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let manifest = inference_manifest(&client, &loaded_test_model_cid(), None);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let resp = handler(PeerId::random(), bytes).await;
    match resp {
        JobRelayResponse::Err { reason } => assert!(reason.contains("not authorized")),
        other => panic!("expected Err, got {other:?}"),
    }
}

#[tokio::test]
async fn sec06_oversized_prompt_rejected_before_dispatch() {
    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    let config = PolicyConfig {
        allow_unauthenticated_jobs: true,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    // Build a manifest whose prompt exceeds the server-side cap.
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{InferenceJobSpec, SamplingParams};
    let huge = "x".repeat(MAX_PROMPT_CHARS + 1);
    let client = NodeIdentity::generate();
    let spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: loaded_test_model_cid(),
        messages: vec![],
        prompt: Some(huge),
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: Some(512),
        stream: true,
    });
    let manifest = ManifestBuilder::new(spec)
        .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
        .sign_with(&client)
        .unwrap();
    let bytes = serde_json::to_vec(&manifest).unwrap();

    let resp = handler(PeerId::random(), bytes).await;
    match resp {
        JobRelayResponse::Err { reason } => assert!(reason.contains("prompt too large")),
        other => panic!("expected Err, got {other:?}"),
    }
    assert_eq!(
        spy.call_count(),
        0,
        "oversized prompt must not reach the worker"
    );
}

#[tokio::test]
async fn inbound_embedding_shape_bounds_reject_before_dispatch() {
    use phase_manifest::ManifestBuilder;
    use phase_protocol::EmbeddingJobSpec;

    let spy = SpyWorker::new();
    let worker: Arc<dyn DynWorker> = Arc::new(spy.clone());
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            allow_unauthenticated_jobs: true,
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let handler = make_inbound_relay_handler(worker, registry, policy);
    let client = NodeIdentity::generate();

    for input in [
        Vec::new(),
        vec![String::new()],
        vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS + 1)],
        vec!["x".to_string(); MAX_EMBEDDING_INPUTS + 1],
        vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS); 5],
    ] {
        let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
            model_cid: loaded_test_model_cid(),
            input,
        }))
        .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
        .sign_with(&client)
        .expect("sign embedding");
        let response = handler(
            PeerId::random(),
            serde_json::to_vec(&manifest).expect("encode embedding"),
        )
        .await;
        assert!(matches!(response, JobRelayResponse::Err { .. }));
    }
    assert_eq!(spy.call_count(), 0);
}

#[test]
fn dynamic_remote_concurrency_gate_honors_current_limit() {
    let gate = Arc::new(RemoteConcurrencyGate::default());
    let first = gate.try_acquire(2).expect("first permit");
    let second = gate.try_acquire(2).expect("second permit");
    assert!(gate.try_acquire(2).is_none());

    // A policy reload lowering the limit is represented by the next
    // request supplying 1. Existing work is not killed, but admission is
    // immediately closed until active work drains below the new ceiling.
    assert!(gate.try_acquire(1).is_none());
    drop(second);
    assert!(gate.try_acquire(1).is_none());
    drop(first);
    assert!(gate.try_acquire(1).is_some());
}

#[tokio::test]
async fn sec06_concurrency_cap_rejects_n_plus_one() {
    // A worker that blocks until released, so we can hold N permits and
    // prove the (N+1)th relay is refused busy.
    #[derive(Clone)]
    struct BlockingWorker {
        gate: Arc<tokio::sync::Semaphore>,
        inner: EchoWorker,
    }
    impl phase_protocol::Worker for BlockingWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            &[phase_protocol::JobSpecKind::Inference]
        }
        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            // Block here until the test releases a gate permit, holding the
            // relay's concurrency permit for the duration.
            let _g = self.gate.acquire().await.unwrap();
            self.inner.execute(job).await
        }
    }

    let gate = Arc::new(tokio::sync::Semaphore::new(0)); // start blocked
    let worker: Arc<dyn DynWorker> = Arc::new(BlockingWorker {
        gate: gate.clone(),
        inner: EchoWorker::new(),
    });
    let registry = registry_with_model("qwen3-mini").await;
    let config = PolicyConfig {
        allow_unauthenticated_jobs: true,
        max_concurrent_remote_jobs: 1,
        ..PolicyConfig::default()
    };
    let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
    let handler = make_inbound_relay_handler(worker, registry, policy);

    let client = NodeIdentity::generate();
    let m1 = serde_json::to_vec(&inference_manifest(
        &client,
        &loaded_test_model_cid(),
        Some(4),
    ))
    .unwrap();
    let m2 = serde_json::to_vec(&inference_manifest(
        &client,
        &loaded_test_model_cid(),
        Some(4),
    ))
    .unwrap();

    let h1 = handler.clone();
    // First job: spawn it; it will grab the single permit and block in the
    // worker awaiting the gate.
    let job1 = tokio::spawn(async move { h1(PeerId::random(), m1).await });
    // Give job1 time to acquire the permit and enter the worker.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Second job while the first holds the only permit → busy refusal.
    let resp2 = handler(PeerId::random(), m2).await;
    match resp2 {
        JobRelayResponse::Err { reason } => {
            assert!(reason.contains("busy"), "expected busy, got {reason}")
        }
        other => panic!("expected busy Err, got {other:?}"),
    }

    // Release the gate so job1 can finish (and the test doesn't leak).
    gate.add_permits(1);
    let resp1 = job1.await.unwrap();
    assert!(
        matches!(resp1, JobRelayResponse::Ok { .. }),
        "job1 should complete"
    );
}

#[tokio::test(start_paused = true)]
async fn v1_and_v2_share_one_remote_concurrency_gate() {
    #[derive(Clone)]
    struct BlockingWorker {
        gate: Arc<tokio::sync::Semaphore>,
        inner: EchoWorker,
    }
    impl phase_protocol::Worker for BlockingWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            &[phase_protocol::JobSpecKind::Inference]
        }

        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            let _permit = self.gate.acquire().await.expect("test gate open");
            self.inner.execute(job).await
        }
    }

    let worker_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let worker: Arc<dyn DynWorker> = Arc::new(BlockingWorker {
        gate: worker_gate.clone(),
        inner: EchoWorker::new(),
    });
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            allow_unauthenticated_jobs: true,
            max_concurrent_remote_jobs: 1,
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let handlers = make_inbound_relay_handlers(worker, registry, policy);
    let client = NodeIdentity::generate();
    let v2_manifest = inference_manifest(&client, &loaded_test_model_cid(), Some(4));
    let v2_job_id = v2_manifest.manifest_hash().expect("manifest hash");
    let v2_payload = serde_json::to_vec(&v2_manifest).expect("encode manifest");
    let v1_payload = serde_json::to_vec(&inference_manifest(
        &client,
        &loaded_test_model_cid(),
        Some(4),
    ))
    .expect("encode manifest");
    let (_controls_tx, controls_rx) = tokio::sync::mpsc::channel(1);
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel(8);
    let stream_handler = handlers.stream.clone();

    let v2_task = tokio::spawn(async move {
        stream_handler(
            PeerId::random(),
            JobRelayStreamOpen {
                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                job_id: v2_job_id,
                payload: v2_payload,
                deadline_unix_ms: unix_time_ms() + 60_000,
                idle_timeout_ms: 30_000,
            },
            controls_rx,
            frames_tx,
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let response = (handlers.batch)(PeerId::random(), v1_payload).await;
    assert!(matches!(
        response,
        JobRelayResponse::Err { ref reason } if reason.contains("busy")
    ));

    let accepted = tokio::time::timeout(Duration::from_secs(16), frames_rx.recv())
        .await
        .expect("v2 unblocks")
        .expect("v2 sends decision");
    assert!(matches!(accepted.kind, JobRelayStreamFrameKind::Accepted));
    assert_eq!(accepted.sequence, 0);
    for sequence in 1..=2 {
        let frame = tokio::time::timeout(Duration::from_secs(16), frames_rx.recv())
            .await
            .expect("keepalive arrives before the idle deadline")
            .expect("v2 emits a loading update");
        assert_eq!(frame.sequence, sequence);
        let JobRelayStreamFrameKind::Event {
            payload,
            terminal: false,
        } = frame.kind
        else {
            panic!("expected nonterminal progress, got {:?}", frame.kind);
        };
        assert!(matches!(
            serde_json::from_slice::<JobEvent>(&payload).unwrap(),
            JobEvent::Progress(progress) if progress.kind == "model_load"
        ));
    }
    worker_gate.add_permits(1);
    let mut receipt_seen = false;
    while let Some(frame) = frames_rx.recv().await {
        receipt_seen |= matches!(frame.kind, JobRelayStreamFrameKind::Receipt { .. });
    }
    assert!(receipt_seen, "cold load should finish with a receipt");
    v2_task.await.expect("v2 handler exits and releases permit");
}

#[tokio::test]
async fn v1_and_v2_share_one_remote_manifest_replay_cache() {
    let worker: Arc<dyn DynWorker> = Arc::new(EchoWorker::new());
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            allow_unauthenticated_jobs: true,
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let handlers = make_inbound_relay_handlers(worker, registry, policy);
    let client = NodeIdentity::generate();
    let manifest = inference_manifest(&client, &loaded_test_model_cid(), Some(4));
    let job_id = manifest.manifest_hash().expect("manifest hash");
    let payload = serde_json::to_vec(&manifest).expect("encode manifest");

    let first = (handlers.batch)(PeerId::random(), payload.clone()).await;
    assert!(matches!(first, JobRelayResponse::Ok { .. }));

    let (_controls_tx, controls_rx) = tokio::sync::mpsc::channel(1);
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel(2);
    (handlers.stream)(
        PeerId::random(),
        JobRelayStreamOpen {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id,
            payload,
            deadline_unix_ms: unix_time_ms() + 5_000,
            idle_timeout_ms: 1_000,
        },
        controls_rx,
        frames_tx,
    )
    .await;

    let rejection = frames_rx.recv().await.expect("replay rejection");
    assert!(matches!(
        rejection.kind,
        JobRelayStreamFrameKind::Rejected { ref reason }
            if reason.contains("replay")
    ));
    assert!(frames_rx.recv().await.is_none());
}

#[tokio::test]
async fn inbound_v1_and_v2_enforce_absolute_whole_job_deadlines() {
    #[derive(Clone)]
    struct NeverEndingWorker;

    impl phase_protocol::Worker for NeverEndingWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            &[phase_protocol::JobSpecKind::Inference]
        }

        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            let hash = job
                .manifest_hash()
                .map_err(|error| WorkerError::BadManifest(error.to_string()))?;
            let (handle, _producer) = JobHandle::new(JobId(hash));
            Ok((handle, Box::pin(futures::stream::pending())))
        }
    }

    let worker: Arc<dyn DynWorker> = Arc::new(NeverEndingWorker);
    let registry = registry_with_model("qwen3-mini").await;
    let policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            allow_unauthenticated_jobs: true,
            max_concurrent_remote_jobs: 2,
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let context = InboundRelayContext::new(worker, registry, policy);

    let v1 = make_inbound_relay_handler_with_context_and_timeout(
        context.clone(),
        Duration::from_millis(25),
    );
    let first_client = NodeIdentity::generate();
    let first = inference_manifest(&first_client, &loaded_test_model_cid(), Some(4));
    let started = TokioInstant::now();
    let response = v1(
        PeerId::random(),
        serde_json::to_vec(&first).expect("encode v1 manifest"),
    )
    .await;
    assert!(matches!(
        response,
        JobRelayResponse::Err { ref reason } if reason.contains("drain deadline")
    ));
    assert!(started.elapsed() < Duration::from_secs(1));

    let v2 = make_inbound_relay_stream_handler_with_context(context);
    let second_client = NodeIdentity::generate();
    let second = inference_manifest(&second_client, &loaded_test_model_cid(), Some(4));
    let job_id = second.manifest_hash().expect("v2 manifest hash");
    let (_controls_tx, controls_rx) = tokio::sync::mpsc::channel(1);
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel(4);
    v2(
        PeerId::random(),
        JobRelayStreamOpen {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id,
            payload: serde_json::to_vec(&second).expect("encode v2 manifest"),
            deadline_unix_ms: unix_time_ms() + 30,
            idle_timeout_ms: 250,
        },
        controls_rx,
        frames_tx,
    )
    .await;
    let accepted = frames_rx.recv().await.expect("v2 accepted frame");
    assert!(matches!(accepted.kind, JobRelayStreamFrameKind::Accepted));
    let failed = frames_rx.recv().await.expect("v2 deadline frame");
    assert!(matches!(
        failed.kind,
        JobRelayStreamFrameKind::Failed { ref reason } if reason.contains("deadline")
    ));
    assert!(frames_rx.recv().await.is_none());
}

fn discovery_for(identity: &NodeIdentity) -> Arc<Discovery> {
    Arc::new(
        Discovery::new(phase_net::DiscoveryConfig {
            identity: Some(identity.clone()),
            ..phase_net::DiscoveryConfig::default()
        })
        .unwrap(),
    )
}

async fn dial_loopback_fixture(client: &Discovery, worker: &Discovery) {
    worker.listen("/ip4/127.0.0.1/tcp/0").await.unwrap();
    let address = timeout(Duration::from_secs(3), async {
        loop {
            if let Some(address) = worker.listen_addrs().await.unwrap().into_iter().next() {
                break address;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker listen address");
    client
        .dial_peer(&format!("{address}/p2p/{}", worker.local_peer_id()))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_two_peer_router_streams_before_completion_and_verifies_receipt() {
    #[derive(Clone)]
    struct GateAfterFirstOutputWorker {
        inner: EchoWorker,
        release: Arc<tokio::sync::Semaphore>,
        waiting_after_first: Arc<std::sync::atomic::AtomicBool>,
    }

    impl phase_protocol::Worker for GateAfterFirstOutputWorker {
        fn supported_kinds(&self) -> &[phase_protocol::JobSpecKind] {
            phase_protocol::Worker::supported_kinds(&self.inner)
        }

        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            let (handle, mut inner_stream) = self.inner.execute(job).await?;
            let release = self.release.clone();
            let waiting_after_first = self.waiting_after_first.clone();
            let gated_stream: JobStream = Box::pin(stream! {
                let mut gated = false;
                while let Some(event) = futures::StreamExt::next(&mut inner_stream).await {
                    let gate_after_event = !gated && matches!(event, JobEvent::Output(_));
                    yield event;
                    if gate_after_event {
                        gated = true;
                        waiting_after_first.store(true, std::sync::atomic::Ordering::Release);
                        let _permit = release.acquire().await.expect("test gate remains open");
                    }
                }
            });
            Ok((handle, gated_stream))
        }
    }

    let client_identity = NodeIdentity::generate();
    let worker_identity = NodeIdentity::generate();
    let client_net = discovery_for(&client_identity);
    let worker_net = discovery_for(&worker_identity);
    let worker_peer = *worker_net.local_peer_id();
    dial_loopback_fixture(&client_net, &worker_net).await;

    let worker_registry = registry_with_model("qwen3-mini").await;
    let client_pubkey = {
        let manifest = inference_manifest(&client_identity, &loaded_test_model_cid(), Some(512));
        manifest.signer_pubkey
    };
    let worker_policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig {
            authorized_submitters: vec![client_pubkey],
            ..PolicyConfig::default()
        },
        PolicyState::default(),
    ));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let waiting_after_first = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker: Arc<dyn DynWorker> = Arc::new(GateAfterFirstOutputWorker {
        inner: EchoWorker {
            token_delay: Duration::ZERO,
            identity: worker_identity.clone(),
        },
        release: release.clone(),
        waiting_after_first: waiting_after_first.clone(),
    });
    worker_net
        .set_job_relay_stream_handler(Some(make_inbound_relay_stream_handler(
            worker,
            worker_registry,
            worker_policy,
        )))
        .unwrap();

    let client_registry = Arc::new(ModelRegistry::new(
        client_identity.clone(),
        Arc::new(MockDht::default()) as Arc<dyn DhtTransport>,
    ));
    let client_policy = Arc::new(PolicyEngine::new_for_tests(
        PolicyConfig::default(),
        PolicyState::default(),
    ));
    let router = Router::new(
        None,
        client_registry,
        client_policy,
        client_identity.clone(),
        client_net,
    )
    .with_remote_access(None, true);
    let manifest = {
        use phase_manifest::ManifestBuilder;
        use phase_protocol::{ChatMessage, ChatRole, InferenceJobSpec, SamplingParams};
        ManifestBuilder::new(JobSpec::Inference(InferenceJobSpec {
            model_cid: loaded_test_model_cid(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "stream".to_string(),
                images: vec![],
            }],
            prompt: None,
            resume_from: None,
            sampling: SamplingParams::default(),
            max_tokens: Some(512),
            stream: true,
        }))
        .expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
        .sign_with(&client_identity)
        .unwrap()
    };

    let (handle, mut stream, verification) = router
        .execute_via_peer(worker_peer, manifest)
        .await
        .unwrap();
    assert_eq!(verification, ReceiptVerification::Pending);
    let first = futures::StreamExt::next(&mut stream)
        .await
        .expect("first live event");
    assert!(matches!(first, JobEvent::Output(_)));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !waiting_after_first.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("serving worker reaches deterministic post-output gate");
    // The serving worker cannot produce a second event or its terminal
    // receipt until this permit is released. Observing the first output
    // while the gate is still closed proves it crossed the real libp2p
    // stream before job completion, without depending on wall-clock
    // delays or how Linux scheduled the transport tasks.
    assert_eq!(release.available_permits(), 0);
    release.add_permits(1);

    let mut output_count = 1_u64;
    let mut saw_final = false;
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        match event {
            JobEvent::Output(_) => output_count += 1,
            JobEvent::Final { result, error } => {
                assert_eq!(result.completion, Completion::Stop);
                assert!(error.is_none());
                saw_final = true;
            }
            _ => {}
        }
    }
    assert_eq!(output_count, 6);
    assert!(saw_final);
    let receipt = handle.finish().await.expect("verified live receipt");
    receipt.verify().unwrap();
    assert_eq!(receipt.worker_pubkey, worker_net.public_key_hex());
}

#[test]
fn sec06_hex_decode_32_roundtrip_and_rejects_bad_input() {
    let id = NodeIdentity::generate();
    let hex = {
        let b = id.verifying_key().to_bytes();
        let mut s = String::new();
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    };
    assert_eq!(hex_decode_32(&hex), Some(id.verifying_key().to_bytes()));
    assert_eq!(hex_decode_32("zz"), None); // wrong length
    assert_eq!(hex_decode_32(&"g".repeat(64)), None); // non-hex nibble
}
