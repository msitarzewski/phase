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
//! ## Compatibility and admission boundaries
//!
//! - **Live-first relay.** v2 forwards ordered worker events as they arrive.
//!   The bounded v1 batch path is used only when protocol negotiation proves
//!   the peer has not enabled v2 and no output has been exposed.
//! - **Multi-peer failover.** When the DHT returns more than one peer for
//!   a model, the first (registry-ranked) peer is the primary and the rest
//!   ride along on the [`RouteDecision`] as `fallback_peers`; `execute`
//!   walks them in order if the primary fails before output.
//! - **No fits-in-VRAM check before local dispatch.** Worker layer does
//!   its own admission control via `WorkerError::Capacity`. The router
//!   surfaces that as a 503 to the client.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use async_stream::stream;
use phase_identity::NodeIdentity;
use phase_net::{
    Discovery, JobRelayRequest, JobRelayResponse, JobRelayStreamControlKind, JobRelayStreamFrame,
    JobRelayStreamFrameKind, JobRelayStreamHandler, JobRelayStreamOpen, PeerId,
    JOB_RELAY_STREAM_SCHEMA_VERSION,
};
use phase_protocol::{
    CommitmentAccumulator, Completion, DynWorker, JobEvent, JobHandle, JobId, JobMetrics,
    JobResult, JobSpec, JobStream, SignedManifest, SignedReceipt, WorkerError,
};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{timeout, timeout_at, Instant as TokioInstant};
use tracing::{debug, error, info, warn};

/// SEC-06: hard ceiling on the total prompt/message character length a relay
/// peer may submit, enforced in the authz/policy gate *before* dispatch. A
/// hostile peer can otherwise ship a multi-megabyte prompt to exhaust GPU
/// context memory even with `max_tokens` clamped. 256 KiB of text is far
/// beyond any legitimate chat turn while staying under the 256 KiB relay
/// request frame cap (SEC-06, discovery.rs).
const MAX_PROMPT_CHARS: usize = 256 * 1024;
const MAX_EMBEDDING_INPUTS: usize = 128;
const MAX_EMBEDDING_ENTRY_CHARS: usize = 64 * 1024;
const MAX_REMOTE_REPLAY_ENTRIES: usize = 65_536;
const INBOUND_RELAY_JOB_TIMEOUT: Duration = Duration::from_secs(5 * 60);

use crate::policy::{PauseReason, PolicyDecision, PolicyEngine};
use crate::registry::{normalize_model_alias, ModelCapabilities, ModelCid, ModelRegistry};
use crate::reputation::{
    compare_assessments, AssessmentClass, EvidenceContext, EvidenceOutcome, EvidenceRuntime,
};

/// How long the requesting side will wait for a relay response. CBOR is
/// cheap; the real time is the serving peer's inference. Five minutes
/// covers a long generation on a slow GPU before we give up.
pub const RELAY_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EVIDENCE_PROTOCOL_LIVE: &str = "/phase/job-relay-stream/2.0.0";
const EVIDENCE_PROTOCOL_BATCH: &str = "/phase/job-relay/1.0.0";
const EVIDENCE_PROTOCOL_REDUNDANT: &str = "/phase/redundant-check/1.0.0";
const EVIDENCE_SOFTWARE_VERSION: &str = concat!("lucidd/", env!("CARGO_PKG_VERSION"));
const REDUNDANT_SAMPLE_DOMAIN: &[u8] = b"lucid-redundant-sample:v1\0";

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
    /// Additional peers (ranked, primary first excluded) to fail over to
    /// if the primary peer relay fails. Empty for Local / Refused / single-peer.
    pub fallback_peers: Vec<PeerId>,
    /// Human-readable account of policy, evidence ordering, and cold-start
    /// opportunity. This is explanatory only, never a correctness claim.
    pub explanation: String,
}

/// Operator-owned limits for automatic and explicit redundant verification.
/// Disabled by default; at most one two-peer check may run concurrently per
/// router, and sampling is keyed by the local identity to resist requester
/// grinding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedundantVerificationConfig {
    pub enabled: bool,
    pub sample_cap_permille: u16,
}

/// Caller assertion that a job has explicitly been judged deterministic for
/// commitment comparison. This is an operator/integration API, not a field
/// accepted from an untrusted inference request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeterministicVerificationEligibility {
    _private: (),
}

