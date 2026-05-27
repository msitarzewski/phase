// SPDX-License-Identifier: Apache-2.0

//! The `Worker` trait — every Phase node implementation impls this.
//!
//! See `SPEC.md` in this crate for the narrative spec and rationale. This
//! file is the normative type surface.

use crate::job_spec::{ConversationToken, JobResult, JobSpec, JobSpecKind};
// `ConversationToken` is used only by the `should_resume_on_same_peer` helper
// below; keep the import explicit for clarity rather than re-export-only.
use crate::{SignedManifest, SignedReceipt};
use bytes::Bytes;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::watch;

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// A node capable of executing one or more [`JobSpec`] kinds.
///
/// Implementations are e.g. `WasmtimeWorker` (Plasm), `LlamaCppWorker` /
/// `MlxWorker` (LUCID), or a future `ImageGenWorker` / `ScientificWorker`.
///
/// ## Why streaming everywhere?
///
/// `execute()` ALWAYS returns a stream of [`JobEvent`]s. A batch job (a WASM
/// run that produces one result) is the degenerate case of a stream that
/// emits a single `JobEvent::Final` and ends. There is no separate
/// request/response path. This collapses two code paths into one and means
/// the cancellation, signing, and resumption stories work identically for
/// inference and batch work.
///
/// ## Cancellation
///
/// The returned [`JobHandle`] is the cancellation token. Dropping the
/// `JobStream` (the `Stream` half) signals cancellation to the worker, which
/// MUST release KV-cache slots, kill subprocesses, etc. and emit a final
/// `JobEvent::Final { completion: Completion::Cancelled, .. }` before
/// terminating the stream. See SPEC.md § "Cancellation semantics".
///
/// ## Why `async fn` in trait (not `#[async_trait]`)?
///
/// `phase-protocol` requires Rust 1.88 (libp2p 0.57 MSRV). Native async fn
/// in traits is stable since 1.75 and avoids the per-call allocation of
/// `#[async_trait]`. The trait is explicitly `Send + Sync` and the returned
/// future is `Send`, so workers can be used behind `Arc<dyn Worker>`.
pub trait Worker: Send + Sync {
    /// The [`JobSpecKind`]s this worker can serve. A scheduler MUST NOT
    /// dispatch a job whose `kind()` is not in this list; if it does, the
    /// worker MAY return [`WorkerError::Unsupported`].
    ///
    /// Workers that can serve everything (e.g. a generic test fixture)
    /// return a slice of every variant; workers that specialise (e.g.
    /// `LlamaCppWorker`) return just their own.
    fn supported_kinds(&self) -> &[JobSpecKind];

    /// Soft capacity hint — how many in-flight jobs this worker thinks it
    /// can handle. Used by the local-or-DHT router (LUCID M5) for back-
    /// pressure; not normative. Default: 1.
    fn capacity_hint(&self) -> usize {
        1
    }

    /// Begin executing a job. Returns immediately with a handle plus a
    /// stream of [`JobEvent`]s.
    ///
    /// The future resolves once the worker has *accepted* the job (model
    /// loaded into the appropriate slot, WASM module compiled, etc.) — not
    /// when it completes. Completion is signalled by the terminal
    /// `JobEvent::Final` on the stream.
    ///
    /// Errors returned by this future are **dispatch-time** errors
    /// (`Unsupported`, `Capacity`, `BadManifest`). Errors that happen
    /// *during* execution surface as `JobEvent::Final { completion:
    /// Completion::Error, .. }` plus an error field in the signed receipt.
    fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> impl Future<Output = Result<(JobHandle, JobStream), WorkerError>> + Send;
}

