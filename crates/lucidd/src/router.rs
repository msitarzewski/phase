// SPDX-License-Identifier: AGPL-3.0-or-later

//! LUCID M5 — local-or-DHT router.
//!
//! Per-request decision: do we serve this inference job locally, hand it
//! off to a peer over the Phase DHT, or refuse outright? The router glues
//! together the four pieces of LUCID's prior milestones:
//!
//! - [`phase_protocol::Worker`] (M2/M3): the local inference backend.
//! - [`crate::ModelRegistry`] (M6): "who on the DHT has this model loaded?".
//! - [`crate::PolicyEngine`] (M7): operator-controlled gating.
//! - `phase_net::Discovery` (substrate): libp2p transport for both the DHT
//!   and the peer-relay request/response protocol.
//!
//! ## Decision order
//!
//! 1. `local_only && !(local has model loaded)` → `Refused("local-only
//!    requested but model not loaded locally")`. The privacy posture flag
//!    is non-negotiable.
//! 2. Operator policy ([`PolicyEngine::should_serve`]) says pause →
//!    `Refused(PauseReason)`.
//! 3. Local worker has the model loaded → `Local`.
//! 4. Otherwise: DHT lookup; first valid peer → `Peer { peer_id }`.
//! 5. No peers → `Refused("no peers serving model X")`.
//!
//! ## v0.1 limitations (documented, not bugs)
//!
//! - **Peer relay is batch-shaped.** The serving side drains its
//!   `JobStream` end-to-end and returns the whole `Vec<JobEvent>` in one
//!   shot. Token-by-token streaming over the relay is v0.2 — see
//!   `releases/lucid/index.yaml`. Local routes still stream natively.
//! - **No retry across peers.** If the first peer fails, the request
//!   fails. Multi-peer fallback is straightforward to add but lives
//!   outside the M5 critical path.
//! - **No fits-in-VRAM check before local dispatch.** Worker layer does
//!   its own admission control via `WorkerError::Capacity`. The router
//!   surfaces that as a 503 to the client.

use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use phase_identity::NodeIdentity;
use phase_net::{Discovery, JobRelayRequest, JobRelayResponse, PeerId};
use phase_protocol::{
    DynWorker, JobEvent, JobHandle, JobId, JobSpec, JobStream, SignedManifest, WorkerError,
};
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::registry::ModelRegistry;
use crate::policy::{PauseReason, PolicyDecision, PolicyEngine};

/// How long the requesting side will wait for a relay response. CBOR is
/// cheap; the real time is the serving peer's inference. Five minutes
/// covers a long generation on a slow GPU before we give up.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Where this request will be served.
#[derive(Debug, Clone)]
pub enum RouteVia {
    /// Dispatch to the local worker.
    Local,
    /// Relay to a peer over `/phase/job-relay/1.0.0`.
    Peer { peer_id: PeerId },
    /// Refuse — the reason is human-readable for the HTTP layer.
    Refused { reason: String },
}

/// Outcome of a routing decision.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub via: RouteVia,
    pub model_id: String,
}

impl RouteDecision {
    /// Short label suitable for the `X-Lucid-Routed-Via` response header.
    /// Returns `None` on `Refused` — the HTTP layer omits the header in
    /// that case.
    pub fn header_value(&self) -> Option<String> {
        match &self.via {
            RouteVia::Local => Some("local".to_string()),
            RouteVia::Peer { peer_id } => {
                let s = peer_id.to_string();
                let short: String = s.chars().rev().take(8).collect::<String>().chars().rev().collect();
                Some(format!("peer:{short}"))
            }
            RouteVia::Refused { .. } => None,
        }
    }
}