impl DeterministicVerificationEligibility {
    pub const fn operator_approved() -> Self {
        Self { _private: () }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedundantCheckResult {
    SkippedDisabled,
    SkippedSampling,
    SkippedBusy,
    Agreement {
        commitment: [u8; 32],
    },
    Disagreement {
        primary_commitment: [u8; 32],
        duplicate_commitment: [u8; 32],
    },
    Incomparable,
}

/// SEC-05: receipt verification status for a dispatched job, surfaced to the
/// HTTP layer so it can set `X-Lucid-Receipt-Verified`.
///
/// The local path is `Local` (the worker is us — no peer receipt to bind).
/// The peer path is `Verified` when the worker's `SignedReceipt` passed every
/// check (signature, job_id bind, worker-pubkey→PeerId bind, commitment
/// replay), `Failed` when a check did not hold (the terminal API state is
/// failed rather than claiming verified success), or `Unverifiable` when a
/// permitted legacy v1 peer shipped no receipt at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptVerification {
    /// Served locally — no peer receipt to verify.
    Local,
    /// Peer receipt verified and bound to the dispatched job + peer.
    Verified,
    /// Peer receipt present but a check failed (signature/bind/commitment).
    Failed,
    /// Peer returned no receipt (pre-SEC-05 serving node).
    Unverifiable,
    /// Live peer stream is still in progress; verification happens only when
    /// the terminal receipt arrives and is bound to the delivered chunks.
    Pending,
}

impl ReceiptVerification {
    /// Value for the `X-Lucid-Receipt-Verified` header, or `None` to omit
    /// (the local path doesn't carry a peer-receipt assertion).
    pub fn header_value(&self) -> Option<&'static str> {
        match self {
            ReceiptVerification::Local => None,
            ReceiptVerification::Verified => Some("true"),
            ReceiptVerification::Failed => Some("false"),
            ReceiptVerification::Unverifiable => Some("unverifiable"),
            ReceiptVerification::Pending => Some("pending"),
        }
    }
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
                let short: String = s
                    .chars()
                    .rev()
                    .take(8)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
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
#[derive(Clone)]
pub struct Router {
    local_worker: Option<Arc<dyn DynWorker>>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
    identity: NodeIdentity,
    phase_net: Arc<Discovery>,
    evidence: Option<Arc<EvidenceRuntime>>,
    redundant_config: RedundantVerificationConfig,
    redundant_gate: Arc<Semaphore>,
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
            evidence: None,
            redundant_config: RedundantVerificationConfig::default(),
            redundant_gate: Arc::new(Semaphore::new(1)),
        }
    }

    /// Attach local evidence-based ordering without changing existing callers.
    pub fn with_evidence_runtime(mut self, evidence: Arc<EvidenceRuntime>) -> Self {
        let expected_observer = node_identity_peer_id(&self.identity);
        if expected_observer != evidence.observer_peer_id() {
            error!(
                expected = %expected_observer,
                observed = %evidence.observer_peer_id(),
                "refusing evidence runtime bound to a different local identity"
            );
            return self;
        }
        self.evidence = Some(evidence);
        self
    }

    /// Configure redundant checking. Values above 1000‰ are clamped; checks
    /// remain limited by a non-queueing one-per-router gate.
    pub fn with_redundant_verification(mut self, mut config: RedundantVerificationConfig) -> Self {
        config.sample_cap_permille = config.sample_cap_permille.min(1_000);
        self.redundant_config = config;
        self
    }

    /// Choose where to serve `model_id`. Pure decision step — no side
    /// effects, no worker dispatch.
    pub async fn route(&self, model_id: &str, local_only: bool) -> RouteDecision {
        let has_local_worker = self.local_worker.is_some();
        let local_models = self.registry.local_models_async().await;
        let requested_cid = ModelCid::from_hex(model_id).ok();
        let normalized_alias = normalize_model_alias(model_id).ok();
        let local_model = local_models.iter().find(|caps| {
            requested_cid == Some(caps.model_cid)
                || normalized_alias.as_deref() == Some(caps.model_id.as_str())
        });
        let local_has_model = local_model.is_some();
        let policy_model_id = local_model
            .map(|caps| caps.model_id.as_str())
            .or(normalized_alias.as_deref())
            .unwrap_or(model_id);

        // 1. Local-only privacy posture wins over everything else.
        if local_only && !(has_local_worker && local_has_model) {
            return RouteDecision {
                via: RouteVia::Refused {
                    reason: format!(
                        "local-only requested but model '{model_id}' not loaded locally"
                    ),
                },
                model_id: model_id.to_string(),
                fallback_peers: Vec::new(),
                explanation: "local-only privacy policy refused remote routing".to_string(),
            };
        }

        // 2. Policy gate. The policy engine returns Allow on the happy
        //    path; otherwise we refuse with the structured reason.
        //
        //    NOTE: this governs the operator's OWN self-initiated request.
        //    `should_serve_self` honors ONLY `manual_pause` — an explicit
        //    "this node is off" the operator set themselves. Every other gate
        //    in the full `should_serve` (battery, thermal, time window,
        //    concurrency, model allowlist) is donation-protection: it exists
        //    to shield the node from *other* peers' work, so it must not block
        //    the operator using their own GPU (a laptop on battery should
        //    still answer its owner's curl). The inbound relay path keeps the
        //    full `should_serve` gate — sovereignty there is donation-
        //    protection by definition.
        match self.policy.should_serve_self(policy_model_id) {
            PolicyDecision::Allow => {}
            PolicyDecision::Pause { reason } => {
                return RouteDecision {
                    via: RouteVia::Refused {
                        reason: pause_reason_string(&reason),
                    },
                    model_id: model_id.to_string(),
                    fallback_peers: Vec::new(),
                    explanation: "operator policy refused this request before candidate selection"
                        .to_string(),
                };
            }
        }

        // 3. Local has the model — fast path.
        if has_local_worker && local_has_model {
            return RouteDecision {
                via: RouteVia::Local,
                model_id: model_id.to_string(),
                fallback_peers: Vec::new(),
                explanation: "local worker is loaded; remote reputation was not consulted"
                    .to_string(),
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
        let (peers, ranking_explanation) = self.rank_remote_candidates(peers).await;
        let mut iter = peers.into_iter();
        if let Some((peer_id, caps)) = iter.next() {
            debug!(
                model = %model_id,
                peer = %peer_id,
                quant = %caps.quantization,
                "routing to peer"
            );
            // Keep the remaining peers in the registry's ranking order as the
            // failover chain — `execute()` walks them if the primary relay
            // fails.
            let fallback_peers: Vec<PeerId> = iter.map(|(p, _)| p).collect();
            return RouteDecision {
                via: RouteVia::Peer { peer_id },
                model_id: model_id.to_string(),
                fallback_peers,
                explanation: ranking_explanation,
            };
        }

        // 5. Nobody can serve.
        RouteDecision {
            via: RouteVia::Refused {
                reason: format!("no peers serving model '{model_id}'"),
            },
            model_id: model_id.to_string(),
            fallback_peers: Vec::new(),
            explanation: ranking_explanation,
        }
    }

    async fn rank_remote_candidates(
        &self,
        peers: Vec<(PeerId, ModelCapabilities)>,
    ) -> (Vec<(PeerId, ModelCapabilities)>, String) {
        let Some(runtime) = &self.evidence else {
            return (
                peers,
                "evidence ordering disabled; preserved deterministic registry order".to_string(),
            );
        };
        let peer_ids = peers.iter().map(|(peer, _)| *peer).collect::<Vec<_>>();
        let assessments = match runtime.assess_peers(&peer_ids, unix_time_ms()).await {
            Ok(assessments) => assessments,
            Err(error) => {
                error!(%error, "routing evidence assessment failed; preserving registry order");
                return (
                    peers,
                    format!(
                        "evidence assessment failed ({error}); preserved registry order without claiming reputation"
                    ),
                );
            }
        };

        let mut allowed = peers
            .into_iter()
            .zip(assessments)
            .filter(|(_, assessment)| assessment.class != AssessmentClass::Blocked)
            .collect::<Vec<_>>();
        let blocked_count = peer_ids.len().saturating_sub(allowed.len());
        allowed.sort_by(|left, right| compare_assessments(&left.1, &right.1));

        // Preserve one bounded cold-start opportunity near the front without
        // letting unlimited fresh identities displace an observed or pinned
        // primary. Remaining cold peers stay deterministically ordered.
        if let Some(cold_index) = allowed
            .iter()
            .position(|(_, assessment)| assessment.class == AssessmentClass::ColdStart)
        {
            let has_observed = allowed
                .iter()
                .any(|(_, assessment)| assessment.class != AssessmentClass::ColdStart);
            if has_observed && cold_index > 1 {
                let cold = allowed.remove(cold_index);
                allowed.insert(1, cold);
            }
        }

        let selected_explanation = allowed
            .first()
            .map(|(_, assessment)| assessment.explanation.as_str())
            .unwrap_or("no candidate remained after local operator blocks");
        let explanation = format!(
            "local evidence ordered {} candidates; filtered {blocked_count} operator-blocked peer(s); one cold-start fallback is promoted when available; selected assessment: {selected_explanation}",
            allowed.len()
        );
        (
            allowed
                .into_iter()
                .map(|(candidate, _)| candidate)
                .collect(),
            explanation,
        )
    }

    /// Execute `job` according to `decision`. Returns the same
    /// `(JobHandle, JobStream)` shape the underlying `Worker::execute`
    /// would — so the HTTP layer's NDJSON loop doesn't have to care
    /// whether the bytes are coming from a local worker or a peer relay.
    pub async fn execute(
        &self,
        decision: &RouteDecision,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
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
                let (handle, stream) = worker.execute_boxed(job).await?;
                Ok((handle, stream, ReceiptVerification::Local))
            }
            // Multi-peer failover: try the primary peer first; if its relay
            // fails, walk `decision.fallback_peers` in ranking order. The
            // ranking comes from the registry (route step 4). `SignedManifest`
            // is Clone, so each attempt gets its own copy of the job.
            RouteVia::Peer { peer_id } => {
                match self.execute_via_peer(*peer_id, job.clone()).await {
                    Ok(execution) => Ok(self.attach_redundant_probe(
                        *peer_id,
                        decision.fallback_peers.first().copied(),
                        job,
                        execution,
                    )),
                    Err(first_err) => {
                        for (index, fb) in decision.fallback_peers.iter().enumerate() {
                            warn!(failed_peer = %peer_id, next = %fb, "relay failed; failing over to next peer");
                            match self.execute_via_peer(*fb, job.clone()).await {
                                Ok(execution) => {
                                    let duplicate = decision.fallback_peers[index + 1..]
                                        .iter()
                                        .copied()
                                        .find(|candidate| candidate != fb);
                                    return Ok(
                                        self.attach_redundant_probe(*fb, duplicate, job, execution)
                                    );
                                }
                                Err(e) => {
                                    warn!(peer = %fb, error = %e, "fallback peer also failed")
                                }
                            }
                        }
                        Err(first_err)
                    }
                }
            }
        }
    }

    fn attach_redundant_probe(
        &self,
        primary_peer: PeerId,
        duplicate_peer: Option<PeerId>,
        job: SignedManifest<JobSpec>,
        execution: (JobHandle, JobStream, ReceiptVerification),
    ) -> (JobHandle, JobStream, ReceiptVerification) {
        let Some(duplicate_peer) = duplicate_peer else {
            return execution;
        };
        let Some(permit) = self.try_begin_redundant_probe(&job, primary_peer, duplicate_peer)
        else {
            return execution;
        };

        let (handle, mut primary_stream, verification) = execution;
        let (primary_tx, primary_rx) = oneshot::channel();
        let observed_primary: JobStream = Box::pin(stream! {
            let mut primary_tx = Some(primary_tx);
            while let Some(event) = futures::StreamExt::next(&mut primary_stream).await {
                if matches!(event, JobEvent::Final { .. }) {
                    let commitment = match &event {
                        JobEvent::Final { result, error }
                            if error.is_none()
                                && matches!(result.completion, Completion::Stop | Completion::Length) =>
                        {
                            Some(result.output_commitment)
                        }
                        _ => None,
                    };
                    if let Some(tx) = primary_tx.take() {
                        let _ = tx.send(commitment);
                    }
                }
                yield event;
            }
            if let Some(tx) = primary_tx.take() {
                let _ = tx.send(None);
            }
        });

        let router = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if !router
                .redundant_peers_are_backend_equivalent(primary_peer, duplicate_peer, &job)
                .await
            {
                router
                    .record_redundant_result(
                        primary_peer,
                        duplicate_peer,
                        &job,
                        &RedundantCheckResult::Incomparable,
                    )
                    .await;
                return;
            }
            let duplicate_commitment =
                match router.execute_via_peer(duplicate_peer, job.clone()).await {
                    Ok((
                        _handle,
                        stream,
                        ReceiptVerification::Verified | ReceiptVerification::Pending,
                    )) => verified_terminal_commitment(stream).await,
                    Ok(_) | Err(_) => None,
                };
            let primary_commitment = primary_rx.await.unwrap_or(None);
            let result = compare_redundant_commitments(primary_commitment, duplicate_commitment);
            router
                .record_redundant_result(primary_peer, duplicate_peer, &job, &result)
                .await;
        });

        (handle, observed_primary, verification)
    }

    /// Compare only peers whose current signed advertisements attest the same
    /// backend and quantization for the exact immutable model CID. The
    /// registry has already verified each advertisement's signature and
    /// signer→PeerId binding. Missing, expired, or semantically different
    /// capabilities are incomparable and must never generate negative
    /// reputation evidence from output disagreement.
    async fn redundant_peers_are_backend_equivalent(
        &self,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
        job: &SignedManifest<JobSpec>,
    ) -> bool {
        let JobSpec::Inference(spec) = &job.payload else {
            return false;
        };
        let Ok(model_cid) = ModelCid::from_hex(&spec.model_cid) else {
            return false;
        };
        let Ok(peers) = self.registry.find_peers_for_model(&model_cid).await else {
            return false;
        };
        let primary = peers
            .iter()
            .find_map(|(peer, caps)| (*peer == primary_peer).then_some(caps));
        let duplicate = peers
            .iter()
            .find_map(|(peer, caps)| (*peer == duplicate_peer).then_some(caps));
        matches!(
            (primary, duplicate),
            (Some(primary), Some(duplicate))
                if capabilities_are_redundancy_equivalent(primary, duplicate)
        )
    }

    fn try_begin_redundant_probe(
        &self,
        job: &SignedManifest<JobSpec>,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
    ) -> Option<OwnedSemaphorePermit> {
        if !self.redundant_config.enabled
            || self.evidence.is_none()
            || primary_peer == duplicate_peer
            || validate_redundant_job_eligibility(&job.payload).is_err()
        {
            return None;
        }
        let manifest_hash = job.manifest_hash().ok()?;
        if !redundant_sample_selected(
            &self.identity,
            manifest_hash,
            self.redundant_config.sample_cap_permille,
        ) {
            return None;
        }
        Arc::clone(&self.redundant_gate).try_acquire_owned().ok()
    }

    /// Prefer the real v2 bidirectional stream. A v1 batch fallback is
    /// permitted only when protocol negotiation/default-handler refusal proves
    /// the peer has not enabled v2, and only before any output was delivered.
    async fn execute_via_peer(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
        match self.execute_via_peer_live(peer_id, job.clone()).await {
            Ok(stream) => Ok(stream),
            Err(RouterError::Relay(reason))
                if reason.contains("remote does not support")
                    || reason.contains("no live relay handler installed") =>
            {
                warn!(peer = %peer_id, %reason, "live relay unavailable; using explicit v1 batch compatibility");
                self.record_remote_outcome(
                    peer_id,
                    &job,
                    EVIDENCE_PROTOCOL_LIVE,
                    EvidenceOutcome::PreOutputDiscoveryFailure,
                    None,
                )
                .await;
                let result = self.execute_via_peer_batch(peer_id, job.clone()).await;
                if let Err(error) = &result {
                    self.record_remote_outcome(
                        peer_id,
                        &job,
                        EVIDENCE_PROTOCOL_BATCH,
                        classify_relay_failure(error, false),
                        None,
                    )
                    .await;
                }
                result
            }
            Err(error) => {
                self.record_remote_outcome(
                    peer_id,
                    &job,
                    EVIDENCE_PROTOCOL_LIVE,
                    classify_relay_failure(&error, false),
                    None,
                )
                .await;
                Err(error)
            }
        }
    }

    async fn record_remote_outcome(
        &self,
        peer_id: PeerId,
        job: &SignedManifest<JobSpec>,
        protocol_version: &'static str,
        outcome: EvidenceOutcome,
        output_commitment: Option<[u8; 32]>,
    ) {
        let Some(runtime) = &self.evidence else {
            return;
        };
        let context = match evidence_context(
            runtime.observer_peer_id(),
            peer_id,
            job,
            protocol_version,
        ) {
            Ok(context) => context,
            Err(error) => {
                error!(peer = %peer_id, %error, "could not construct remote execution evidence");
                return;
            }
        };
        if let Err(error) = runtime.record(context, outcome, output_commitment).await {
            error!(peer = %peer_id, ?outcome, %error, "failed to persist remote execution evidence");
        }
    }

    async fn execute_via_peer_live(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| RouterError::Relay(format!("manifest hash: {e}")))?;
        let job_id = JobId(manifest_hash);
        let evidence_runtime = self.evidence.clone();
        let evidence_context = evidence_runtime.as_ref().and_then(|runtime| {
            match evidence_context(
                runtime.observer_peer_id(),
                peer_id,
                &job,
                EVIDENCE_PROTOCOL_LIVE,
            ) {
                Ok(context) => Some(context),
                Err(error) => {
                    error!(peer = %peer_id, %error, "could not construct live relay evidence context");
                    None
                }
            }
        });
        let payload = serde_json::to_vec(&job)
            .map_err(|e| RouterError::Relay(format!("encode SignedManifest: {e}")))?;
        let deadline_unix_ms = unix_time_ms().saturating_add(RELAY_TIMEOUT.as_millis() as u64);
        let mut remote = self
            .phase_net
            .open_job_relay_stream(
                peer_id,
                JobRelayStreamOpen {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id: manifest_hash,
                    payload,
                    deadline_unix_ms,
                    idle_timeout_ms: 30_000,
                },
            )
            .await
            .map_err(|e| RouterError::Relay(format!("open live relay: {e}")))?;

        // Wait for the explicit decision before returning a JobStream. This
        // is the only point at which legacy fallback is safe: no worker event
        // has reached the HTTP caller yet.
        match timeout(Duration::from_secs(30), remote.next_frame()).await {
            Ok(Some(Ok(JobRelayStreamFrame {
                kind: JobRelayStreamFrameKind::Accepted,
                ..
            }))) => {}
            Ok(Some(Ok(JobRelayStreamFrame {
                kind: JobRelayStreamFrameKind::Rejected { reason },
                ..
            }))) => return Err(RouterError::Relay(format!("peer refused: {reason}"))),
            Ok(Some(Ok(frame))) => {
                return Err(RouterError::Relay(format!(
                    "live relay returned {:?} before acceptance",
                    frame.kind
                )));
            }
            Ok(Some(Err(error))) => {
                return Err(RouterError::Relay(format!("live relay decision: {error}")));
            }
            Ok(None) => {
                return Err(RouterError::Relay(
                    "live relay closed before decision".into(),
                ))
            }
            Err(_) => return Err(RouterError::Relay("live relay decision timed out".into())),
        }

        let (handle, mut producer) = JobHandle::new(job_id);
        let stream: JobStream = Box::pin(stream! {
            let mut commitment = CommitmentAccumulator::new();
            let mut expected_output_sequence = 0_u64;
            let mut terminal: Option<(JobResult, Option<String>)> = None;
            let mut progress_frames = 0_u32;

            while let Some(frame_result) = remote.next_frame().await {
                let frame = match frame_result {
                    Ok(frame) => frame,
                    Err(error) => {
                        let (output_commitment, output_chunk_count) = commitment.finalize();
                        persist_runtime_outcome(
                            &evidence_runtime,
                            &evidence_context,
                            if output_chunk_count == 0 {
                                EvidenceOutcome::PreOutputTransportFailure
                            } else {
                                EvidenceOutcome::MidStreamTransportLoss
                            },
                            observed_commitment(output_commitment, output_chunk_count),
                        ).await;
                        yield failed_terminal_event(
                            manifest_hash,
                            output_commitment,
                            output_chunk_count,
                            format!("live relay transport verification failed: {error}"),
                        );
                        return;
                    }
                };

                match frame.kind {
                    JobRelayStreamFrameKind::Event { payload, terminal: declared_terminal } => {
                        let event: JobEvent = match serde_json::from_slice(&payload) {
                            Ok(event) => event,
                            Err(error) => {
                                let _ = remote.cancel("malformed worker event").await;
                                let (output_commitment, output_chunk_count) = commitment.finalize();
                                persist_runtime_outcome(
                                    &evidence_runtime,
                                    &evidence_context,
                                    EvidenceOutcome::SequenceMismatch,
                                    observed_commitment(output_commitment, output_chunk_count),
                                ).await;
                                yield failed_terminal_event(
                                    manifest_hash,
                                    output_commitment,
                                    output_chunk_count,
                                    format!("decode live worker event: {error}"),
                                );
                                return;
                            }
                        };
                        match event {
                            JobEvent::Output(chunk) if !declared_terminal => {
                                if chunk.seq != expected_output_sequence {
                                    let _ = remote.cancel("invalid output sequence").await;
                                    let (output_commitment, output_chunk_count) = commitment.finalize();
                                    persist_runtime_outcome(
                                        &evidence_runtime,
                                        &evidence_context,
                                        EvidenceOutcome::SequenceMismatch,
                                        observed_commitment(output_commitment, output_chunk_count),
                                    ).await;
                                    yield failed_terminal_event(
                                        manifest_hash,
                                        output_commitment,
                                        output_chunk_count,
                                        format!(
                                            "expected output sequence {expected_output_sequence}, got {}",
                                            chunk.seq
                                        ),
                                    );
                                    return;
                                }
                                expected_output_sequence = expected_output_sequence.saturating_add(1);
                                commitment.update(&chunk);
                                yield JobEvent::Output(chunk);
                            }
                            JobEvent::Progress(progress) if !declared_terminal => {
                                progress_frames = progress_frames.saturating_add(1);
                                if progress_frames > 1024
                                    || progress.message.as_ref().is_some_and(|m| m.len() > 1024)
                                {
                                    let _ = remote.cancel("progress frame limit exceeded").await;
                                    let (output_commitment, output_chunk_count) = commitment.finalize();
                                    persist_runtime_outcome(
                                        &evidence_runtime,
                                        &evidence_context,
                                        EvidenceOutcome::SequenceMismatch,
                                        observed_commitment(output_commitment, output_chunk_count),
                                    ).await;
                                    yield failed_terminal_event(
                                        manifest_hash,
                                        output_commitment,
                                        output_chunk_count,
                                        "progress frame limit exceeded".to_string(),
                                    );
                                    return;
                                }
                                yield JobEvent::Progress(progress);
                            }
                            JobEvent::Final { result, error } if declared_terminal => {
                                if terminal.is_some() {
                                    let _ = remote.cancel("duplicate terminal event").await;
                                    let (output_commitment, output_chunk_count) = commitment.finalize();
                                    persist_runtime_outcome(
                                        &evidence_runtime,
                                        &evidence_context,
                                        EvidenceOutcome::SequenceMismatch,
                                        observed_commitment(output_commitment, output_chunk_count),
                                    ).await;
                                    yield failed_terminal_event(
                                        manifest_hash,
                                        output_commitment,
                                        output_chunk_count,
                                        "duplicate terminal event".to_string(),
                                    );
                                    return;
                                }
                                terminal = Some((result, error));
                            }
                            _ => {
                                let _ = remote.cancel("event kind disagrees with terminal marker").await;
                                let (output_commitment, output_chunk_count) = commitment.finalize();
                                persist_runtime_outcome(
                                    &evidence_runtime,
                                    &evidence_context,
                                    EvidenceOutcome::SequenceMismatch,
                                    observed_commitment(output_commitment, output_chunk_count),
                                ).await;
                                yield failed_terminal_event(
                                    manifest_hash,
                                    output_commitment,
                                    output_chunk_count,
                                    "event kind disagrees with terminal marker".to_string(),
                                );
                                return;
                            }
                        }
                    }
                    JobRelayStreamFrameKind::Receipt { payload } => {
                        let Some((terminal_result, terminal_error)) = terminal.take() else {
                            let (output_commitment, output_chunk_count) = commitment.finalize();
                            persist_runtime_outcome(
                                &evidence_runtime,
                                &evidence_context,
                                EvidenceOutcome::MissingTerminalEvent,
                                observed_commitment(output_commitment, output_chunk_count),
                            ).await;
                            yield failed_terminal_event(
                                manifest_hash,
                                output_commitment,
                                output_chunk_count,
                                "receipt arrived without terminal event".to_string(),
                            );
                            return;
                        };
                        let (replayed_commitment, replayed_count) = commitment.finalize();
                        match verify_peer_receipt_evidence(
                            &payload,
                            manifest_hash,
                            peer_id,
                            replayed_commitment,
                            replayed_count,
                            &terminal_result,
                        ) {
                            Ok(receipt) => {
                                persist_runtime_outcome(
                                    &evidence_runtime,
                                    &evidence_context,
                                    completion_outcome(&receipt.result.completion),
                                    Some(receipt.result.output_commitment),
                                ).await;
                                producer.deliver_receipt(receipt);
                                yield JobEvent::Final {
                                    result: terminal_result,
                                    error: terminal_error,
                                };
                                let _ = remote.acknowledge_receipt().await;
                            }
                            Err(error) => {
                                warn!(peer = %peer_id, %error, "live relay receipt verification failed");
                                persist_runtime_outcome(
                                    &evidence_runtime,
                                    &evidence_context,
                                    classify_receipt_failure(&error),
                                    observed_commitment(replayed_commitment, replayed_count),
                                ).await;
                                yield failed_terminal_event(
                                    manifest_hash,
                                    replayed_commitment,
                                    replayed_count,
                                    error,
                                );
                            }
                        }
                        return;
                    }
                    JobRelayStreamFrameKind::Failed { reason } => {
                        let (output_commitment, output_chunk_count) = commitment.finalize();
                        persist_runtime_outcome(
                            &evidence_runtime,
                            &evidence_context,
                            if output_chunk_count == 0 {
                                EvidenceOutcome::PreOutputTransportFailure
                            } else {
                                EvidenceOutcome::MidStreamTransportLoss
                            },
                            observed_commitment(output_commitment, output_chunk_count),
                        ).await;
                        yield failed_terminal_event(
                            manifest_hash,
                            output_commitment,
                            output_chunk_count,
                            format!("serving peer failed: {reason}"),
                        );
                        return;
                    }
                    JobRelayStreamFrameKind::Accepted | JobRelayStreamFrameKind::Rejected { .. } => {
                        let (output_commitment, output_chunk_count) = commitment.finalize();
                        persist_runtime_outcome(
                            &evidence_runtime,
                            &evidence_context,
                            EvidenceOutcome::SequenceMismatch,
                            observed_commitment(output_commitment, output_chunk_count),
                        ).await;
                        yield failed_terminal_event(
                            manifest_hash,
                            output_commitment,
                            output_chunk_count,
                            "duplicate live relay decision".to_string(),
                        );
                        return;
                    }
                }
            }

            let (output_commitment, output_chunk_count) = commitment.finalize();
            persist_runtime_outcome(
                &evidence_runtime,
                &evidence_context,
                if terminal.is_some() {
                    EvidenceOutcome::MissingReceipt
                } else {
                    EvidenceOutcome::MissingTerminalEvent
                },
                observed_commitment(output_commitment, output_chunk_count),
            ).await;
            yield failed_terminal_event(
                manifest_hash,
                output_commitment,
                output_chunk_count,
                "live relay ended without a verified receipt".to_string(),
            );
        });
        Ok((handle, stream, ReceiptVerification::Pending))
    }

    /// Build a synthetic `(JobHandle, JobStream)` pair backed by the legacy
    /// v1 request/response wire. This path is explicitly labeled compatibility
    /// behavior and never masquerades as genuine first-token streaming.
    async fn execute_via_peer_batch(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
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

        let (events_bytes, receipt_bytes) = match response {
            JobRelayResponse::Ok { events, receipt } => (events, receipt),
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

        // A legacy batch is usable in v0.2 only when its receipt is fully
        // security-equivalent: exact signed result, job, PeerId, sequence,
        // terminal, commitment, and chunk-count bindings must all pass.
        let receipt =
            require_verified_peer_batch_receipt(&receipt_bytes, &events, manifest_hash, peer_id)?;
        self.record_remote_outcome(
            peer_id,
            &job,
            EVIDENCE_PROTOCOL_BATCH,
            completion_outcome(&receipt.result.completion),
            Some(receipt.result.output_commitment),
        )
        .await;

        // Synthesize the handle/stream pair. SEC-05: if the peer shipped a
        // receipt, deliver it through the handle so the Ollama layer's
        // `handle.finish()` resolves with the real `SignedReceipt` (matching
        // the local path) instead of `WorkerError::Dropped`.
        let (handle, mut producer) = JobHandle::new(job_id);
        producer.deliver_receipt(receipt);
        let stream: JobStream = Box::pin(stream! {
            // Keep the producer alive for the duration of the stream so the
            // delivered receipt remains available to `handle.finish()`.
            let _producer = producer;
            for ev in events {
                yield ev;
            }
        });
        Ok((handle, stream, ReceiptVerification::Verified))
    }

    /// Explicitly run one bounded redundant check. This API never participates
    /// in the requester-visible stream: it performs exactly two independent
    /// verified batch executions on distinct peers, drains them separately, and
    /// compares only their terminal commitments.
    pub async fn check_redundant_execution(
        &self,
        job: SignedManifest<JobSpec>,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
        _eligibility: DeterministicVerificationEligibility,
    ) -> Result<RedundantCheckResult, RouterError> {
        if !self.redundant_config.enabled {
            return Ok(RedundantCheckResult::SkippedDisabled);
        }
        if primary_peer == duplicate_peer {
            return Err(RouterError::Relay(
                "redundant verification requires two distinct peers".to_string(),
            ));
        }
        validate_redundant_job_eligibility(&job.payload)?;
        let manifest_hash = job
            .manifest_hash()
            .map_err(|error| RouterError::Relay(format!("manifest hash: {error}")))?;
        if !redundant_sample_selected(
            &self.identity,
            manifest_hash,
            self.redundant_config.sample_cap_permille,
        ) {
            return Ok(RedundantCheckResult::SkippedSampling);
        }
        let Ok(_permit) = Arc::clone(&self.redundant_gate).try_acquire_owned() else {
            return Ok(RedundantCheckResult::SkippedBusy);
        };

        if !self
            .redundant_peers_are_backend_equivalent(primary_peer, duplicate_peer, &job)
            .await
        {
            let result = RedundantCheckResult::Incomparable;
            self.record_redundant_result(primary_peer, duplicate_peer, &job, &result)
                .await;
            return Ok(result);
        }

        let primary = self.execute_via_peer_batch(primary_peer, job.clone());
        let duplicate = self.execute_via_peer_batch(duplicate_peer, job.clone());
        let (primary_result, duplicate_result) = tokio::join!(primary, duplicate);
        let primary_commitment = match primary_result {
            Ok((_handle, stream, ReceiptVerification::Verified)) => {
                verified_terminal_commitment(stream).await
            }
            Ok(_) => None,
            Err(error) => {
                self.record_remote_outcome(
                    primary_peer,
                    &job,
                    EVIDENCE_PROTOCOL_BATCH,
                    classify_relay_failure(&error, false),
                    None,
                )
                .await;
                None
            }
        };
        let duplicate_commitment = match duplicate_result {
            Ok((_handle, stream, ReceiptVerification::Verified)) => {
                verified_terminal_commitment(stream).await
            }
            Ok(_) => None,
            Err(error) => {
                self.record_remote_outcome(
                    duplicate_peer,
                    &job,
                    EVIDENCE_PROTOCOL_BATCH,
                    classify_relay_failure(&error, false),
                    None,
                )
                .await;
                None
            }
        };

        let result = compare_redundant_commitments(primary_commitment, duplicate_commitment);
        self.record_redundant_result(primary_peer, duplicate_peer, &job, &result)
            .await;
        Ok(result)
    }

    async fn record_redundant_result(
        &self,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
        job: &SignedManifest<JobSpec>,
        result: &RedundantCheckResult,
    ) {
        let (outcome, primary_evidence, duplicate_evidence) = match result {
            RedundantCheckResult::Agreement { commitment } => (
                EvidenceOutcome::RedundantExecutionAgreement,
                Some(*commitment),
                Some(*commitment),
            ),
            RedundantCheckResult::Disagreement {
                primary_commitment,
                duplicate_commitment,
            } => (
                EvidenceOutcome::RedundantExecutionDisagreement,
                Some(*primary_commitment),
                Some(*duplicate_commitment),
            ),
            RedundantCheckResult::Incomparable => (
                EvidenceOutcome::RedundantExecutionIncomparableResult,
                None,
                None,
            ),
            RedundantCheckResult::SkippedDisabled
            | RedundantCheckResult::SkippedSampling
            | RedundantCheckResult::SkippedBusy => return,
        };
        self.record_remote_outcome(
            primary_peer,
            job,
            EVIDENCE_PROTOCOL_REDUNDANT,
            outcome,
            primary_evidence,
        )
        .await;
        self.record_remote_outcome(
            duplicate_peer,
            job,
            EVIDENCE_PROTOCOL_REDUNDANT,
            outcome,
            duplicate_evidence,
        )
        .await;
    }
}