/// Object-safe shim. Because `Worker::execute` returns `impl Future`, the
/// raw trait isn't dyn-compatible. `DynWorker` boxes the future so we can
/// store `Arc<dyn DynWorker>` in the dispatch table and `phase-net` swarm.
///
/// Most call sites should depend on `Worker` directly; only the registry /
/// router code uses `DynWorker`.
pub trait DynWorker: Send + Sync {
    fn supported_kinds(&self) -> &[JobSpecKind];
    fn capacity_hint(&self) -> usize;
    // The `Pin<Box<dyn Future<...> + Send + '_>>` shape is the canonical
    // erased-future signature; factoring it behind a type alias hides the
    // `Send` bound at the call site and makes the trait surface harder to
    // read. The trait is stable public API and intentionally explicit.
    #[allow(clippy::type_complexity)]
    fn execute_boxed(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<(JobHandle, JobStream), WorkerError>> + Send + '_>>;
}

impl<W: Worker> DynWorker for W {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        Worker::supported_kinds(self)
    }
    fn capacity_hint(&self) -> usize {
        Worker::capacity_hint(self)
    }
    fn execute_boxed(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<(JobHandle, JobStream), WorkerError>> + Send + '_>>
    {
        Box::pin(Worker::execute(self, job))
    }
}

// ---------------------------------------------------------------------------
// Stream + handle
// ---------------------------------------------------------------------------

/// The stream half returned by [`Worker::execute`].
///
/// Yields zero or more [`JobEvent::Output`] followed by exactly one
/// [`JobEvent::Final`]. After the `Final` event the stream MUST return
/// `None` from `poll_next`.
///
/// Dropping the stream signals cancellation to the worker. The worker still
/// has to produce a `JobEvent::Final` on its internal channel for the signed
/// receipt — see [`JobHandle::finish`] for how a caller retrieves it after
/// drop.
pub type JobStream = Pin<Box<dyn Stream<Item = JobEvent> + Send + 'static>>;

/// Control + signing handle for an in-flight job.
///
/// Caller responsibilities:
/// 1. Drive `JobStream` to completion (or drop it to cancel).
/// 2. Once the stream ends, call [`JobHandle::finish`] to get the
///    [`SignedReceipt<JobResult>`] for accounting / observability /
///    on-network propagation.
///
/// The handle is `Clone` because both the API translator and the scheduler
/// want a copy — the API translator to call `.cancel()` on HTTP-client
/// disconnect, the scheduler to log + persist the receipt.
#[derive(Clone)]
pub struct JobHandle {
    inner: Arc<JobHandleInner>,
}

impl fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandle")
            .field("job_id", &self.inner.job_id)
            .field(
                "cancelled",
                &matches!(*self.inner.cancel_tx.borrow(), CancelState::Cancelled),
            )
            .finish()
    }
}

struct JobHandleInner {
    job_id: JobId,
    cancel_tx: watch::Sender<CancelState>,
    // The receipt is set by the worker when the stream's Final event has
    // been generated AND signed. Callers `.await` on this via `finish()`.
    // `Option<_>` because the receiver gets `take()`-n on first use; later
    // callers see `None` and get `WorkerError::Dropped`.
    receipt_rx:
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<SignedReceipt<JobResult>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelState {
    Running,
    Cancelled,
}

impl JobHandle {
    /// Construct a handle paired with the producer side. Workers call this
    /// inside `execute()` to wire up cancellation and receipt delivery.
    ///
    /// Returns `(handle, producer)` — give the handle to the caller, keep
    /// the producer to drive the stream + sign the receipt.
    pub fn new(job_id: JobId) -> (Self, JobHandleProducer) {
        let (cancel_tx, cancel_rx) = watch::channel(CancelState::Running);
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        let inner = Arc::new(JobHandleInner {
            job_id: job_id.clone(),
            cancel_tx,
            receipt_rx: tokio::sync::Mutex::new(Some(receipt_rx)),
        });
        let handle = JobHandle { inner };
        let producer = JobHandleProducer {
            job_id,
            cancel_rx,
            receipt_tx: Some(receipt_tx),
        };
        (handle, producer)
    }

    /// Unique identifier for this job, derived from the manifest hash.
    pub fn job_id(&self) -> &JobId {
        &self.inner.job_id
    }

    /// Signal the worker to cancel. Idempotent — calling twice is fine.
    ///
    /// After this returns, the worker will continue emitting on the stream
    /// only as far as it takes to flush a `JobEvent::Final` with
    /// `Completion::Cancelled` and shut down cleanly.
    pub fn cancel(&self) {
        // `send` failures mean all receivers dropped — worker already gone.
        let _ = self.inner.cancel_tx.send(CancelState::Cancelled);
    }

