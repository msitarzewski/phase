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
//! 4. Otherwise: DHT lookup; first authorized peer → `Peer { peer_id }`.
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

use std::collections::{HashMap, HashSet};
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
    JobResult, JobSpec, JobStream, ProgressUpdate, SignedManifest, SignedReceipt, WorkerError,
};
use sha2::{Digest, Sha256};
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

use crate::consumer::ConsumerControl;
use crate::policy::{PauseReason, PolicyDecision, PolicyEngine};
use crate::registry::{normalize_model_alias, ModelCapabilities, ModelCid, ModelRegistry};
use crate::reputation::{
    compare_assessments, AssessmentClass, EvidenceContext, EvidenceOutcome, EvidenceRuntime,
    SpotCheckEvidence, SPOT_CHECK_METHOD,
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
const SPOT_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SPOT_CHECK_TOKENS: u32 = 1_024;
const MAX_SPOT_CHECK_INPUT_BYTES: usize = 16 * 1_024;
const MAX_SPOT_CHECK_OUTPUT_BYTES: usize = 64 * 1_024;
const MAX_SPOT_CHECK_OUTPUT_CHUNKS: u64 = 4_096;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Where this request will be served.
#[derive(Debug, Clone)]
pub enum RouteVia {
    /// Dispatch to the local worker.
    Local,
    /// Dispatch to the local coordinator offloading layers to Tier-1
    /// trusted-circle RPC peers (`--llama-rpc-servers`).
    TrustedCircleSharded,
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

type RoutedExecution = (JobHandle, JobStream, ReceiptVerification);
pub(crate) type SpotCheckResult = oneshot::Receiver<RedundantCheckResult>;

#[derive(Clone, Copy)]
struct SpotCheckPair {
    primary: PeerId,
    validator: PeerId,
}

#[derive(Clone, Copy)]
struct SpotCheckOutput {
    commitment: [u8; 32],
    text_digest: [u8; 32],
}

struct SpotCheckComparison {
    primary: SpotCheckOutput,
    validator: SpotCheckOutput,
    capability_digest: [u8; 32],
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
            RouteVia::TrustedCircleSharded => Some(
                crate::shard_plan::ShardTrustTier::TrustedCircle
                    .trust_label()
                    .to_string(),
            ),
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
    consumer: Option<Arc<ConsumerControl>>,
    allow_open_peers: bool,
    local_worker_uses_rpc_offload: bool,
    redundant_config: RedundantVerificationConfig,
    redundant_gate: Arc<Semaphore>,
    /// Live count of active remote jobs dispatched to each peer, used to spread
    /// concurrent requests across equally-ranked healthy peers.
    in_flight_by_peer: Arc<std::sync::Mutex<std::collections::HashMap<PeerId, usize>>>,
}

struct InFlightPeerGuard {
    peer_id: PeerId,
    counts: Arc<std::sync::Mutex<std::collections::HashMap<PeerId, usize>>>,
}

pub(crate) fn find_local_model_by_alias<'a>(
    models: &'a [ModelCapabilities],
    alias: &str,
) -> Option<&'a ModelCapabilities> {
    models
        .iter()
        .find(|caps| caps.model_id == alias)
        .or_else(|| {
            if let Some(bare) = alias.strip_suffix(":latest") {
                models.iter().find(|caps| caps.model_id == bare)
            } else if !alias.contains(':') {
                models
                    .iter()
                    .find(|caps| caps.model_id.strip_suffix(":latest") == Some(alias))
            } else {
                None
            }
        })
}