fn unix_time_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

fn validate_redundant_job_eligibility(job: &JobSpec) -> Result<(), RouterError> {
    match job {
        JobSpec::Inference(spec)
            if spec.resume_from.is_none()
                && spec
                    .sampling
                    .params
                    .get("seed")
                    .is_some_and(|seed| seed.parse::<u64>().is_ok())
                && ModelCid::from_hex(&spec.model_cid).is_ok() =>
        {
            Ok(())
        }
        JobSpec::Inference(_) => Err(RouterError::Relay(
            "redundant verification requires an exact model CID and a non-negative seed on non-resumed inference".to_string(),
        )),
        JobSpec::Embedding(_) => Err(RouterError::Relay(
            "embedding redundancy is disabled until an approved numeric-tolerance and backend-equivalence contract exists; byte commitments are not a cross-backend correctness metric".to_string(),
        )),
        JobSpec::Wasm(_) => Err(RouterError::Relay(
            "redundant verification is not supported for WASM jobs".to_string(),
        )),
        _ => Err(RouterError::Relay(
            "redundant verification is not supported for this job kind".to_string(),
        )),
    }
}

fn redundant_sample_selected(
    identity: &NodeIdentity,
    manifest_hash: [u8; 32],
    sample_cap_permille: u16,
) -> bool {
    if sample_cap_permille == 0 {
        return false;
    }
    if sample_cap_permille >= 1_000 {
        return true;
    }
    let mut material = Vec::with_capacity(REDUNDANT_SAMPLE_DOMAIN.len() + manifest_hash.len());
    material.extend_from_slice(REDUNDANT_SAMPLE_DOMAIN);
    material.extend_from_slice(&manifest_hash);
    let keyed_sample = identity.sign(&material).to_bytes();
    u16::from_be_bytes([keyed_sample[0], keyed_sample[1]]) % 1_000 < sample_cap_permille
}