/// Errors `execute` can return.
#[derive(Debug, Error)]
pub enum RouterError {
    #[error("router refused: {reason}")]
    Refused { reason: String },
    #[error("local worker error: {0}")]
    Worker(#[from] WorkerError),
    #[error("peer relay error: {0}")]
    Relay(String),
    #[error("router has no local worker")]
    NoLocalWorker,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Per-request router. Cheap to clone — every field is `Arc`/handle-shaped.
pub struct Router {
    local_worker: Option<Arc<dyn DynWorker>>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
    #[allow(dead_code)] // surfaces in receipts/logs in a future milestone
    identity: NodeIdentity,
    phase_net: Arc<Discovery>,
}

impl Router {
    /// Build a router.
    ///
    /// `local_worker = None` puts the daemon in **consume-only mode** —
    /// every request is routed to a peer (or refused). This is the
    /// `--no-local-worker` CLI path: a node with no GPU still wants to be
    /// a useful client.
    pub fn new(
        local_worker: Option<Arc<dyn DynWorker>>,
        registry: Arc<ModelRegistry>,
        policy: Arc<PolicyEngine>,
        identity: NodeIdentity,
        phase_net: Arc<Discovery>,
    ) -> Self {
        Self {
            local_worker,
            registry,
            policy,
            identity,
            phase_net,
        }
    }

    /// Choose where to serve `model_id`. Pure decision step — no side
    /// effects, no worker dispatch.
    pub async fn route(&self, model_id: &str, local_only: bool) -> RouteDecision {
        let has_local_worker = self.local_worker.is_some();
        let local_models = self.registry.local_models_async().await;
        let local_has_model = local_models.iter().any(|c| c.model_id == model_id);

        // 1. Local-only privacy posture wins over everything else.
        if local_only && !(has_local_worker && local_has_model) {
            return RouteDecision {
                via: RouteVia::Refused {
                    reason: format!(
                        "local-only requested but model '{model_id}' not loaded locally"
                    ),
                },
                model_id: model_id.to_string(),
            };
        }

        // 2. Policy gate. The policy engine returns Allow on the happy
        //    path; otherwise we refuse with the structured reason.
        //
        //    NOTE: `should_serve` was designed for the serving side —
        //    "should I accept work from a peer?" — but the same gating
        //    is correct for self-initiated work too: an operator who
        //    paused their node should not see their own requests start
        //    chewing through battery. M5 deliberately reuses the
        //    decision function; M7 framed it this way on purpose.
        match self.policy.should_serve(model_id, 0) {
            PolicyDecision::Allow => {}
            PolicyDecision::Pause { reason } => {
                return RouteDecision {
                    via: RouteVia::Refused {
                        reason: pause_reason_string(&reason),
                    },
                    model_id: model_id.to_string(),
                };
            }
        }

        // 3. Local has the model — fast path.
        if has_local_worker && local_has_model {
            return RouteDecision {
                via: RouteVia::Local,
                model_id: model_id.to_string(),
            };
        }

        // 4. Look up peers on the DHT.
        let peers = match self.registry.find_peers_by_model_id(model_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "registry lookup failed");
                Vec::new()
            }
        };
        if let Some((peer_id, caps)) = peers.into_iter().next() {
            debug!(
                model = %model_id,
                peer = %peer_id,
                quant = %caps.quantization,
                "routing to peer"
            );
            return RouteDecision {
                via: RouteVia::Peer { peer_id },
                model_id: model_id.to_string(),
            };
        }

        // 5. Nobody can serve.
        RouteDecision {
            via: RouteVia::Refused {
                reason: format!("no peers serving model '{model_id}'"),
            },
            model_id: model_id.to_string(),
        }
    }

    /// Execute `job` according to `decision`. Returns the same
    /// `(JobHandle, JobStream)` shape the underlying `Worker::execute`
    /// would — so the HTTP layer's NDJSON loop doesn't have to care
    /// whether the bytes are coming from a local worker or a peer relay.
    pub async fn execute(
        &self,
        decision: &RouteDecision,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), RouterError> {
        match &decision.via {
            RouteVia::Refused { reason } => Err(RouterError::Refused {
                reason: reason.clone(),
            }),
            RouteVia::Local => {
                let worker = self
                    .local_worker
                    .as_ref()
                    .ok_or(RouterError::NoLocalWorker)?
                    .clone();
                Ok(worker.execute_boxed(job).await?)
            }
            RouteVia::Peer { peer_id } => self.execute_via_peer(*peer_id, job).await,
        }
    }

    /// Build a synthetic `(JobHandle, JobStream)` pair backed by the
    /// peer-relay request/response wire. v0.1 is batch-shaped — the
    /// stream materialises the full event vector before yielding,
    /// because the serving side returns all events in one CBOR response.
    async fn execute_via_peer(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), RouterError> {
        // Compute the JobId up front (mirrors what a local worker would
        // do via manifest_hash) so the caller's NDJSON loop can log it.
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| RouterError::Relay(format!("manifest hash: {e}")))?;
        let job_id = JobId(manifest_hash);