    /// Wait for the signed receipt to be available. Resolves when the
    /// worker has emitted its terminal `JobEvent::Final` AND signed it.
    ///
    /// Returns an error if the worker was dropped before producing one —
    /// callers should treat that as `Completion::Error`.
    pub async fn finish(self) -> Result<SignedReceipt<JobResult>, WorkerError> {
        // `finish` is logically consuming (the receipt only arrives once),
        // but we keep `JobHandle: Clone` so multiple holders can call
        // `.cancel()`. The receiver lives behind a mutex+Option; the
        // first caller takes it, later callers get `WorkerError::Dropped`.
        let rx = {
            let mut guard = self.inner.receipt_rx.lock().await;
            guard.take()
        };
        match rx {
            Some(rx) => rx.await.map_err(|_| WorkerError::Dropped),
            None => Err(WorkerError::Dropped),
        }
    }
}

/// The producer side of a [`JobHandle`]. Workers use this internally to
/// observe cancellation and deliver the signed receipt.
pub struct JobHandleProducer {
    job_id: JobId,
    cancel_rx: watch::Receiver<CancelState>,
    receipt_tx: Option<tokio::sync::oneshot::Sender<SignedReceipt<JobResult>>>,
}

impl fmt::Debug for JobHandleProducer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobHandleProducer")
            .field("job_id", &self.job_id)
            .field("receipt_delivered", &self.receipt_tx.is_none())
            .finish()
    }
}

impl JobHandleProducer {
    pub fn job_id(&self) -> &JobId {
        &self.job_id
    }

    /// Has the caller asked for cancellation? Non-blocking — workers
    /// typically poll this between token decodes.
    pub fn is_cancelled(&self) -> bool {
        matches!(*self.cancel_rx.borrow(), CancelState::Cancelled)
    }

    /// `await` until cancellation is requested. Workers `tokio::select!`
    /// this against their token-decode future so cancellation lands within
    /// a single token's worth of latency.
    pub async fn cancelled(&mut self) {
        while *self.cancel_rx.borrow_and_update() == CancelState::Running {
            if self.cancel_rx.changed().await.is_err() {
                // Caller side dropped; treat as cancellation.
                return;
            }
        }
    }

    /// Hand the signed receipt off to whoever's waiting on the
    /// corresponding [`JobHandle::finish`]. Single-shot — calling twice is
    /// a logic bug.
    pub fn deliver_receipt(&mut self, receipt: SignedReceipt<JobResult>) {
        if let Some(tx) = self.receipt_tx.take() {
            let _ = tx.send(receipt);
        }
    }
}

/// Job identifier. Defined as the BLAKE3/SHA-256 of the canonical
/// serialisation of the [`SignedManifest`] header — `phase-manifest` will
/// own the exact construction, this is just the byte container.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub [u8; 32]);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0[..8] {
            write!(f, "{:02x}", b)?;
        }
        f.write_str("…")
    }
}

// ---------------------------------------------------------------------------
// JobEvent — the streamed item type
// ---------------------------------------------------------------------------

/// An item on a [`JobStream`].
///
/// One of:
/// - Zero or more [`JobEvent::Output`] carrying partial-result chunks
///   (tokens for inference, byte ranges for image generation, progress
///   updates for fine-tuning, etc.).
/// - Zero or more [`JobEvent::Progress`] for non-output telemetry (queue
///   wait, prompt-eval percent, etc.).
/// - Exactly one terminal [`JobEvent::Final`] before the stream ends.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobEvent {
    /// A partial-result chunk. Verifier replays these into a
    /// [`CommitmentAccumulator`] and checks the result against the
    /// `output_commitment` field of the signed `JobResult`.
    Output(OutputChunk),

    /// A progress update — informational only, NOT included in the
    /// signing commitment. Use for things the verifier doesn't need to
    /// reconstruct: load percentages, queue positions, time-to-first-token.
    Progress(ProgressUpdate),

    /// Terminal event. After this, the stream MUST yield `None`.
    /// The receipt is *also* delivered through [`JobHandle::finish`] —
    /// `Final` here is the unsigned mirror so streaming consumers (the
    /// Ollama HTTP translator) can emit their terminal frame without
    /// waiting for the cryptographic signing round-trip.
    Final {
        result: JobResult,
        /// Backend-facing error message when `result.completion ==
        /// Completion::Error`. Empty otherwise.
        error: Option<String>,
    },
}