async fn verified_terminal_commitment(mut stream: JobStream) -> Option<[u8; 32]> {
    let mut terminal_commitment = None;
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        match event {
            JobEvent::Final { result, error }
                if terminal_commitment.is_none()
                    && error.is_none()
                    && matches!(result.completion, Completion::Stop | Completion::Length) =>
            {
                terminal_commitment = Some(result.output_commitment);
            }
            JobEvent::Final { .. } => return None,
            JobEvent::Output(_) | JobEvent::Progress(_) if terminal_commitment.is_none() => {}
            _ => return None,
        }
    }
    terminal_commitment
}

fn compare_redundant_commitments(
    primary_commitment: Option<[u8; 32]>,
    duplicate_commitment: Option<[u8; 32]>,
) -> RedundantCheckResult {
    match (primary_commitment, duplicate_commitment) {
        (Some(primary), Some(duplicate)) if primary == duplicate => {
            RedundantCheckResult::Agreement {
                commitment: primary,
            }
        }
        (Some(primary_commitment), Some(duplicate_commitment)) => {
            RedundantCheckResult::Disagreement {
                primary_commitment,
                duplicate_commitment,
            }
        }
        _ => RedundantCheckResult::Incomparable,
    }
}

fn capabilities_are_redundancy_equivalent(
    primary: &ModelCapabilities,
    duplicate: &ModelCapabilities,
) -> bool {
    primary.model_cid == duplicate.model_cid
        && primary.backend == duplicate.backend
        && primary.quantization == duplicate.quantization
}

