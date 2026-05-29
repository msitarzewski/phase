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
    CommitmentAccumulator, DynWorker, JobEvent, JobHandle, JobId, JobResult, JobSpec, JobStream,
    SignedManifest, SignedReceipt, WorkerError,
};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// SEC-06: hard ceiling on the total prompt/message character length a relay
/// peer may submit, enforced in the authz/policy gate *before* dispatch. A
/// hostile peer can otherwise ship a multi-megabyte prompt to exhaust GPU
/// context memory even with `max_tokens` clamped. 256 KiB of text is far
/// beyond any legitimate chat turn while staying under the 256 KiB relay
/// request frame cap (SEC-06, discovery.rs).
const MAX_PROMPT_CHARS: usize = 256 * 1024;

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

/// SEC-05: receipt verification status for a dispatched job, surfaced to the
/// HTTP layer so it can set `X-Lucid-Receipt-Verified`.
///
/// The local path is `Local` (the worker is us — no peer receipt to bind).
/// The peer path is `Verified` when the worker's `SignedReceipt` passed every
/// check (signature, job_id bind, worker-pubkey→PeerId bind, commitment
/// replay), `Failed` when a check did not hold (logged; tokens still returned
/// per the v0.1 "friend's GPU" trust posture), or `Unverifiable` when a v1
/// peer shipped no receipt at all.
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

        // SEC-05: verify + bind the worker's signed receipt against the job
        // we dispatched and the peer we dispatched it to, and re-check the
        // output commitment over the received chunks. v0.1 trust posture is
        // "friend's GPU": we don't fail the user's tokens on a mismatch, but
        // we surface the verdict so the HTTP layer can flag it and we log
        // loudly. v0.2 with reputation can harden this to a hard reject.
        let verification = verify_peer_receipt(&receipt_bytes, &events, manifest_hash, peer_id);

        // Synthesize the handle/stream pair. SEC-05: if the peer shipped a
        // receipt, deliver it through the handle so the Ollama layer's
        // `handle.finish()` resolves with the real `SignedReceipt` (matching
        // the local path) instead of `WorkerError::Dropped`.
        let (handle, mut producer) = JobHandle::new(job_id);
        if !receipt_bytes.is_empty() {
            if let Ok(receipt) =
                serde_json::from_slice::<SignedReceipt<JobResult>>(&receipt_bytes)
            {
                producer.deliver_receipt(receipt);
            }
        }
        let stream: JobStream = Box::pin(stream! {
            // Keep the producer alive for the duration of the stream so the
            // delivered receipt remains available to `handle.finish()`.
            let _producer = producer;
            for ev in events {
                yield ev;
            }
        });
        Ok((handle, stream, verification))
    }
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

    let receipt: SignedReceipt<JobResult> = match serde_json::from_slice(receipt_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!(peer = %peer_id, error = %e, "relay: receipt failed to decode");
            return ReceiptVerification::Failed;
        }
    };

    // 1. Signature.
    if let Err(e) = receipt.verify() {
        warn!(peer = %peer_id, error = %e, "relay: receipt signature verification FAILED");
        return ReceiptVerification::Failed;
    }

    // 2. job_id bind: the receipt must be for the job we actually dispatched.
    match receipt.job_id_bytes() {
        Some(jid) if jid == manifest_hash => {}
        Some(_) => {
            warn!(
                peer = %peer_id,
                expected = %JobId(manifest_hash),
                got = %receipt.job_id,
                "relay: receipt job_id does NOT match dispatched manifest hash"
            );
            return ReceiptVerification::Failed;
        }
        None => {
            warn!(peer = %peer_id, "relay: receipt job_id is malformed hex");
            return ReceiptVerification::Failed;
        }
    }

    // 3. worker-pubkey → PeerId bind.
    match worker_pubkey_to_peer_id(&receipt.worker_pubkey) {
        Some(derived) if derived == peer_id => {}
        Some(derived) => {
            warn!(
                peer = %peer_id,
                derived = %derived,
                "relay: receipt worker_pubkey derives to a DIFFERENT PeerId than dispatched"
            );
            return ReceiptVerification::Failed;
        }
        None => {
            warn!(peer = %peer_id, "relay: receipt worker_pubkey is not a valid Ed25519 key");
            return ReceiptVerification::Failed;
        }
    }

    // 4. Commitment replay over the received chunks.
    let mut acc = CommitmentAccumulator::new();
    let mut final_result: Option<&JobResult> = None;
    for ev in events {
        match ev {
            JobEvent::Output(chunk) => acc.update(chunk),
            JobEvent::Final { result, .. } => final_result = Some(result),
            _ => {}
        }
    }
    let (replayed_commitment, replayed_count) = acc.finalize();
    let Some(result) = final_result else {
        warn!(peer = %peer_id, "relay: event batch carried no Final result to bind commitment");
        return ReceiptVerification::Failed;
    };
    if replayed_commitment != result.output_commitment || replayed_count != result.output_chunk_count
    {
        warn!(
            peer = %peer_id,
            replayed_count,
            signed_count = result.output_chunk_count,
            "relay: recomputed output commitment does NOT match the signed receipt"
        );
        return ReceiptVerification::Failed;
    }

    debug!(peer = %peer_id, job = %JobId(manifest_hash), "relay: receipt verified + bound");
    ReceiptVerification::Verified
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
    // SEC-06: concurrency cap. Sized to the operator's
    // `max_concurrent_remote_jobs` at construction time. A permit is held for
    // the full dispatch+drain; the (N+1)th concurrent relay gets a "busy"
    // refusal instead of spinning up another GPU-heavy job. We read the live
    // config once here — a reload changing the ceiling takes effect on the
    // next handler rebuild, which is acceptable for a DoS backstop.
    let max_concurrent = policy.config().max_concurrent_remote_jobs.max(1) as usize;
    let semaphore = Arc::new(Semaphore::new(max_concurrent));

    Arc::new(move |delivering_peer: PeerId, bytes: Vec<u8>| {
        let worker = worker.clone();
        let registry = registry.clone();
        let policy = policy.clone();
        let semaphore = semaphore.clone();
        Box::pin(async move {
            // SEC-06: acquire a concurrency permit FIRST and cheaply. If the
            // node is already serving `max_concurrent_remote_jobs`, refuse
            // (busy) before doing any decode/verify/dispatch work. `try_acquire`
            // is non-blocking — we fail fast rather than queue unboundedly.
            let _permit = match semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    warn!("relay: refusing job — concurrency limit reached (busy)");
                    return JobRelayResponse::Err {
                        reason: "busy: max concurrent remote jobs reached".to_string(),
                    };
                }
            };

            // 1. Decode. JSON (matches the request-side encoding); see the
            //    note on the requesting side about why not bincode.
            let mut job: SignedManifest<JobSpec> = match serde_json::from_slice(&bytes) {
                Ok(j) => j,
                Err(e) => {
                    return JobRelayResponse::Err {
                        reason: format!("decode SignedManifest: {e}"),
                    };
                }
            };

            // 1a. SEC-01: VERIFY the signature before trusting anything in
            //     the manifest. The pre-SEC-01 code dispatched without ever
            //     calling verify(), so any malformed/forged envelope reached
            //     the worker. verify() proves "some keyholder signed this".
            if let Err(e) = job.verify() {
                warn!(error = %e, "relay: rejecting manifest that failed verify()");
                return JobRelayResponse::Err {
                    reason: format!("manifest verification failed: {e}"),
                };
            }

            // 1b. SEC-01 + SEC-06: AUTHORIZATION gate. verify() only proves
            //     *some* keyholder signed it — not an *authorized* one. A
            //     manifest is authorized if EITHER:
            //       (a) its signer pubkey is in the operator allowlist (or the
            //           insecure `allow_unauthenticated_jobs` escape hatch), OR
            //       (b) SEC-06 PeerID-bind: the signer's Ed25519 key derives to
            //           the libp2p PeerId that actually delivered this request.
            //     (b) makes the SEC-01 hook real: a peer signing with the same
            //     identity it dials from is implicitly trusted to spend its own
            //     work, without the operator pre-listing every key.
            let allowlisted = policy.is_authorized_submitter(&job.signer_pubkey);
            let peer_bound = signer_matches_peer(&job.signer_pubkey, delivering_peer);
            if !allowlisted && !peer_bound {
                warn!(
                    signer = %job.signer_pubkey,
                    peer = %delivering_peer,
                    "relay: rejecting job — signer neither allowlisted nor bound to delivering PeerId"
                );
                return JobRelayResponse::Err {
                    reason: "submitter not authorized".to_string(),
                };
            }
            if peer_bound && !allowlisted {
                debug!(
                    peer = %delivering_peer,
                    "relay: authorized via SEC-06 PeerID-bind (signer == delivering peer)"
                );
            }

            // 1c. SEC-01: clamp manifest-supplied resource limits to operator
            //     maxima BEFORE dispatch, regardless of what the (untrusted)
            //     client requested.
            if let JobSpec::Inference(spec) = &mut job.payload {
                let clamped = policy.clamp_max_tokens(spec.max_tokens);
                if clamped != spec.max_tokens {
                    debug!(
                        requested = ?spec.max_tokens,
                        clamped = ?clamped,
                        "relay: clamped max_tokens to operator ceiling"
                    );
                    spec.max_tokens = clamped;
                }
            }

            // 1d. SEC-06: bound total prompt/message length BEFORE dispatch.
            //     `max_tokens` caps *output*; this caps *input* so a peer
            //     can't exhaust context memory with a giant prompt.
            if let JobSpec::Inference(spec) = &job.payload {
                let prompt_chars: usize = spec.prompt.as_ref().map(|p| p.len()).unwrap_or(0)
                    + spec.messages.iter().map(|m| m.content.len()).sum::<usize>();
                if prompt_chars > MAX_PROMPT_CHARS {
                    warn!(
                        prompt_chars,
                        max = MAX_PROMPT_CHARS,
                        "relay: rejecting job — prompt exceeds server-side length cap"
                    );
                    return JobRelayResponse::Err {
                        reason: format!(
                            "prompt too large: {prompt_chars} chars > {MAX_PROMPT_CHARS} cap"
                        ),
                    };
                }
            }

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
            // SEC-05: ship the worker's `SignedReceipt<JobResult>` back in the
            // relay response so the requesting side can verify + bind it. The
            // commitment also rides inside `JobEvent::Final`, but only the
            // signed receipt proves *which worker* produced *which job*.
            let receipt_bytes = match handle.finish().await {
                Ok(receipt) => match serde_json::to_vec(&receipt) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(error = %e, "relay: failed to encode receipt; returning unverifiable batch");
                        Vec::new()
                    }
                },
                Err(e) => {
                    // No receipt available (worker dropped). Return the events
                    // anyway; the requester treats an empty receipt as
                    // unverifiable rather than a hard failure.
                    warn!(error = %e, "relay: worker produced no receipt");
                    Vec::new()
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
            &[phase_protocol::JobSpecKind::Inference]
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
            max_tokens,
            stream: true,
        });
        ManifestBuilder::new(spec).sign_with(client).unwrap()
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
        let manifest = inference_manifest(&attacker, "qwen3-mini", None);
        let bytes = serde_json::to_vec(&manifest).unwrap();

        // SEC-06: deliver from a random peer that does NOT match the signer,
        // so these SEC-01 tests still exercise the allowlist path (not the
        // PeerID-bind path).
        let resp = handler(PeerId::random(), bytes).await;
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
        let manifest = inference_manifest(&client, "qwen3-mini", None);
        // signer_pubkey is the canonical lowercase-hex the manifest carries.
        let config = PolicyConfig {
            authorized_submitters: vec![manifest.signer_pubkey.clone()],
            ..PolicyConfig::default()
        };
        let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
        let handler = make_inbound_relay_handler(worker, registry, policy);

        let bytes = serde_json::to_vec(&manifest).unwrap();

        // SEC-06: deliver from a random peer that does NOT match the signer,
        // so these SEC-01 tests still exercise the allowlist path (not the
        // PeerID-bind path).
        let resp = handler(PeerId::random(), bytes).await;
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
        let manifest = inference_manifest(&anyone, "qwen3-mini", None);
        let bytes = serde_json::to_vec(&manifest).unwrap();

        // SEC-06: deliver from a random peer that does NOT match the signer,
        // so these SEC-01 tests still exercise the allowlist path (not the
        // PeerID-bind path).
        let resp = handler(PeerId::random(), bytes).await;
        assert!(matches!(resp, JobRelayResponse::Ok { .. }), "got {resp:?}");
        assert_eq!(spy.call_count(), 1);
    }

    #[tokio::test]
    async fn sec01_relay_clamps_max_tokens_to_ceiling() {
        // A manifest claiming max_tokens = u32::MAX must be clamped to the
        // operator ceiling before the worker ever sees it.
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
        let manifest = inference_manifest(&client, "qwen3-mini", Some(u32::MAX));
        let config = PolicyConfig {
            authorized_submitters: vec![manifest.signer_pubkey.clone()],
            max_tokens_ceiling: ceiling,
            ..PolicyConfig::default()
        };
        let policy = Arc::new(PolicyEngine::new_for_tests(config, PolicyState::default()));
        let handler = make_inbound_relay_handler(worker, registry, policy);

        let bytes = serde_json::to_vec(&manifest).unwrap();
        // SEC-06: deliver from a random peer that does NOT match the signer,
        // so these SEC-01 tests still exercise the allowlist path (not the
        // PeerID-bind path).
        let resp = handler(PeerId::random(), bytes).await;
        assert!(matches!(resp, JobRelayResponse::Ok { .. }), "got {resp:?}");

        let seen = captured.lock().unwrap().expect("worker saw the job");
        assert_eq!(seen, ceiling, "max_tokens must be clamped to ceiling");
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

    // --- SEC-05: receipt verify + bind -----------------------------------

    /// Derive the libp2p PeerId an identity's Ed25519 key maps to — the same
    /// transform `worker_pubkey_to_peer_id` performs, used by tests to compute
    /// the "dispatched-to" PeerId for the bind check.
    fn peer_id_of(identity: &NodeIdentity) -> PeerId {
        use phase_net::libp2p_identity::{ed25519, PublicKey};
        let ed =
            ed25519::PublicKey::try_from_bytes(&identity.verifying_key().to_bytes()).unwrap();
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

        let manifest = inference_manifest(client, model_id, Some(8));
        let manifest_hash = manifest.manifest_hash().unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let resp = handler(PeerId::random(), bytes).await;
        match resp {
            JobRelayResponse::Ok { events, receipt } => (events, receipt, manifest_hash),
            other => panic!("expected Ok, got {other:?}"),
        }
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
        assert_eq!(v, ReceiptVerification::Verified, "honest round-trip must verify");
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
        assert_eq!(v, ReceiptVerification::Failed, "job_id mismatch must be detected");
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
        assert!(tampered, "round-trip should have produced at least one Output chunk");

        let v = verify_peer_receipt(&receipt_b, &events, manifest_hash, peer_id_of(&worker_id));
        assert_eq!(
            v,
            ReceiptVerification::Failed,
            "tampered output vs signed commitment must be detected"
        );
    }

    #[tokio::test]
    async fn sec05_missing_receipt_is_unverifiable() {
        // A pre-SEC-05 serving node ships no receipt → unverifiable, not a
        // hard failure (v0.1 trust posture).
        let v = verify_peer_receipt(&[], &[], [0u8; 32], PeerId::random());
        assert_eq!(v, ReceiptVerification::Unverifiable);
    }

    // --- SEC-06: DoS caps + PeerID-bind authz ----------------------------

    #[tokio::test]
    async fn sec06_peer_id_bind_authorizes_self_signed_peer() {
        // A peer that signs with the SAME identity it dials from is authorized
        // even with an EMPTY allowlist and `allow_unauthenticated_jobs=false`.
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

        let manifest = inference_manifest(&client, "qwen3-mini", None);
        let bytes = serde_json::to_vec(&manifest).unwrap();
        // Deliver from the client's OWN PeerId → PeerID-bind path accepts.
        let delivering = peer_id_of(&client);
        let resp = handler(delivering, bytes).await;
        assert!(
            matches!(resp, JobRelayResponse::Ok { .. }),
            "self-signed peer should be authorized via PeerID-bind, got {resp:?}"
        );
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

        let manifest = inference_manifest(&client, "qwen3-mini", None);
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
            model_cid: "qwen3-mini".to_string(),
            messages: vec![],
            prompt: Some(huge),
            resume_from: None,
            sampling: SamplingParams::default(),
            max_tokens: None,
            stream: true,
        });
        let manifest = ManifestBuilder::new(spec).sign_with(&client).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();

        let resp = handler(PeerId::random(), bytes).await;
        match resp {
            JobRelayResponse::Err { reason } => assert!(reason.contains("prompt too large")),
            other => panic!("expected Err, got {other:?}"),
        }
        assert_eq!(spy.call_count(), 0, "oversized prompt must not reach the worker");
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
        let m1 = serde_json::to_vec(&inference_manifest(&client, "qwen3-mini", Some(4))).unwrap();
        let m2 = serde_json::to_vec(&inference_manifest(&client, "qwen3-mini", Some(4))).unwrap();

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
        assert!(matches!(resp1, JobRelayResponse::Ok { .. }), "job1 should complete");
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