/// A partial-result chunk on the [`JobStream`].
///
/// Workload-agnostic by design. The `kind` field carries a workload-specific
/// discriminator (`"token"` for inference, `"image_tile"` for image gen,
/// `"progress_log"` for fine-tuning, `"stdout"` for WASM streaming output);
/// the `data` field is opaque bytes interpreted per-kind.
///
/// This is the "future workloads" extension point. Adding image generation
/// or scientific compute means defining new chunk kinds + a new `JobSpec`
/// variant — the `Worker` trait itself doesn't grow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputChunk {
    /// Workload-specific discriminator. Stable strings, lower_snake_case.
    /// Reserved kinds: `"token"`, `"image_tile"`, `"progress_log"`,
    /// `"stdout"`, `"stderr"`.
    pub kind: String,

    /// Opaque payload. For `kind: "token"` this is the UTF-8 bytes of the
    /// token text. For `kind: "image_tile"` this is PNG-encoded tile data
    /// preceded by a small header (defined per workload, not by the
    /// protocol). The protocol layer NEVER inspects these bytes.
    #[serde(with = "serde_bytes_field")]
    pub data: Bytes,

    /// Monotonically increasing sequence number within this job. Lets a
    /// verifier detect reordering and out-of-order delivery even after
    /// the commitment accumulator has been checked.
    pub seq: u64,
}

mod serde_bytes_field {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(b: &Bytes, s: S) -> Result<S::Ok, S::Error> {
        serde_bytes::ByteBuf::from(b.to_vec()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
        let v = serde_bytes::ByteBuf::deserialize(d)?;
        Ok(Bytes::from(v.into_vec()))
    }
}

/// An informational progress update. Not signed; not part of the commitment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    /// Free-form lower_snake_case kind: `"queued"`, `"prompt_eval"`,
    /// `"first_token"`, `"model_load"`, etc.
    pub kind: String,
    /// 0.0 → 1.0 fraction complete, or `None` for indeterminate progress.
    pub fraction: Option<f32>,
    /// Optional human-readable detail.
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned synchronously from [`Worker::execute`] — dispatch-time
/// failures. Errors that occur during execution are reported on the stream
/// as `JobEvent::Final { completion: Completion::Error, .. }`.
#[derive(Debug, Error)]
pub enum WorkerError {
    /// The worker does not handle this [`JobSpecKind`].
    #[error("worker does not support job kind {kind:?}")]
    Unsupported { kind: JobSpecKind },

    /// The worker is at its capacity hint and declined the job. Schedulers
    /// MAY retry against a different worker.
    #[error("worker at capacity")]
    Capacity,

    /// Manifest failed verification (signature, ttl, schema). Surfaced
    /// before any execution begins.
    #[error("invalid manifest: {0}")]
    BadManifest(String),

    /// Couldn't fetch a referenced artifact (model weights, WASM module).
    /// Distinct from `BadManifest` because retries against different
    /// artifact-server peers may succeed.
    #[error("artifact unavailable: {0}")]
    ArtifactUnavailable(String),

    /// Worker took longer than its declared `start_deadline` to accept the
    /// job. Used by the router to fail over.
    #[error("dispatch exceeded deadline")]
    DeadlineExceeded,

    /// Caller dropped before completion; the receipt cannot be delivered.
    #[error("worker dropped before completion")]
    Dropped,

    /// Catch-all for backend-specific dispatch failures. Should be rare;
    /// prefer one of the structured variants where possible.
    #[error("worker error: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Resumption affinity helper
// ---------------------------------------------------------------------------

/// Tiny helper a scheduler uses to decide whether to route a follow-up turn
/// to the same worker that issued a [`ConversationToken`]. Stateless;
/// schedulers may inline it.
pub fn should_resume_on_same_peer(token: &ConversationToken, now_unix_ms: u64) -> bool {
    token.valid_until_unix_ms > now_unix_ms
}

/// Recommended grace period a worker SHOULD hold KV-cache state for after
/// emitting a `ConversationToken` if the caller hasn't come back. Defaults
/// align with Ollama's `keep_alive=5m`.
pub const DEFAULT_RESUMPTION_GRACE: Duration = Duration::from_secs(5 * 60);