fn evidence_context(
    observer_peer_id: PeerId,
    remote_peer_id: PeerId,
    job: &SignedManifest<JobSpec>,
    protocol_version: &'static str,
) -> Result<EvidenceContext, String> {
    let job_spec_hash = job
        .manifest_hash()
        .map_err(|error| format!("manifest hash: {error}"))?;
    let model_cid = match &job.payload {
        JobSpec::Inference(spec) => ModelCid::from_hex(&spec.model_cid),
        JobSpec::Embedding(spec) => ModelCid::from_hex(&spec.model_cid),
        JobSpec::Wasm(_) => return Err("WASM is not routable through LUCID peers".to_string()),
        _ => return Err("unsupported LUCID evidence job kind".to_string()),
    }
    .map_err(|error| format!("evidence model CID: {error}"))?;
    Ok(EvidenceContext {
        observer_peer_id,
        remote_peer_id,
        job_spec_hash,
        job_class: job.payload.kind(),
        model_cid,
        protocol_version: protocol_version.to_string(),
        software_version: EVIDENCE_SOFTWARE_VERSION.to_string(),
        observed_at_unix_ms: unix_time_ms(),
    })
}

async fn persist_runtime_outcome(
    runtime: &Option<Arc<EvidenceRuntime>>,
    context: &Option<EvidenceContext>,
    outcome: EvidenceOutcome,
    output_commitment: Option<[u8; 32]>,
) {
    let (Some(runtime), Some(context)) = (runtime, context) else {
        return;
    };
    if let Err(error) = runtime
        .record(context.clone(), outcome, output_commitment)
        .await
    {
        error!(?outcome, %error, "failed to persist remote execution evidence");
    }
}

fn observed_commitment(commitment: [u8; 32], chunk_count: u64) -> Option<[u8; 32]> {
    (chunk_count > 0).then_some(commitment)
}

fn completion_outcome(completion: &Completion) -> EvidenceOutcome {
    match completion {
        Completion::Stop | Completion::Length => EvidenceOutcome::VerifiedSuccessfulCompletion,
        Completion::Cancelled => EvidenceOutcome::VerifiedCancellation,
        Completion::Error => EvidenceOutcome::VerifiedWorkerError,
        _ => EvidenceOutcome::VerifiedWorkerError,
    }
}

fn classify_receipt_failure(reason: &str) -> EvidenceOutcome {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("no terminal receipt") || reason.contains("no receipt") {
        EvidenceOutcome::MissingReceipt
    } else if reason.contains("signature")
        || reason.contains("receipt schema")
        || reason.contains("receipt failed to decode")
    {
        EvidenceOutcome::InvalidReceiptSignature
    } else if reason.contains("worker key") || reason.contains("peerid") {
        EvidenceOutcome::SignerPeerIdMismatch
    } else if reason.contains("job_id") || reason.contains("job result") {
        EvidenceOutcome::JobMismatch
    } else if reason.contains("job_spec_hash") || reason.contains("manifest") {
        EvidenceOutcome::ManifestMismatch
    } else if reason.contains("chunk count") {
        EvidenceOutcome::ChunkCountMismatch
    } else if reason.contains("commitment") || reason.contains("delivered output") {
        EvidenceOutcome::OutputCommitmentMismatch
    } else {
        EvidenceOutcome::InvalidReceiptSignature
    }
}

fn classify_relay_failure(error: &RouterError, output_started: bool) -> EvidenceOutcome {
    let reason = error.to_string().to_ascii_lowercase();
    if reason.contains("remote does not support")
        || reason.contains("no live relay handler installed")
        || reason.contains("discovery")
    {
        EvidenceOutcome::PreOutputDiscoveryFailure
    } else if reason.contains("peer refused") || reason.contains("policy") {
        if reason.contains("busy") || reason.contains("capacity") || reason.contains("concurrent") {
            EvidenceOutcome::CapacityRefusal
        } else {
            EvidenceOutcome::PolicyRefusal
        }
    } else if reason.contains("timed out") || reason.contains("deadline") {
        EvidenceOutcome::DeadlineTimeout
    } else if reason.contains("no terminal receipt") || reason.contains("no receipt") {
        EvidenceOutcome::MissingReceipt
    } else if reason.contains("signature")
        || reason.contains("receipt schema")
        || reason.contains("receipt failed to decode")
    {
        EvidenceOutcome::InvalidReceiptSignature
    } else if reason.contains("worker key") || reason.contains("peerid") {
        EvidenceOutcome::SignerPeerIdMismatch
    } else if reason.contains("job_id") || reason.contains("job result") {
        EvidenceOutcome::JobMismatch
    } else if reason.contains("job_spec_hash") || reason.contains("manifest") {
        EvidenceOutcome::ManifestMismatch
    } else if reason.contains("chunk count") {
        EvidenceOutcome::ChunkCountMismatch
    } else if reason.contains("commitment") || reason.contains("delivered output") {
        EvidenceOutcome::OutputCommitmentMismatch
    } else if reason.contains("sequence")
        || reason.contains("decode peer events")
        || reason.contains("reordered")
        || reason.contains("duplicate terminal")
        || reason.contains("after terminal")
    {
        EvidenceOutcome::SequenceMismatch
    } else if reason.contains("missing terminal") || reason.contains("without terminal") {
        EvidenceOutcome::MissingTerminalEvent
    } else if reason.contains("receipt") {
        classify_receipt_failure(&reason)
    } else if output_started {
        EvidenceOutcome::MidStreamTransportLoss
    } else {
        EvidenceOutcome::PreOutputTransportFailure
    }
}

fn failed_terminal_event(
    manifest_hash: [u8; 32],
    output_commitment: [u8; 32],
    output_chunk_count: u64,
    error: String,
) -> JobEvent {
    JobEvent::Final {
        result: JobResult {
            job_spec_hash: manifest_hash,
            output_commitment,
            output_chunk_count,
            completion: Completion::Error,
            resumption: None,
            metrics: JobMetrics::default(),
        },
        error: Some(error),
    }
}

fn verify_peer_receipt_evidence(
    receipt_bytes: &[u8],
    manifest_hash: [u8; 32],
    peer_id: PeerId,
    replayed_commitment: [u8; 32],
    replayed_count: u64,
    terminal_result: &JobResult,
) -> Result<SignedReceipt<JobResult>, String> {
    if receipt_bytes.is_empty() {
        return Err("peer returned no terminal receipt".to_string());
    }
    let receipt: SignedReceipt<JobResult> = serde_json::from_slice(receipt_bytes)
        .map_err(|error| format!("receipt failed to decode: {error}"))?;
    if receipt.schema_version != phase_receipt::SCHEMA_VERSION {
        return Err(format!(
            "unsupported receipt schema {}; expected {}",
            receipt.schema_version,
            phase_receipt::SCHEMA_VERSION
        ));
    }
    receipt
        .verify()
        .map_err(|error| format!("receipt signature verification failed: {error}"))?;
    if receipt.job_id_bytes() != Some(manifest_hash) {
        return Err("receipt job_id does not match dispatched manifest".to_string());
    }
    if worker_pubkey_to_peer_id(&receipt.worker_pubkey) != Some(peer_id) {
        return Err("receipt worker key does not match dispatched PeerId".to_string());
    }
    if terminal_result.job_spec_hash != manifest_hash
        || receipt.result.job_spec_hash != manifest_hash
    {
        return Err("terminal/receipt job_spec_hash does not match manifest".to_string());
    }
    if replayed_count != terminal_result.output_chunk_count {
        return Err("delivered output chunk count does not match terminal result".to_string());
    }
    if replayed_commitment != terminal_result.output_commitment {
        return Err("delivered output does not match terminal commitment".to_string());
    }
    if receipt.result != *terminal_result {
        return Err("signed receipt job result does not exactly match terminal result".to_string());
    }
    Ok(receipt)
}