        // Encode the request payload as JSON. We initially tried bincode
        // 1.x but `SignedManifest` has `expires_at: Option<DateTime<Utc>>`
        // with `#[serde(skip_serializing_if = "Option::is_none")]`, which
        // bincode 1.x can't roundtrip. JSON costs us a few extra bytes on
        // the wire (~1-2KB per relay request) in exchange for compatibility
        // with every serde-friendly type — fine for v0.1.
        let payload = serde_json::to_vec(&job)
            .map_err(|e| RouterError::Relay(format!("encode SignedManifest: {e}")))?;
        let request = JobRelayRequest { payload };

        info!(
            peer = %peer_id,
            job = %job_id,
            payload_bytes = request.payload.len(),
            "relay: sending job to peer"
        );

        // Fire-and-await with a wall-clock cap.
        let response = timeout(
            RELAY_TIMEOUT,
            self.phase_net.send_job_relay(peer_id, request),
        )
        .await
        .map_err(|_| RouterError::Relay(format!("peer {peer_id} relay timed out")))?
        .map_err(|e| RouterError::Relay(format!("send_job_relay: {e}")))?;

        let events_bytes = match response {
            JobRelayResponse::Ok { events } => events,
            JobRelayResponse::Err { reason } => {
                return Err(RouterError::Relay(format!("peer refused: {reason}")));
            }
        };

        let events: Vec<JobEvent> = serde_json::from_slice(&events_bytes)
            .map_err(|e| RouterError::Relay(format!("decode peer events: {e}")))?;
        debug!(
            peer = %peer_id,
            job = %job_id,
            count = events.len(),
            "relay: peer returned event batch"
        );

        // Synthesize the handle/stream pair. The relay path doesn't get
        // a real receipt back (the peer keeps it locally — the
        // `output_commitment` does ride along inside `JobEvent::Final`),
        // so we deliver_receipt-less here. `handle.finish()` will return
        // `WorkerError::Dropped` — the Ollama layer already tolerates
        // that on the streaming path.
        let (handle, _producer) = JobHandle::new(job_id);
        let stream: JobStream = Box::pin(stream! {
            for ev in events {
                yield ev;
            }
        });
        Ok((handle, stream))
    }
}