impl Drop for InFlightPeerGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.counts.lock() {
            if let Some(entry) = map.get_mut(&self.peer_id) {
                *entry = entry.saturating_sub(1);
                if *entry == 0 {
                    map.remove(&self.peer_id);
                }
            }
        }
    }
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
            consumer: None,
            allow_open_peers: false,
            local_worker_uses_rpc_offload: false,
            redundant_config: RedundantVerificationConfig::default(),
            redundant_gate: Arc::new(Semaphore::new(1)),
            in_flight_by_peer: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Attach live enrollment grants and the operator's open-network opt-in.
    pub fn with_remote_access(
        mut self,
        consumer: Option<Arc<ConsumerControl>>,
        allow_open_peers: bool,
    ) -> Self {
        self.consumer = consumer;
        self.allow_open_peers = allow_open_peers;
        self
    }

    /// Mark the local worker as using Tier-1 RPC layer offload (`--llama-rpc-servers`).
    /// When enabled, `X-Lucid-Local-Only` requests are refused (because activations
    /// leave this host for RPC peers) and local dispatch reports
    /// `"trusted-circle-sharded"` instead of `"local"`.
    pub fn with_local_rpc_offload(mut self, uses_rpc_offload: bool) -> Self {
        self.local_worker_uses_rpc_offload = uses_rpc_offload;
        self
    }

    pub fn local_worker_uses_rpc_offload(&self) -> bool {
        self.local_worker_uses_rpc_offload
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
        self.route_with_cid(model_id, local_only).await.0
    }

    pub(crate) async fn route_with_cid(
        &self,
        model_id: &str,
        local_only: bool,
    ) -> (RouteDecision, Option<ModelCid>) {
        let has_local_worker = self.local_worker.is_some();
        let local_models = self.registry.local_models_async().await;
        let requested_cid = ModelCid::from_hex(model_id).ok();
        let normalized_alias = normalize_model_alias(model_id).ok();
        let local_model = match requested_cid {
            Some(cid) => local_models.iter().find(|caps| caps.model_cid == cid),
            None => normalized_alias
                .as_deref()
                .and_then(|alias| find_local_model_by_alias(&local_models, alias)),
        };
        let local_has_model = local_model.is_some();
        let policy_model_id = local_model
            .map(|caps| caps.model_id.as_str())
            .or(normalized_alias.as_deref())
            .unwrap_or(model_id);

        // 1. Local-only privacy posture wins over everything else.
        //    A local worker using Tier-1 RPC layer offload ships prompt-derived
        //    activations to trusted-circle peers, so it does not satisfy
        //    `X-Lucid-Local-Only`.
        if local_only
            && !(has_local_worker && local_has_model && !self.local_worker_uses_rpc_offload)
        {
            let reason = if has_local_worker
                && local_has_model
                && self.local_worker_uses_rpc_offload
            {
                format!(
                    "local-only requested but model '{model_id}' runs on trusted-circle RPC layer offload"
                )
            } else {
                format!("local-only requested but model '{model_id}' not loaded locally")
            };
            return (
                RouteDecision {
                    via: RouteVia::Refused { reason },
                    model_id: model_id.to_string(),
                    fallback_peers: Vec::new(),
                    explanation: "local-only privacy policy refused non-local routing".to_string(),
                },
                None,
            );
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
                return (
                    RouteDecision {
                        via: RouteVia::Refused {
                            reason: pause_reason_string(&reason),
                        },
                        model_id: model_id.to_string(),
                        fallback_peers: Vec::new(),
                        explanation:
                            "operator policy refused this request before candidate selection"
                                .to_string(),
                    },
                    None,
                );
            }
        }

        // 3. Local has the model — fast path (labelled honestly if RPC-offloaded).
        if let Some(local_model) = local_model.filter(|_| has_local_worker) {
            let (via, explanation) = if self.local_worker_uses_rpc_offload {
                (
                    RouteVia::TrustedCircleSharded,
                    "local coordinator offloads layers to trusted-circle RPC peers; remote reputation was not consulted"
                        .to_string(),
                )
            } else {
                (
                    RouteVia::Local,
                    "local worker is loaded; remote reputation was not consulted".to_string(),
                )
            };
            return (
                RouteDecision {
                    via,
                    model_id: model_id.to_string(),
                    fallback_peers: Vec::new(),
                    explanation,
                },
                Some(local_model.model_cid),
            );
        }

        // 4. Preserve Ollama's bare alias lookup after checking both local names.
        let remote_alias = normalized_alias.as_deref().unwrap_or(model_id);
        let remote_alias = remote_alias.strip_suffix(":latest").unwrap_or(remote_alias);
        let peers = match self.registry.find_peers_by_model_id(remote_alias).await {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "registry lookup failed");
                Vec::new()
            }
        };
        let authorized_issuers = self.authorized_issuers();
        let advertised = peers.len();
        let peers: Vec<_> = peers
            .into_iter()
            .filter(|(peer, _)| {
                self.remote_peer_allowed(peer, &authorized_issuers, self.allow_open_peers)
            })
            .collect();
        let filtered = advertised > peers.len();
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
            return (
                RouteDecision {
                    via: RouteVia::Peer { peer_id },
                    model_id: model_id.to_string(),
                    fallback_peers,
                    explanation: ranking_explanation,
                },
                Some(caps.model_cid),
            );
        }

        // 5. Nobody can serve.
        (
            RouteDecision {
                via: RouteVia::Refused {
                    reason: if filtered {
                        format!("no authorized peers serving model '{model_id}'")
                    } else {
                        format!("no peers serving model '{model_id}'")
                    },
                },
                model_id: model_id.to_string(),
                fallback_peers: Vec::new(),
                explanation: ranking_explanation,
            },
            None,
        )
    }

    fn authorized_issuers(&self) -> HashSet<PeerId> {
        self.consumer
            .iter()
            .flat_map(|consumer| consumer.authorized_issuer_peers())
            .collect()
    }

    fn remote_peer_allowed(
        &self,
        peer: &PeerId,
        authorized_issuers: &HashSet<PeerId>,
        allow_open_peers: bool,
    ) -> bool {
        let operator = match &self.evidence {
            Some(runtime) => match runtime.operator_override(peer) {
                Ok(operator) => operator,
                Err(error) => {
                    tracing::error!(%error, "could not read peer routing overrides");
                    return false;
                }
            },
            None => Default::default(),
        };
        !operator.blocked
            && (operator.pinned || allow_open_peers || authorized_issuers.contains(peer))
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
        let in_flight_snapshot = self
            .in_flight_by_peer
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        allowed.sort_by(|left, right| {
            let left_load = in_flight_snapshot.get(&left.0 .0).copied().unwrap_or(0);
            let right_load = in_flight_snapshot.get(&right.0 .0).copied().unwrap_or(0);
            // Spread concurrent requests across peers within the same healthy
            // assessment class when one peer already has active in-flight work.
            // When all peers are idle (0 in-flight), ordering is strictly
            // identical to `compare_assessments`.
            if left.1.class == right.1.class && left_load != right_load {
                return left_load.cmp(&right_load);
            }
            compare_assessments(&left.1, &right.1)
        });

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
    ) -> Result<RoutedExecution, RouterError> {
        let (execution, _) = self.execute_with_spot_check(decision, job).await?;
        Ok(execution)
    }

    pub(crate) async fn execute_with_spot_check(
        &self,
        decision: &RouteDecision,
        job: SignedManifest<JobSpec>,
    ) -> Result<(RoutedExecution, Option<SpotCheckResult>), RouterError> {
        match &decision.via {
            RouteVia::Refused { reason } => Err(RouterError::Refused {
                reason: reason.clone(),
            }),
            RouteVia::Local | RouteVia::TrustedCircleSharded => {
                let worker = self
                    .local_worker
                    .as_ref()
                    .ok_or(RouterError::NoLocalWorker)?
                    .clone();
                let (handle, stream) = worker.execute_boxed(job).await?;
                Ok(((handle, stream, ReceiptVerification::Local), None))
            }
            // Multi-peer failover: try the primary peer first; if its relay
            // fails, walk `decision.fallback_peers` in ranking order. The
            // ranking comes from the registry (route step 4). `SignedManifest`
            // is Clone, so each attempt gets its own copy of the job.
            RouteVia::Peer { peer_id } => {
                match self.execute_via_peer_tracked(*peer_id, job.clone()).await {
                    Ok(execution) => Ok(self.attach_redundant_probe(
                        *peer_id,
                        decision.fallback_peers.first().copied(),
                        job,
                        execution,
                    )),
                    Err(first_err) => {
                        for (index, fb) in decision.fallback_peers.iter().enumerate() {
                            warn!(failed_peer = %peer_id, next = %fb, "relay failed; failing over to next peer");
                            match self.execute_via_peer_tracked(*fb, job.clone()).await {
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

    async fn execute_via_peer_tracked(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
        {
            if let Ok(mut map) = self.in_flight_by_peer.lock() {
                *map.entry(peer_id).or_insert(0) += 1;
            }
        }
        let guard = InFlightPeerGuard {
            peer_id,
            counts: Arc::clone(&self.in_flight_by_peer),
        };
        let (handle, mut inner_stream, verification) = self.execute_via_peer(peer_id, job).await?;
        let tracked_stream: JobStream = Box::pin(stream! {
            let _guard = guard;
            while let Some(event) = futures::StreamExt::next(&mut inner_stream).await {
                yield event;
            }
        });
        Ok((handle, tracked_stream, verification))
    }

    fn attach_redundant_probe(
        &self,
        primary_peer: PeerId,
        duplicate_peer: Option<PeerId>,
        job: SignedManifest<JobSpec>,
        execution: RoutedExecution,
    ) -> (RoutedExecution, Option<SpotCheckResult>) {
        let Some(duplicate_peer) = duplicate_peer else {
            return (execution, None);
        };
        let Some(permit) = self.try_begin_redundant_probe(&job, primary_peer, duplicate_peer)
        else {
            return (execution, None);
        };

        let (handle, mut primary_stream, verification) = execution;
        if !matches!(
            verification,
            ReceiptVerification::Verified | ReceiptVerification::Pending
        ) {
            return ((handle, primary_stream, verification), None);
        }
        let pair = SpotCheckPair {
            primary: primary_peer,
            validator: duplicate_peer,
        };
        let (primary_tx, primary_rx) = oneshot::channel();
        let (verdict_tx, verdict_rx) = oneshot::channel();
        let observed_primary: JobStream = Box::pin(stream! {
            let mut transcript = SpotCheckTranscript::default();
            while let Some(event) = futures::StreamExt::next(&mut primary_stream).await {
                transcript.observe(&event);
                yield event;
            }
            // The verified live and batch relays only deliver a successful
            // terminal after verifying the signed receipt for this exact peer.
            let _ = primary_tx.send(transcript.finish());
        });

        let router = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let comparison = timeout(SPOT_CHECK_TIMEOUT, async {
                let (primary, (validator, capability_digest)) =
                    tokio::try_join!(async { primary_rx.await.ok().flatten().ok_or(()) }, async {
                        let capability_digest =
                            router.spot_check_capabilities(pair, &job).await.ok_or(())?;
                        let validator = router
                            .spot_check_peer_output(pair.validator, &job)
                            .await
                            .ok_or(())?;
                        Ok((validator, capability_digest))
                    })
                    .ok()?;
                Some(SpotCheckComparison {
                    primary,
                    validator,
                    capability_digest,
                })
            })
            .await
            .ok()
            .flatten();
            let result = router.record_spot_check(pair, &job, comparison).await;
            let _ = verdict_tx.send(result);
        });

        ((handle, observed_primary, verification), Some(verdict_rx))
    }

    /// Compare only peers whose current signed advertisements attest the same
    /// backend and quantization for the exact immutable model CID. The
    /// registry has already verified each advertisement's signature and
    /// signer→PeerId binding. Missing, expired, or semantically different
    /// capabilities are incomparable and must never generate negative
    /// reputation evidence from output disagreement.
    async fn spot_check_capabilities(
        &self,
        pair: SpotCheckPair,
        job: &SignedManifest<JobSpec>,
    ) -> Option<[u8; 32]> {
        let JobSpec::Inference(spec) = &job.payload else {
            return None;
        };
        let model_cid = ModelCid::from_hex(&spec.model_cid).ok()?;
        let peers = self.registry.find_peers_for_model(&model_cid).await.ok()?;
        let primary = peers
            .iter()
            .find_map(|(peer, caps)| (*peer == pair.primary).then_some(caps))?;
        let duplicate = peers
            .iter()
            .find_map(|(peer, caps)| (*peer == pair.validator).then_some(caps))?;
        if !capabilities_are_redundancy_equivalent(primary, duplicate) {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(b"lucid-spot-check-capabilities:v1\0");
        hasher.update(primary.backend.as_bytes());
        hasher.update([0]);
        hasher.update(primary.quantization.as_bytes());
        hasher.update(primary.context_length.to_be_bytes());
        Some(hasher.finalize().into())
    }

    fn try_begin_redundant_probe(
        &self,
        job: &SignedManifest<JobSpec>,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
    ) -> Option<OwnedSemaphorePermit> {
        if !self.redundant_config.enabled
            || self.evidence.is_none()
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
        if !self.spot_check_peers_allowed(SpotCheckPair {
            primary: primary_peer,
            validator: duplicate_peer,
        }) {
            return None;
        }
        Arc::clone(&self.redundant_gate).try_acquire_owned().ok()
    }

    fn spot_check_peers_allowed(&self, pair: SpotCheckPair) -> bool {
        let observer = node_identity_peer_id(&self.identity);
        if pair.primary == pair.validator || pair.primary == observer || pair.validator == observer
        {
            return false;
        }
        let issuers = self.authorized_issuers();
        self.remote_peer_allowed(&pair.primary, &issuers, false)
            && self.remote_peer_allowed(&pair.validator, &issuers, false)
    }

    async fn spot_check_peer_output(
        &self,
        peer: PeerId,
        job: &SignedManifest<JobSpec>,
    ) -> Option<SpotCheckOutput> {
        if !self.remote_peer_allowed(&peer, &self.authorized_issuers(), false) {
            return None;
        }
        // The live relay carries the check deadline to the server and cancels
        // when dropped. A batch fallback cannot cancel in-flight work.
        let (handle, mut stream, verification) = self
            .execute_via_peer_live(peer, job.clone(), SPOT_CHECK_TIMEOUT)
            .await
            .ok()?;
        if !matches!(
            verification,
            ReceiptVerification::Verified | ReceiptVerification::Pending
        ) {
            return None;
        }
        let mut transcript = SpotCheckTranscript::default();
        while let Some(event) = futures::StreamExt::next(&mut stream).await {
            transcript.observe(&event);
            if transcript.invalid {
                handle.cancel();
                return None;
            }
        }
        let output = transcript.finish()?;
        let receipt = handle.finish().await.ok()?;
        (receipt.result.output_commitment == output.commitment).then_some(output)
    }

    /// Prefer the real v2 bidirectional stream. A v1 batch fallback is
    /// permitted only when protocol negotiation/default-handler refusal proves
    /// the peer has not enabled v2, and only before any output was delivered.
    async fn execute_via_peer(
        &self,
        peer_id: PeerId,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream, ReceiptVerification), RouterError> {
        if !self.remote_peer_allowed(&peer_id, &self.authorized_issuers(), self.allow_open_peers) {
            return Err(RouterError::Refused {
                reason: "serving peer is no longer authorized for remote requests".to_string(),
            });
        }
        match self
            .execute_via_peer_live(peer_id, job.clone(), RELAY_TIMEOUT)
            .await
        {
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
        job_timeout: Duration,
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
        let deadline_unix_ms = unix_time_ms().saturating_add(job_timeout.as_millis() as u64);
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

    /// Run a bounded check without exposing either execution's output to a caller.
    pub async fn check_redundant_execution(
        &self,
        job: SignedManifest<JobSpec>,
        primary_peer: PeerId,
        duplicate_peer: PeerId,
        _eligibility: DeterministicVerificationEligibility,
    ) -> Result<RedundantCheckResult, RouterError> {
        if !self.redundant_config.enabled || self.evidence.is_none() {
            return Ok(RedundantCheckResult::SkippedDisabled);
        }
        let pair = SpotCheckPair {
            primary: primary_peer,
            validator: duplicate_peer,
        };
        if !self.spot_check_peers_allowed(pair) {
            return Err(RouterError::Refused {
                reason: "spot checks require two distinct authorized remote peers".to_string(),
            });
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

        let comparison = timeout(SPOT_CHECK_TIMEOUT, async {
            let capability_digest = self.spot_check_capabilities(pair, &job).await?;
            let job = &job;
            let checked_output =
                |peer| async move { self.spot_check_peer_output(peer, job).await.ok_or(()) };
            let (primary, validator) =
                tokio::try_join!(checked_output(pair.primary), checked_output(pair.validator))
                    .ok()?;
            Some(SpotCheckComparison {
                primary,
                validator,
                capability_digest,
            })
        })
        .await
        .ok()
        .flatten();
        Ok(self.record_spot_check(pair, &job, comparison).await)
    }

    async fn record_spot_check(
        &self,
        pair: SpotCheckPair,
        job: &SignedManifest<JobSpec>,
        comparison: Option<SpotCheckComparison>,
    ) -> RedundantCheckResult {
        let Some(evidence) = &self.evidence else {
            return RedundantCheckResult::Incomparable;
        };
        let Some(comparison) = comparison else {
            self.record_remote_outcome(
                pair.primary,
                job,
                EVIDENCE_PROTOCOL_REDUNDANT,
                EvidenceOutcome::RedundantExecutionIncomparableResult,
                None,
            )
            .await;
            return RedundantCheckResult::Incomparable;
        };
        let Ok(context) = evidence_context(
            evidence.observer_peer_id(),
            pair.primary,
            job,
            SPOT_CHECK_METHOD,
        ) else {
            return RedundantCheckResult::Incomparable;
        };
        let (outcome, result) = compare_spot_check(pair.validator, &comparison);
        match evidence
            .record(context, outcome, Some(comparison.primary.commitment))
            .await
        {
            Ok(_) => result,
            Err(error) => {
                warn!(%error, "could not persist verified spot-check evidence");
                RedundantCheckResult::Incomparable
            }
        }
    }
}

fn unix_time_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

fn validate_redundant_job_eligibility(job: &JobSpec) -> Result<(), RouterError> {
    match job {
        JobSpec::Inference(spec)
            if spec.resume_from.is_none()
                && matches!(spec.max_tokens, Some(1..=MAX_SPOT_CHECK_TOKENS))
                && spec.sampling.params.get("temperature").and_then(|value| value.parse::<f64>().ok()) == Some(0.0)
                && spec
                    .sampling
                    .params
                    .get("seed")
                    .is_some_and(|seed| seed.parse::<u32>().is_ok())
                && ModelCid::from_hex(&spec.model_cid).is_ok()
                && spec.messages.iter().all(|message| message.images.is_empty())
                && spec.messages.iter().fold(
                    spec.prompt.as_ref().map_or(0, String::len),
                    |bytes, message| bytes.saturating_add(message.content.len())
                ) <= MAX_SPOT_CHECK_INPUT_BYTES =>
        {
            Ok(())
        }
        JobSpec::Inference(_) => Err(RouterError::Relay(
            "spot checks require bounded, non-resumed text inference with an exact model CID, a fixed seed, and zero temperature".to_string(),
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
    let draw = u64::from_be_bytes(keyed_sample[..8].try_into().expect("signature is 64 bytes"));
    let threshold = (u128::from(u64::MAX) + 1) * u128::from(sample_cap_permille) / 1_000;
    u128::from(draw) < threshold
}

/// Hash canonical text bytes without storing text or depending on chunk boundaries.
#[derive(Default)]
struct SpotCheckTranscript {
    digest: Sha256,
    bytes: usize,
    chunks: u64,
    terminal: Option<[u8; 32]>,
    invalid: bool,
}

impl SpotCheckTranscript {
    fn observe(&mut self, event: &JobEvent) {
        if self.invalid || self.terminal.is_some() {
            self.invalid = true;
            return;
        }
        match event {
            JobEvent::Output(chunk)
                if chunk.kind == "token"
                    && chunk.seq == self.chunks
                    && std::str::from_utf8(&chunk.data).is_ok() =>
            {
                self.bytes = self.bytes.saturating_add(chunk.data.len());
                self.chunks += 1;
                if self.bytes > MAX_SPOT_CHECK_OUTPUT_BYTES
                    || self.chunks > MAX_SPOT_CHECK_OUTPUT_CHUNKS
                {
                    self.invalid = true;
                } else {
                    self.digest.update(&chunk.data);
                }
            }
            JobEvent::Progress(_) => {}
            JobEvent::Final { result, error }
                if error.is_none()
                    && matches!(result.completion, Completion::Stop | Completion::Length)
                    && result.output_chunk_count == self.chunks
                    && self.bytes > 0 =>
            {
                self.terminal = Some(result.output_commitment);
            }
            _ => self.invalid = true,
        }
    }

    fn finish(self) -> Option<SpotCheckOutput> {
        if self.invalid {
            return None;
        }
        Some(SpotCheckOutput {
            commitment: self.terminal?,
            text_digest: self.digest.finalize().into(),
        })
    }
}

fn compare_spot_check(
    validator_peer: PeerId,
    comparison: &SpotCheckComparison,
) -> (EvidenceOutcome, RedundantCheckResult) {
    let evidence = SpotCheckEvidence {
        validator_peer,
        validator_commitment: comparison.validator.commitment,
        primary_text_digest: comparison.primary.text_digest,
        validator_text_digest: comparison.validator.text_digest,
        capability_digest: comparison.capability_digest,
    };
    if evidence.primary_text_digest == evidence.validator_text_digest {
        (
            EvidenceOutcome::VerifiedSpotCheckAgreement(evidence),
            RedundantCheckResult::Agreement {
                commitment: comparison.primary.commitment,
            },
        )
    } else {
        (
            EvidenceOutcome::VerifiedSpotCheckDisagreement(evidence),
            RedundantCheckResult::Disagreement {
                primary_commitment: comparison.primary.commitment,
                duplicate_commitment: comparison.validator.commitment,
            },
        )
    }
}

fn capabilities_are_redundancy_equivalent(
    primary: &ModelCapabilities,
    duplicate: &ModelCapabilities,
) -> bool {
    primary.model_cid == duplicate.model_cid
        && primary.backend == duplicate.backend
        && primary.quantization == duplicate.quantization
        && primary.context_length == duplicate.context_length
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

async fn prepare_inbound_relay(
    delivering_peer: PeerId,
    bytes: Vec<u8>,
    expected_job_id: Option<[u8; 32]>,
    context: &InboundRelayContext,
) -> Result<(RemoteExecutionPermit, SignedManifest<JobSpec>), String> {
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

    Ok((permit, job))
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
                prepare_inbound_relay(delivering_peer, open.payload, Some(open.job_id), &context),
            )
            .await
            {
                Ok(execution) => execution,
                Err(_) => Err("remote job deadline reached before acceptance".to_string()),
            };
            let (_permit, job) = match execution {
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

            let frame = |sequence, kind| JobRelayStreamFrame {
                schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
                job_id: open.job_id,
                sequence,
                kind,
            };
            // Keep the usual rejection behavior for fast dispatch failures.
            // If cold loading takes longer, accept once and then send progress
            // events until dispatch returns. The absolute deadline still applies.
            let interval = Duration::from_millis((open.idle_timeout_ms / 2).max(1));
            let mut loading = tokio::time::interval_at(TokioInstant::now() + interval, interval);
            loading.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let dispatch = timeout_at(deadline, context.worker.execute_boxed(job));
            tokio::pin!(dispatch);
            let mut sequence = 0;
            let (handle, mut stream) = loop {
                let kind = tokio::select! {
                    biased;
                    _ = controls.recv() => return,
                    _ = frames.closed() => return,
                    result = &mut dispatch => {
                        let reason = match result {
                            Ok(Ok(execution)) => break execution,
                            Ok(Err(error)) => bounded_reason(&format!("local worker dispatch failed: {error}")),
                            Err(_) => "remote job deadline reached during dispatch".to_string(),
                        };
                        let kind = if sequence == 0 {
                            JobRelayStreamFrameKind::Rejected { reason }
                        } else {
                            JobRelayStreamFrameKind::Failed { reason }
                        };
                        let _ = send_live_handler_frame_before(&frames, frame(sequence, kind), deadline).await;
                        return;
                    }
                    _ = loading.tick() => {
                        if sequence == 0 {
                            JobRelayStreamFrameKind::Accepted
                        } else {
                            let progress = JobEvent::Progress(ProgressUpdate {
                                kind: "model_load".to_string(),
                                fraction: None,
                                message: None,
                            });
                            let Ok(payload) = serde_json::to_vec(&progress) else {
                                return;
                            };
                            JobRelayStreamFrameKind::Event { payload, terminal: false }
                        }
                    }
                };
                if !send_live_handler_frame_before(&frames, frame(sequence, kind), deadline).await {
                    return;
                }
                sequence += 1;
            };
            if sequence == 0 {
                if !send_live_handler_frame_before(
                    &frames,
                    frame(sequence, JobRelayStreamFrameKind::Accepted),
                    deadline,
                )
                .await
                {
                    handle.cancel();
                    return;
                }
                sequence = 1;
            }

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
            let execution = timeout_at(deadline, async {
                let (permit, job) =
                    prepare_inbound_relay(delivering_peer, bytes, None, &context).await?;
                let (handle, stream) = context
                    .worker
                    .execute_boxed(job)
                    .await
                    .map_err(|error| format!("local worker dispatch failed: {error}"))?;
                Ok((permit, handle, stream))
            })
            .await;
            let execution = match execution {
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
mod tests;