/// SEC-05: verify a peer-served `SignedReceipt<JobResult>` and bind it to the
/// dispatched job and delivering peer.
///
/// Checks, in order:
/// 1. **Signature** — `receipt.verify()` proves the `worker_pubkey` signed
///    this exact `(job_id, JobResult)`.
/// 2. **job_id bind** — `receipt.job_id` must equal the `manifest_hash` we
///    dispatched (a malicious worker can sign any job_id it likes, so this
///    pins the receipt to *our* request).
/// 3. **worker-pubkey → PeerId bind** — the Ed25519 `worker_pubkey` must
///    derive to the libp2p `PeerId` we dispatched to (same primitive as
///    `registry.rs::peer_id_from_ed25519_pubkey`), so a third party can't
///    relay someone else's valid receipt.
/// 4. **commitment replay** — recompute the `CommitmentAccumulator` over the
///    received `OutputChunk`s and compare to the signed
///    `result.output_commitment` (+ chunk count), detecting tampered or
///    truncated output.
#[cfg(test)]
fn verify_peer_receipt(
    receipt_bytes: &[u8],
    events: &[JobEvent],
    manifest_hash: [u8; 32],
    peer_id: PeerId,
) -> ReceiptVerification {
    if receipt_bytes.is_empty() {
        warn!(peer = %peer_id, "relay: peer returned no receipt (pre-SEC-05 node) — unverifiable");
        return ReceiptVerification::Unverifiable;
    }
    match verified_peer_receipt_from_events(receipt_bytes, events, manifest_hash, peer_id) {
        Ok(_) => {
            debug!(peer = %peer_id, job = %JobId(manifest_hash), "relay: receipt verified + bound");
            ReceiptVerification::Verified
        }
        Err(error) => {
            warn!(peer = %peer_id, %error, "relay: receipt verification FAILED");
            ReceiptVerification::Failed
        }
    }
}

fn verified_peer_receipt_from_events(
    receipt_bytes: &[u8],
    events: &[JobEvent],
    manifest_hash: [u8; 32],
    peer_id: PeerId,
) -> Result<SignedReceipt<JobResult>, String> {
    let mut acc = CommitmentAccumulator::new();
    let mut final_result: Option<&JobResult> = None;
    let mut expected_output_sequence = 0_u64;
    for event in events {
        match event {
            JobEvent::Output(chunk)
                if final_result.is_none() && chunk.seq == expected_output_sequence =>
            {
                expected_output_sequence = expected_output_sequence.saturating_add(1);
                acc.update(chunk);
            }
            JobEvent::Progress(_) if final_result.is_none() => {}
            JobEvent::Final { result, .. } if final_result.is_none() => {
                final_result = Some(result);
            }
            JobEvent::Output(chunk) if final_result.is_none() => {
                return Err(format!(
                    "output sequence mismatch: expected {expected_output_sequence}, got {}",
                    chunk.seq
                ));
            }
            JobEvent::Final { .. } => return Err("duplicate terminal event".to_string()),
            JobEvent::Output(_) | JobEvent::Progress(_) => {
                return Err("event occurred after terminal event".to_string());
            }
            _ => {}
        }
    }
    let terminal_result = final_result.ok_or_else(|| "missing terminal event".to_string())?;
    let (replayed_commitment, replayed_count) = acc.finalize();
    verify_peer_receipt_evidence(
        receipt_bytes,
        manifest_hash,
        peer_id,
        replayed_commitment,
        replayed_count,
        terminal_result,
    )
}

/// Enforce the v1 compatibility boundary used by
/// [`Router::execute_via_peer_batch`]. Classification remains useful for
/// observability, but a legacy batch is never executable unless its receipt
/// and complete event transcript are fully verified.
fn require_verified_peer_batch_receipt(
    receipt_bytes: &[u8],
    events: &[JobEvent],
    manifest_hash: [u8; 32],
    peer_id: PeerId,
) -> Result<SignedReceipt<JobResult>, RouterError> {
    verified_peer_receipt_from_events(receipt_bytes, events, manifest_hash, peer_id)
        .map_err(RouterError::Relay)
}

/// Derive a libp2p `PeerId` from a hex-encoded Ed25519 verifying key (the
/// `worker_pubkey` field of a `SignedReceipt`). Same primitive as
/// `registry.rs::peer_id_from_ed25519_pubkey`, adapted for the hex input the
/// receipt carries. Returns `None` on malformed hex / invalid key bytes.
fn worker_pubkey_to_peer_id(pubkey_hex: &str) -> Option<PeerId> {
    use phase_net::libp2p_identity::{ed25519, PublicKey};
    let bytes = hex_decode_32(pubkey_hex)?;
    let ed = ed25519::PublicKey::try_from_bytes(&bytes).ok()?;
    let pk: PublicKey = ed.into();
    Some(PeerId::from(pk))
}

fn node_identity_peer_id(identity: &NodeIdentity) -> PeerId {
    use phase_net::libp2p_identity::{ed25519, PublicKey};
    let bytes = identity.verifying_key().to_bytes();
    let key = ed25519::PublicKey::try_from_bytes(&bytes)
        .expect("NodeIdentity always contains a valid Ed25519 public key");
    PeerId::from(PublicKey::from(key))
}

/// Decode exactly 32 bytes from a lowercase/uppercase hex string. `None` if
/// the length is wrong or any nibble is non-hex.
fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[2 * i] as char).to_digit(16)?;
        let lo = (bytes[2 * i + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RemoteReplayKey {
    signer_pubkey: String,
    manifest_hash: [u8; 32],
}

/// Process-local, fail-closed replay admission shared by v1 and v2. Entries
/// remain until the signed manifest expires; when the bounded table is full of
/// still-valid entries, new work is refused rather than evicting replay proof.
/// Restart persistence remains an explicit residual risk for this release.
#[derive(Default)]
struct RemoteReplayCache {
    expires_at_unix_ms: AsyncMutex<HashMap<RemoteReplayKey, i64>>,
}

/// Admission counter whose ceiling is supplied from the current policy for
/// every request. Unlike a fixed-size semaphore, lowering
/// `max_concurrent_remote_jobs` during a policy reload takes effect
/// immediately: existing jobs finish, while no new job is admitted until the
/// active count falls below the new ceiling.
#[derive(Default)]
struct RemoteConcurrencyGate {
    active: AtomicUsize,
}

impl RemoteConcurrencyGate {
    fn try_acquire(self: &Arc<Self>, configured_limit: u32) -> Option<RemoteExecutionPermit> {
        let limit = configured_limit.max(1) as usize;
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= limit {
                return None;
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(RemoteExecutionPermit { gate: self.clone() });
                }
                Err(observed) => active = observed,
            }
        }
    }
}

struct RemoteExecutionPermit {
    gate: Arc<RemoteConcurrencyGate>,
}

impl Drop for RemoteExecutionPermit {
    fn drop(&mut self) {
        let previous = self.gate.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "remote execution permit underflow");
    }
}

#[derive(Clone)]
struct InboundRelayContext {
    worker: Arc<dyn DynWorker>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
    concurrency: Arc<RemoteConcurrencyGate>,
    replay_cache: Arc<RemoteReplayCache>,
}

impl InboundRelayContext {
    fn new(
        worker: Arc<dyn DynWorker>,
        registry: Arc<ModelRegistry>,
        policy: Arc<PolicyEngine>,
    ) -> Self {
        Self {
            worker,
            registry,
            policy,
            concurrency: Arc::new(RemoteConcurrencyGate::default()),
            replay_cache: Arc::new(RemoteReplayCache::default()),
        }
    }
}

impl RemoteReplayCache {
    async fn admit(
        &self,
        job: &SignedManifest<JobSpec>,
        manifest_hash: [u8; 32],
    ) -> Result<(), String> {
        let expires_at = job
            .expires_at
            .as_ref()
            .ok_or_else(|| "remote manifest expiry missing after verification".to_string())?
            .timestamp_millis();
        let now = chrono::Utc::now().timestamp_millis();
        let key = RemoteReplayKey {
            signer_pubkey: job.signer_pubkey.clone(),
            manifest_hash,
        };
        let mut entries = self.expires_at_unix_ms.lock().await;
        entries.retain(|_, expiry| *expiry > now);
        if entries.contains_key(&key) {
            return Err("remote manifest replay rejected".to_string());
        }
        if entries.len() >= MAX_REMOTE_REPLAY_ENTRIES {
            return Err("remote replay cache capacity reached; refusing new work".to_string());
        }
        entries.insert(key, expires_at);
        Ok(())
    }
}

fn validate_remote_embedding_input(input: &[String]) -> Result<usize, String> {
    if input.is_empty() {
        return Err("embedding input must contain at least one non-empty entry".to_string());
    }
    if input.len() > MAX_EMBEDDING_INPUTS {
        return Err(format!(
            "embedding input count {} exceeds {MAX_EMBEDDING_INPUTS}",
            input.len()
        ));
    }
    let mut total_chars = 0_usize;
    for (index, entry) in input.iter().enumerate() {
        let entry_chars = entry.chars().count();
        if entry_chars == 0 {
            return Err(format!("embedding input {index} is empty"));
        }
        if entry_chars > MAX_EMBEDDING_ENTRY_CHARS {
            return Err(format!(
                "embedding input {index} exceeds {MAX_EMBEDDING_ENTRY_CHARS} characters"
            ));
        }
        total_chars = total_chars
            .checked_add(entry_chars)
            .ok_or_else(|| "embedding input character count overflow".to_string())?;
        if total_chars > MAX_PROMPT_CHARS {
            return Err(format!(
                "embedding input exceeds {MAX_PROMPT_CHARS} aggregate characters"
            ));
        }
    }
    Ok(total_chars)
}