/// Stringify a [`PauseReason`] for the HTTP body. Stable, human-readable —
/// the operator pastes this into a bug report.
fn pause_reason_string(reason: &PauseReason) -> String {
    match reason {
        PauseReason::Manual => "operator paused (manual)".to_string(),
        PauseReason::OnBattery => "on battery".to_string(),
        PauseReason::ThermalLimit {
            current_c,
            threshold_c,
        } => {
            format!("thermal limit hit ({current_c} °C >= {threshold_c} °C)")
        }
        PauseReason::OutsideTimeWindow => "outside serving time window".to_string(),
        PauseReason::ConcurrencyLimit => "concurrency limit reached".to_string(),
        PauseReason::ModelNotInAllowlist { model_id } => {
            format!("model '{model_id}' not in operator allowlist")
        }
        PauseReason::SystemPaused => "system paused".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Inbound relay handler (serving side)
// ---------------------------------------------------------------------------

/// Build the `JobRelayHandler` that `phase_net::Discovery` will invoke
/// when a peer asks us to run a job on its behalf.
///
/// The serving side:
/// 1. Decodes the bincode `SignedManifest<JobSpec>`.
/// 2. Re-runs the policy gate. (The router check at the requesting side
///    only governed the requester; the serving side is sovereign.)
/// 3. If the model isn't locally loaded → refuse.
/// 4. Dispatches via the local worker, drains the stream into a Vec,
///    and ships it back.
///
/// Errors are surfaced as `JobRelayResponse::Err` rather than dropped on
/// the floor — the requesting side maps that to an HTTP 503.
pub fn make_inbound_relay_handler(
    worker: Arc<dyn DynWorker>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
) -> phase_net::JobRelayHandler {
    Arc::new(move |bytes: Vec<u8>| {
        let worker = worker.clone();
        let registry = registry.clone();
        let policy = policy.clone();
        Box::pin(async move {
            // 1. Decode. JSON (matches the request-side encoding); see the
            //    note on the requesting side about why not bincode.
            let job: SignedManifest<JobSpec> = match serde_json::from_slice(&bytes) {
                Ok(j) => j,
                Err(e) => {
                    return JobRelayResponse::Err {
                        reason: format!("decode SignedManifest: {e}"),
                    };
                }
            };

            // 2. Pull out the model id (so we can policy-check) and ensure
            //    the spec is an inference job.
            let model_id = match &job.payload {
                JobSpec::Inference(spec) => spec.model_cid.clone(),
                _ => {
                    return JobRelayResponse::Err {
                        reason: "non-inference job not supported over relay".to_string(),
                    };
                }
            };

            // 3. Policy gate. (Operator sovereignty.)
            match policy.should_serve(&model_id, 0) {
                PolicyDecision::Allow => {}
                PolicyDecision::Pause { reason } => {
                    return JobRelayResponse::Err {
                        reason: pause_reason_string(&reason),
                    };
                }
            }

            // 4. Check we actually have the model loaded — peers
            //    shouldn't be sending us work for something we don't
            //    advertise, but defending against that is cheap.
            let locals = registry.local_models_async().await;
            if !locals.iter().any(|c| c.model_id == model_id) {
                return JobRelayResponse::Err {
                    reason: format!("model '{model_id}' not loaded on this peer"),
                };
            }

            // 5. Dispatch + drain.
            let (handle, mut stream) = match worker.execute_boxed(job).await {
                Ok(t) => t,
                Err(e) => {
                    return JobRelayResponse::Err {
                        reason: format!("local worker dispatch failed: {e}"),
                    };
                }
            };
            let mut events: Vec<JobEvent> = Vec::new();
            while let Some(ev) = futures::StreamExt::next(&mut stream).await {
                events.push(ev);
            }
            // Best-effort: pull the signed receipt and drop it. The
            // commitment is already in `JobEvent::Final.result.output_commitment`.
            // A future polish step (v0.2) ships the full SignedReceipt in
            // the relay response.
            let _ = handle.finish().await;

            let encoded = match serde_json::to_vec(&events) {
                Ok(b) => b,
                Err(e) => {
                    return JobRelayResponse::Err {
                        reason: format!("encode events: {e}"),
                    };
                }
            };
            JobRelayResponse::Ok { events: encoded }
        }) as _
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::echo::EchoWorker;
    use crate::policy::{PolicyConfig, PolicyState};
    use crate::registry::{DhtTransport, ModelCapabilities, ModelCid, ModelRegistry};
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
            Ok(self.store.lock().unwrap().get(&key).cloned().unwrap_or_default())
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
        let router = Router::new(
            Some(worker),
            registry.clone(),
            policy,
            identity,
            phase_net,
        );
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
    async fn route_peer_when_dht_has_advertisement_and_local_does_not() {
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
        let registry = Arc::new(ModelRegistry::new(
            identity.clone(),
            transport.clone() as _,
        ));
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
        let router = Router::new(
            None,
            registry,
            policy,
            identity,
            build_test_discovery(),
        );

        let decision = router.route("qwen3-big", false).await;
        match &decision.via {
            RouteVia::Peer { peer_id } => {
                // Re-derive the expected peer-id from the foreign
                // identity to assert we picked the right one.
                use phase_net::libp2p_identity::{ed25519, PublicKey};
                let ed = ed25519::PublicKey::try_from_bytes(
                    &foreign.verifying_key().to_bytes(),
                )
                .unwrap();
                let pk: PublicKey = ed.into();
                let expected = PeerId::from(pk);
                assert_eq!(*peer_id, expected);
                // Header value should be "peer:<short>".
                let hv = decision.header_value().unwrap();
                assert!(hv.starts_with("peer:"), "header: {hv}");
            }
            other => panic!("expected Peer, got {:?}", other),
        }
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
        let router = Router::new(
            None,
            registry,
            policy,
            identity,
            build_test_discovery(),
        );
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
        use phase_protocol::{
            ChatMessage, ChatRole, InferenceJobSpec, JobSpec, SamplingParams,
        };

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
            max_tokens: None,
            stream: true,
        });
        let manifest = ManifestBuilder::new(spec).sign_with(&client).unwrap();
        let (_handle, mut stream) = router.execute(&decision, manifest).await.unwrap();
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

    #[test]
    fn header_value_local_and_peer_shapes() {
        let d = RouteDecision {
            via: RouteVia::Local,
            model_id: "x".into(),
        };
        assert_eq!(d.header_value().as_deref(), Some("local"));

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
        };
        let hv = d.header_value().unwrap();
        assert!(hv.starts_with("peer:"), "got {hv}");
        // 5 for "peer:" + 8 short id = 13.
        assert_eq!(hv.len(), 13, "got {hv}");
    }
}