async fn validate_and_execute_inbound_relay(
    delivering_peer: PeerId,
    bytes: Vec<u8>,
    expected_job_id: Option<[u8; 32]>,
    context: InboundRelayContext,
) -> Result<(RemoteExecutionPermit, JobHandle, JobStream), String> {
    let job: SignedManifest<JobSpec> =
        serde_json::from_slice(&bytes).map_err(|e| format!("decode SignedManifest: {e}"))?;
    job.verify_for_remote_execution()
        .map_err(|e| format!("remote manifest verification failed: {e}"))?;
    let manifest_hash = job
        .manifest_hash()
        .map_err(|e| format!("manifest hash: {e}"))?;
    if expected_job_id.is_some_and(|expected| expected != manifest_hash) {
        return Err("stream job_id does not match signed manifest hash".to_string());
    }

    // Attribution and authorization are separate requirements. Except for
    // the explicit insecure development escape hatch, a remote submitter
    // must be operator-authorized AND use that same identity on libp2p.
    let config = context.policy.config();
    let authorized = context.policy.is_authorized_submitter(&job.signer_pubkey);
    let peer_bound = signer_matches_peer(&job.signer_pubkey, delivering_peer);
    if !authorized || (!config.allow_unauthenticated_jobs && !peer_bound) {
        return Err("submitter not authorized or signer does not match delivering PeerId".into());
    }

    if let JobSpec::Inference(spec) = &job.payload {
        let ceiling = config.max_tokens_ceiling.max(1);
        if !matches!(spec.max_tokens, Some(1..=u32::MAX))
            || spec.max_tokens.is_some_and(|tokens| tokens > ceiling)
        {
            return Err(format!(
                "max_tokens must be present and within 1..={ceiling} for remote execution"
            ));
        }
    }
    let prompt_chars = match &job.payload {
        JobSpec::Inference(spec) => {
            spec.prompt.as_ref().map(|p| p.len()).unwrap_or(0)
                + spec.messages.iter().map(|m| m.content.len()).sum::<usize>()
        }
        JobSpec::Embedding(spec) => validate_remote_embedding_input(&spec.input)?,
        _ => return Err("job kind not supported over relay".to_string()),
    };
    if prompt_chars > MAX_PROMPT_CHARS {
        return Err(format!(
            "prompt too large: {prompt_chars} chars > {MAX_PROMPT_CHARS} cap"
        ));
    }
    let model_cid = match &job.payload {
        JobSpec::Inference(spec) => &spec.model_cid,
        JobSpec::Embedding(spec) => &spec.model_cid,
        _ => return Err("job kind not supported over relay".to_string()),
    };
    let requested_cid = ModelCid::from_hex(model_cid)
        .map_err(|error| format!("invalid remote model_cid '{model_cid}': {error}"))?;
    let local_models = context.registry.local_models_async().await;
    let local_model = local_models
        .iter()
        .find(|caps| caps.model_cid == requested_cid)
        .ok_or_else(|| format!("model CID '{model_cid}' not loaded on this peer"))?;
    if let PolicyDecision::Pause { reason } = context.policy.should_serve(&local_model.model_id, 0)
    {
        return Err(pause_reason_string(&reason));
    }

    let permit = context
        .concurrency
        .try_acquire(config.max_concurrent_remote_jobs)
        .ok_or_else(|| "busy: max concurrent remote jobs reached".to_string())?;
    context.replay_cache.admit(&job, manifest_hash).await?;

    let (handle, stream) = context
        .worker
        .execute_boxed(job)
        .await
        .map_err(|e| format!("local worker dispatch failed: {e}"))?;
    Ok((permit, handle, stream))
}

/// Both inbound relay protocol handlers, backed by one remote-execution
/// semaphore. Installing this bundle prevents a peer from consuming the v1
/// and v2 limits independently and reaching twice the operator's cap.
pub struct InboundRelayHandlers {
    pub batch: phase_net::JobRelayHandler,
    pub stream: JobRelayStreamHandler,
}

/// Build v1 and v2 inbound relay handlers with one shared concurrency gate.
pub fn make_inbound_relay_handlers(
    worker: Arc<dyn DynWorker>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
) -> InboundRelayHandlers {
    let context = InboundRelayContext::new(worker, registry, policy);
    InboundRelayHandlers {
        batch: make_inbound_relay_handler_with_context(context.clone()),
        stream: make_inbound_relay_stream_handler_with_context(context),
    }
}

/// Compatibility constructor for callers that install only v2. Daemons that
/// install both protocols must use [`make_inbound_relay_handlers`].
pub fn make_inbound_relay_stream_handler(
    worker: Arc<dyn DynWorker>,
    registry: Arc<ModelRegistry>,
    policy: Arc<PolicyEngine>,
) -> JobRelayStreamHandler {
    make_inbound_relay_stream_handler_with_context(InboundRelayContext::new(
        worker, registry, policy,
    ))
}

fn make_inbound_relay_stream_handler_with_context(
    context: InboundRelayContext,
) -> JobRelayStreamHandler {
    Arc::new(move |delivering_peer, open, mut controls, frames| {
        let context = context.clone();
        Box::pin(async move {
            let deadline_after =
                Duration::from_millis(open.deadline_unix_ms.saturating_sub(unix_time_ms()))
                    .min(INBOUND_RELAY_JOB_TIMEOUT);
            let deadline = TokioInstant::now() + deadline_after;
            let execution = match timeout_at(
                deadline,
                validate_and_execute_inbound_relay(
                    delivering_peer,
                    open.payload,
                    Some(open.job_id),
                    context,
                ),
            )
            .await
            {
                Ok(execution) => execution,
                Err(_) => Err("remote job deadline reached before acceptance".to_string()),
            };
            let (_permit, handle, mut stream) = match execution {
                Ok(execution) => execution,
                Err(reason) => {
                    let reason = bounded_reason(&reason);
                    let _ = send_live_handler_frame_before(
                        &frames,
                        JobRelayStreamFrame {
                            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                            job_id: open.job_id,
                            sequence: 0,
                            kind: JobRelayStreamFrameKind::Rejected { reason },
                        },
                        deadline,
                    )
                    .await;
                    return;
                }
            };

            if !send_live_handler_frame_before(
                &frames,
                JobRelayStreamFrame {
                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                    job_id: open.job_id,
                    sequence: 0,
                    kind: JobRelayStreamFrameKind::Accepted,
                },
                deadline,
            )
            .await
            {
                handle.cancel();
                return;
            }

            let mut sequence = 1_u64;
            let mut saw_terminal = false;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {
                        handle.cancel();
                        let _ = frames.try_send(JobRelayStreamFrame {
                            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                            job_id: open.job_id,
                            sequence,
                            kind: JobRelayStreamFrameKind::Failed {
                                reason: "remote job deadline reached".to_string(),
                            },
                        });
                        return;
                    }
                    control = controls.recv() => {
                        match control.map(|control| control.kind) {
                            Some(JobRelayStreamControlKind::Cancel { .. }) | None => {
                                handle.cancel();
                            }
                            Some(JobRelayStreamControlKind::ReceiptAck) => {
                                handle.cancel();
                                return;
                            }
                        }
                    }
                    event = futures::StreamExt::next(&mut stream) => {
                        let Some(event) = event else {
                            break;
                        };
                        let terminal = matches!(event, JobEvent::Final { .. });
                        if saw_terminal {
                            handle.cancel();
                            return;
                        }
                        saw_terminal = terminal;
                        let payload = match serde_json::to_vec(&event) {
                            Ok(payload) => payload,
                            Err(error) => {
                                let _ = send_live_handler_frame_before(&frames, JobRelayStreamFrame {
                                    schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                    job_id: open.job_id,
                                    sequence,
                                    kind: JobRelayStreamFrameKind::Failed {
                                        reason: bounded_reason(&format!("encode worker event: {error}")),
                                    },
                                }, deadline).await;
                                handle.cancel();
                                return;
                            }
                        };
                        if !send_live_handler_frame_before(&frames, JobRelayStreamFrame {
                            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                            job_id: open.job_id,
                            sequence,
                            kind: JobRelayStreamFrameKind::Event { payload, terminal },
                        }, deadline).await {
                            handle.cancel();
                            return;
                        }
                        sequence = sequence.saturating_add(1);
                        if terminal {
                            break;
                        }
                    }
                }
            }

            if !saw_terminal {
                let _ = send_live_handler_frame_before(
                    &frames,
                    JobRelayStreamFrame {
                        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                        job_id: open.job_id,
                        sequence,
                        kind: JobRelayStreamFrameKind::Failed {
                            reason: "worker stream ended without terminal event".to_string(),
                        },
                    },
                    deadline,
                )
                .await;
                handle.cancel();
                return;
            }

            match timeout_at(deadline, handle.finish()).await {
                Err(_) => {
                    let _ = frames.try_send(JobRelayStreamFrame {
                        schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                        job_id: open.job_id,
                        sequence,
                        kind: JobRelayStreamFrameKind::Failed {
                            reason: "remote job receipt deadline reached".to_string(),
                        },
                    });
                }
                Ok(Ok(receipt)) => match serde_json::to_vec(&receipt) {
                    Ok(payload) => {
                        let _ = send_live_handler_frame_before(
                            &frames,
                            JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence,
                                kind: JobRelayStreamFrameKind::Receipt { payload },
                            },
                            deadline,
                        )
                        .await;
                    }
                    Err(error) => {
                        let _ = send_live_handler_frame_before(
                            &frames,
                            JobRelayStreamFrame {
                                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                                job_id: open.job_id,
                                sequence,
                                kind: JobRelayStreamFrameKind::Failed {
                                    reason: bounded_reason(&format!("encode receipt: {error}")),
                                },
                            },
                            deadline,
                        )
                        .await;
                    }
                },
                Ok(Err(error)) => {
                    let _ = send_live_handler_frame_before(
                        &frames,
                        JobRelayStreamFrame {
                            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                            job_id: open.job_id,
                            sequence,
                            kind: JobRelayStreamFrameKind::Failed {
                                reason: bounded_reason(&format!(
                                    "worker produced no receipt: {error}"
                                )),
                            },
                        },
                        deadline,
                    )
                    .await;
                }
            }
        })
    })
}

fn bounded_reason(reason: &str) -> String {
    const MAX: usize = 1024;
    if reason.len() <= MAX {
        return reason.to_string();
    }
    let mut end = MAX;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_string()
}

async fn send_live_handler_frame_before(
    frames: &tokio::sync::mpsc::Sender<JobRelayStreamFrame>,
    frame: JobRelayStreamFrame,
    deadline: TokioInstant,
) -> bool {
    matches!(timeout_at(deadline, frames.send(frame)).await, Ok(Ok(())))
}

/// Build the `JobRelayHandler` that `phase_net::Discovery` will invoke
/// when a peer asks us to run a job on its behalf.
///
/// The serving side:
/// 1. Decodes the JSON `SignedManifest<JobSpec>`.
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
    make_inbound_relay_handler_with_context(InboundRelayContext::new(worker, registry, policy))
}

fn make_inbound_relay_handler_with_context(
    context: InboundRelayContext,
) -> phase_net::JobRelayHandler {
    make_inbound_relay_handler_with_context_and_timeout(context, INBOUND_RELAY_JOB_TIMEOUT)
}

fn make_inbound_relay_handler_with_context_and_timeout(
    context: InboundRelayContext,
    job_timeout: Duration,
) -> phase_net::JobRelayHandler {
    Arc::new(move |delivering_peer: PeerId, bytes: Vec<u8>| {
        let context = context.clone();
        Box::pin(async move {
            let deadline = TokioInstant::now() + job_timeout;
            let execution = match timeout_at(
                deadline,
                validate_and_execute_inbound_relay(delivering_peer, bytes, None, context),
            )
            .await
            {
                Ok(execution) => execution,
                Err(_) => Err("remote batch job deadline reached before dispatch".to_string()),
            };
            // Keep the shared validator's permit alive through the entire v1
            // batch drain and receipt/response encoding, matching the legacy
            // handler's semaphore lifetime.
            let (_permit, handle, mut stream) = match execution {
                Ok(execution) => execution,
                Err(reason) => {
                    warn!(%reason, "relay: rejecting inbound batch request");
                    return JobRelayResponse::Err { reason };
                }
            };

            let mut events: Vec<JobEvent> = Vec::new();
            loop {
                match timeout_at(deadline, futures::StreamExt::next(&mut stream)).await {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(_) => {
                        handle.cancel();
                        return JobRelayResponse::Err {
                            reason: "remote batch job drain deadline reached".to_string(),
                        };
                    }
                }
            }
            // SEC-05: ship the worker's `SignedReceipt<JobResult>` back in the
            // relay response so the requesting side can verify + bind it. The
            // commitment also rides inside `JobEvent::Final`, but only the
            // signed receipt proves *which worker* produced *which job*.
            let receipt_bytes = match timeout_at(deadline, handle.finish()).await {
                Ok(Ok(receipt)) => match serde_json::to_vec(&receipt) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(error = %e, "relay: failed to encode receipt; returning unverifiable batch");
                        Vec::new()
                    }
                },
                Ok(Err(e)) => {
                    // No receipt available (worker dropped). Return the events
                    // anyway; the requester treats an empty receipt as
                    // unverifiable rather than a hard failure.
                    warn!(error = %e, "relay: worker produced no receipt");
                    Vec::new()
                }
                Err(_) => {
                    return JobRelayResponse::Err {
                        reason: "remote batch receipt deadline reached".to_string(),
                    };
                }
            };

            let encoded = match serde_json::to_vec(&events) {
                Ok(b) => b,
                Err(e) => {
                    return JobRelayResponse::Err {
                        reason: format!("encode events: {e}"),
                    };
                }
            };
            JobRelayResponse::Ok {
                events: encoded,
                receipt: receipt_bytes,
            }
        }) as _
    })
}

/// SEC-06 PeerID-bind: does the manifest's hex `signer_pubkey` derive to the
/// libp2p `PeerId` that delivered the request? Returns `false` on malformed
/// hex / invalid key (fail-closed). Reuses the same Ed25519→PeerId primitive
/// as receipt verification (`worker_pubkey_to_peer_id`).
fn signer_matches_peer(signer_pubkey_hex: &str, delivering_peer: PeerId) -> bool {
    worker_pubkey_to_peer_id(signer_pubkey_hex)
        .map(|derived| derived == delivering_peer)
        .unwrap_or(false)
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

        let decision = router.route("qwen3-big", false).await;
        match &decision.via {
            RouteVia::Peer { peer_id } => {
                // Re-derive the expected peer-id from the foreign
                // identity to assert we picked the right one.
                use phase_net::libp2p_identity::{ed25519, PublicKey};
                let ed = ed25519::PublicKey::try_from_bytes(&foreign.verifying_key().to_bytes())
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
    async fn route_carries_fallback_peers() {
        // Multiple peers advertise the SAME model id (different signer
        // identities → different PeerIds). The router must pick the first as
        // primary and carry the rest as ranked fallbacks for relay failover.
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockDht::default());

        // Two foreign nodes both advertise "qwen3-big" under the same CID.
        let foreign_a = NodeIdentity::generate();
        let foreign_b = NodeIdentity::generate();
        let caps = sample_caps("qwen3-big", 9);
        let cid = caps.model_cid;
        for foreign in [&foreign_a, &foreign_b] {
            let ad = crate::registry::SignedModelAdvertisement::sign(
                sample_caps("qwen3-big", 9),
                foreign,
            )
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
        let router = Router::new(None, registry, policy, identity, build_test_discovery());

        let decision = router.route("qwen3-big", false).await;
        match &decision.via {
            RouteVia::Peer { .. } => {
                assert!(
                    !decision.fallback_peers.is_empty(),
                    "two advertisements should yield at least one fallback peer, got {:?}",
                    decision.fallback_peers
                );
            }
            other => panic!("expected Peer, got {:?}", other),
        }
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
        let runtime = Arc::new(
            EvidenceRuntime::open(temp.path().join("route-evidence.log"), observer).unwrap(),
        );
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
        let (_handle, mut stream, verification) =
            router.execute(&decision, manifest).await.unwrap();
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
            .with_evidence_runtime(runtime);
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

        let permit = enabled
            .try_begin_redundant_probe(&deterministic, primary, duplicate)
            .expect("eligible job gets the sole non-queueing permit");
        assert_eq!(enabled.redundant_gate.available_permits(), 0);
        assert!(enabled
            .try_begin_redundant_probe(&deterministic, primary, PeerId::random())
            .is_none());
        drop(permit);
        assert_eq!(enabled.redundant_gate.available_permits(), 1);
        assert!(enabled
            .try_begin_redundant_probe(&deterministic, primary, primary)
            .is_none());
    }

    #[test]
    fn redundant_sampling_and_commitment_comparison_are_bounded_and_literal() {
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

        assert_eq!(
            compare_redundant_commitments(Some([1; 32]), Some([1; 32])),
            RedundantCheckResult::Agreement {
                commitment: [1; 32]
            }
        );
        assert_eq!(
            compare_redundant_commitments(Some([1; 32]), Some([2; 32])),
            RedundantCheckResult::Disagreement {
                primary_commitment: [1; 32],
                duplicate_commitment: [2; 32],
            }
        );
        assert_eq!(
            compare_redundant_commitments(Some([1; 32]), None),
            RedundantCheckResult::Incomparable
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

        // A commitment mismatch from non-equivalent execution is never fed
        // to `compare_redundant_commitments`; the gate yields the neutral,
        // non-negative evidence class instead.
        let result = if capabilities_are_redundancy_equivalent(&primary, &different_backend) {
            compare_redundant_commitments(Some([1; 32]), Some([2; 32]))
        } else {
            RedundantCheckResult::Incomparable
        };
        assert_eq!(result, RedundantCheckResult::Incomparable);
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
            require_verified_peer_batch_receipt(receipt, events, manifest_hash, worker_peer)
                .is_err(),
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

    #[tokio::test]
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
                    deadline_unix_ms: unix_time_ms() + 5_000,
                    idle_timeout_ms: 1_000,
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

        worker_gate.add_permits(1);
        let accepted = tokio::time::timeout(Duration::from_secs(1), frames_rx.recv())
            .await
            .expect("v2 unblocks")
            .expect("v2 sends decision");
        assert!(matches!(accepted.kind, JobRelayStreamFrameKind::Accepted));
        while frames_rx.recv().await.is_some() {}
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
        let client_net = Arc::new(
            Discovery::new(phase_net::DiscoveryConfig {
                identity: Some(client_identity.clone()),
                ..phase_net::DiscoveryConfig::default()
            })
            .unwrap(),
        );
        let worker_net = Arc::new(
            Discovery::new(phase_net::DiscoveryConfig {
                identity: Some(worker_identity.clone()),
                ..phase_net::DiscoveryConfig::default()
            })
            .unwrap(),
        );
        let worker_peer = *worker_net.local_peer_id();
        worker_net.listen("/ip4/127.0.0.1/tcp/0").await.unwrap();
        let mut worker_addr = None;
        for _ in 0..50 {
            worker_addr = worker_net.listen_addrs().await.unwrap().into_iter().next();
            if worker_addr.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let worker_addr = worker_addr.expect("worker listen address");
        let worker_dial_addr = format!("{worker_addr}/p2p/{worker_peer}");
        client_net.dial_peer(&worker_dial_addr).await.unwrap();

        let worker_registry = registry_with_model("qwen3-mini").await;
        let client_pubkey = {
            let manifest =
                inference_manifest(&client_identity, &loaded_test_model_cid(), Some(512));
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
        );
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
}
