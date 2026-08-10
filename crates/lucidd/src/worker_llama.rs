// SPDX-License-Identifier: AGPL-3.0-or-later

//! `LlamaCppWorker` — the GPU-inference [`Worker`] implementation for LUCID.
//!
//! Shells out to `llama-server` (from llama.cpp), keeps one subprocess per
//! loaded model, and translates the protocol's `JobSpec::Inference` into
//! HTTP streaming requests against that subprocess. Tokens come back as
//! Server-Sent Events on `POST /completion`; we re-emit them as
//! [`JobEvent::Output`] frames and fold them into a
//! [`CommitmentAccumulator`] so the receipt's `output_commitment` is a real
//! cryptographic hash of what we shipped.
//!
//! ## Subprocess lifecycle
//!
//! Each loaded model is a `(child_process, supervisor_task, port)` triple.
//! The supervisor task — one per model — runs three concurrent things:
//!
//! 1. **Liveness watch.** `child.wait()` so a clean (or dirty) exit is
//!    detected the instant it happens. Three crashes within 60 s evicts
//!    the model and surfaces as `WorkerError::Other` on the next request.
//! 2. **Periodic /health poll.** Every 30 s, GET `/health`. After five
//!    consecutive failures we treat the process as crashed even if the
//!    OS still thinks it's running (hung llama-server, jammed CUDA driver).
//! 3. **Restart on crash.** Re-spawn with exponential backoff (1s, 2s, 4s).
//!    Three failures within the rolling 60 s window stop the loop and emit
//!    a "failed" sentinel.
//!
//! Per-request hang detection lives in [`stream_completion`]: if no SSE
//! frame arrives for 30 s the request is aborted and the underlying model
//! is signalled as suspect (next request triggers a health check).
//!
//! ## Why `POST /completion` rather than `/v1/chat/completions`?
//!
//! Both stream; both work. The native `/completion` endpoint has a simpler
//! frame shape (`{"content": "...", "stop": bool}`) that we don't have to
//! reassemble from `delta.content` like the OpenAI flavour, and it doesn't
//! emit the `data: [DONE]` sentinel — easier to parse correctly with the
//! tiny SSE splitter below. Chat-template formatting is handled
//! client-side: we render the conversation into a single prompt string
//! before sending. With `--jinja` enabled on the server, the OpenAI path
//! would handle the template — but rendering ourselves keeps the worker
//! deterministic across llama-server versions.
//!
//! ## What this file deliberately does NOT do
//!
//! - Eviction policy beyond crash handling (LUCID M6 — model registry +
//!   DHT-aware eviction).
//! - Model downloads (artifact-server's job; we expect GGUFs to already be
//!   in `model_dir`).
//! - Quantization or backend-selection logic — those are flag-string
//!   knobs on [`LlamaCppConfig`] that callers populate.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::stream;
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures::StreamExt;
use phase_identity::NodeIdentity;
use phase_protocol::{
    ChatRole, CommitmentAccumulator, Completion, EmbeddingJobSpec, InferenceJobSpec, JobEvent,
    JobHandle, JobHandleProducer, JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream,
    OutputChunk, SamplingParams, SignedManifest, Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

/// Defense-in-depth ceiling enforced by the worker even when a caller bypasses
/// the router's operator-configured `max_tokens_ceiling`.
const MAX_N_PREDICT: u32 = 8192;

/// A missing client limit must never inherit llama.cpp's backend default,
/// which may be unlimited. Keep the default modest while allowing an explicit
/// request up to [`MAX_N_PREDICT`].
const DEFAULT_N_PREDICT: u32 = 512;

const MAX_TOP_K: u64 = 1_000;
const MAX_SEED: u64 = i32::MAX as u64;
const MAX_STOP_SEQUENCES: usize = 16;
const MAX_STOP_SEQUENCE_CHARS: usize = 256;

/// Native llama.cpp responses cross a process boundary and are untrusted even
/// though the HTTP socket is loopback-only.
const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;
const SSE_INGEST_SLICE_BYTES: usize = 4 * 1024;
const MAX_BACKEND_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_BACKEND_ERROR_TEXT_CHARS: usize = 1_024;
const MAX_EMBEDDING_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_EMBEDDING_INPUTS: usize = 128;
const MAX_EMBEDDING_ENTRY_CHARS: usize = 64 * 1024;
const MAX_EMBEDDING_TOTAL_CHARS: usize = 256 * 1024;
const MAX_CHILD_LOG_LINE_BYTES: usize = 4 * 1024;
const MAX_CHILD_LOG_TEXT_CHARS: usize = 4 * 1024;
const CHILD_LOG_READ_BUFFER_BYTES: usize = 1_024;

/// Configuration for [`LlamaCppWorker`].
///
/// Carriers (CLI flags, env vars, config files) populate this; the worker
/// itself is config-agnostic.
#[derive(Debug, Clone)]
pub struct LlamaCppConfig {
    /// Filesystem path to the `llama-server` binary. Resolved on every
    /// model load (so swapping binaries between loads is allowed).
    pub server_binary_path: PathBuf,

    /// Directory containing GGUF files. `model_id` is appended verbatim
    /// (with `.gguf` if not already present) — no fancy CID resolution
    /// yet; LUCID M6 handles that.
    pub model_dir: PathBuf,

    /// Default `--n-gpu-layers` value. `0` = CPU only, `i32::MAX` = "all"
    /// per llama-server's CLI semantics (we render `--n-gpu-layers all`
    /// when this is `i32::MAX`).
    pub default_n_gpu_layers: i32,

    /// Default context window. Per-request `max_tokens` is enforced
    /// server-side via `n_predict`.
    pub default_context_size: usize,

    /// Port pool the worker draws from when spawning subprocesses. The
    /// caller decides the policy (small dev pool, large multi-tenant pool,
    /// etc.); the worker just allocates the next free port from this range.
    pub server_port_range: Range<u16>,

    /// Maximum number of `llama-server` subprocesses (loaded models) kept
    /// resident at once. Each model is ~GB of RAM; without a cap a caller
    /// (or a relay peer) can pin every on-disk model into memory and
    /// exhaust the host (SEC-07). When at cap, [`LlamaCppWorker::ensure_loaded`]
    /// evicts the least-recently-used model before spawning a new one.
    pub max_loaded_models: usize,

    /// Maximum wall-clock wait for `/health` to return 200 after spawn.
    /// 60 s default; big models on slow disks legitimately take longer.
    pub model_load_timeout: Duration,

    /// Inter-token hang threshold. If no SSE frame arrives within this
    /// window, the in-flight request is aborted and the model marked
    /// suspect. 30 s default matches the research brief's hang guidance.
    pub per_request_idle_timeout: Duration,

    /// Extra environment variables to set on the `llama-server` child.
    /// Production callers typically leave this empty; the test fixture
    /// uses it to configure the in-tree `fake-llama-server` per-spawn
    /// rather than mutating the parent process env (which races across
    /// concurrent tokio tests).
    #[doc(hidden)]
    pub extra_env: Vec<(String, String)>,
}

impl Default for LlamaCppConfig {
    fn default() -> Self {
        Self {
            server_binary_path: PathBuf::from("llama-server"),
            model_dir: PathBuf::from("./models"),
            default_n_gpu_layers: i32::MAX, // "all"
            default_context_size: 8192,
            server_port_range: 18080..18200,
            max_loaded_models: 3,
            model_load_timeout: Duration::from_secs(60),
            per_request_idle_timeout: Duration::from_secs(30),
            extra_env: Vec::new(),
        }
    }
}

/// State of a single loaded model. Held inside the worker's `DashMap`
/// keyed by `model_id`.
///
/// `child` is wrapped in `Mutex` because the supervisor task occasionally
/// needs to `kill()` it (on crash beyond retry budget, on drop, on
/// explicit unload), and we don't want to serialise reads behind a write
/// lock the way a single `RwLock<Child>` would.
struct LoadedModel {
    /// Bound port — used to construct `http://127.0.0.1:{port}/completion`.
    port: u16,
    /// Updated on every successful inference for the current LRU eviction
    /// policy.
    last_used: Mutex<Instant>,
    /// Signalled when the supervisor task has given up (3 crashes in 60s,
    /// or unload requested). All in-flight requests should bail.
    failed: Arc<Notify>,
    /// Set to true once the supervisor declared the model dead. Reads
    /// under acquire/release ordering; a stale `false` just means one
    /// extra retry that will hit `failed.notified()` immediately.
    failed_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Join handle for the supervisor task. Held so we can abort it on
    /// `Drop` of the [`LlamaCppWorker`] or on LRU eviction.
    supervisor: tokio::task::JoinHandle<()>,
}

impl LoadedModel {
    /// Tear this model down: mark it failed (so any in-flight request
    /// bails and a concurrent `ensure_loaded` won't hand it out), then
    /// abort the supervisor task. The supervisor owns the `llama-server`
    /// `Child`, which carries `kill_on_drop(true)`, so aborting the task
    /// drops the `Child` and the OS reaps the subprocess — the same kill
    /// path used on `Drop` of the worker. Idempotent.
    fn shutdown(&self) {
        self.failed_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.failed.notify_waiters();
        self.supervisor.abort();
    }
}

/// The GPU-inference worker. Cheaply cloneable — internal state is behind
/// an `Arc` so handing copies to per-request handlers is fine.
#[derive(Clone)]
pub struct LlamaCppWorker {
    inner: Arc<Inner>,
}

struct Inner {
    identity: NodeIdentity,
    loaded_models: DashMap<String, Arc<LoadedModel>>,
    config: LlamaCppConfig,
    client: reqwest::Client,
    /// Serializes the check/evict/port-reserve/spawn/insert transaction.
    /// Model loads are rare and expensive, so this global single-flight gate
    /// is preferable to allowing concurrent cold starts to bypass the
    /// resident-model cap or replace one another in `loaded_models`.
    load_gate: Mutex<()>,
    /// Set of ports currently bound by live `llama-server` children. A
    /// port is inserted in [`LlamaCppWorker::allocate_port`] and removed on
    /// unload/evict so the range can't wrap onto a live port (SEC-07).
    /// Guarded by a `Mutex` rather than a lock-free set so allocate +
    /// "is the range full?" is one atomic decision.
    ports_in_use: Mutex<std::collections::HashSet<u16>>,
}

impl LlamaCppWorker {
    /// Construct a fresh worker. No subprocesses are spawned until the
    /// first inference request for a given model.
    pub fn new(identity: NodeIdentity, config: LlamaCppConfig) -> Self {
        let client = reqwest::Client::builder()
            // Loopback subprocess traffic must never inherit HTTP(S)_PROXY.
            // A hostile local proxy could impersonate `/health` or exfiltrate
            // prompt bodies. Likewise, no llama endpoint is allowed to turn a
            // loopback request into a redirect to another origin.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            // The default 30s connection timeout would surface on first
            // load; we manage our own timeout via `model_load_timeout`.
            // Per-request response read timeout is unbounded — streaming
            // bodies legitimately stay open for minutes — and we enforce
            // idleness inside `stream_completion`.
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .build()
            .expect("reqwest client (rustls-tls) builds with default config");
        Self {
            inner: Arc::new(Inner {
                identity,
                loaded_models: DashMap::new(),
                config,
                client,
                load_gate: Mutex::new(()),
                ports_in_use: Mutex::new(std::collections::HashSet::new()),
            }),
        }
    }

    /// Ensure a model is loaded. Idempotent — if the model is already
    /// loaded, returns the existing entry. If not, spawns a new
    /// `llama-server` subprocess and waits for `/health` to go green
    /// before returning.
    ///
    /// `embeddings` selects which *flavour* of the model to load. A chat
    /// model and an embedding model are spun up differently in llama-server
    /// (`--embeddings` enables the `/embedding` endpoint and switches the
    /// pooling mode), so the two cannot share one subprocess even for the
    /// same `model_id`. To let both coexist we key the embedding instance
    /// under a distinct composite id (`"{model_id}\u{0}emb"`); the NUL byte
    /// is never valid in a caller-supplied id (rejected by
    /// [`resolve_model_path`]), so the namespaces can't collide. Path
    /// resolution always uses the BARE `model_id` — the same GGUF backs both
    /// flavours.
    async fn ensure_loaded(
        &self,
        model_id: &str,
        embeddings: bool,
    ) -> Result<Arc<LoadedModel>, WorkerError> {
        // The DashMap key distinguishes chat vs embedding instances; the
        // path resolved below strips the suffix back off.
        let load_key = if embeddings {
            format!("{model_id}\u{0}emb")
        } else {
            model_id.to_string()
        };

        // This covers every mutation involved in a cold load. It also acts as
        // same-key single-flight: a waiter re-checks the map after the winner
        // inserts and returns that exact process instead of spawning a
        // duplicate. Loading is serialized intentionally because each cold
        // start can consume gigabytes of RAM.
        let _load_guard = self.inner.load_gate.lock().await;

        if let Some(existing) = self.inner.loaded_models.get(&load_key) {
            if !existing
                .failed_flag
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(existing.clone());
            }
            // The previous load has been declared dead; drop it and try
            // again. The supervisor's `kill()` already ran.
            let stale_port = existing.port;
            drop(existing);
            self.inner.loaded_models.remove(&load_key);
            self.release_port(stale_port).await;
        }

        // SEC-04: confine the resolved path to `model_dir`. Any traversal
        // / absolute / leading-dash / nul id is rejected here, before the
        // path ever reaches the spawn. Not-found and an invalid id collapse
        // to the SAME generic client error (oracle closed); the specific
        // reason is logged server-side only. Resolution uses the BARE
        // `model_id`, never the composite key, so the embedding flavour
        // loads the same GGUF as the chat flavour.
        let model_path = match resolve_model_path(&self.inner.config.model_dir, model_id) {
            Ok(p) => p,
            Err(detail) => {
                tracing::warn!(
                    model = %model_id,
                    reason = %detail,
                    "model path resolution rejected"
                );
                return Err(WorkerError::ArtifactUnavailable("model unavailable".into()));
            }
        };

        // SEC-07: enforce the resident-model cap before spawning. If we're
        // at the cap, evict the least-recently-used model first. Done
        // before `allocate_port` so the freed port is available to the new
        // model and we don't trip the range-full check unnecessarily. The
        // incoming candidate is the composite key so a chat instance of the
        // same model isn't mistaken for "already the incoming one".
        self.evict_lru_if_at_cap(&load_key).await;

        let port = self.allocate_port().await?;
        let mut child = match spawn_llama_server(
            &self.inner.config.server_binary_path,
            &model_path,
            port,
            self.inner.config.default_n_gpu_layers,
            self.inner.config.default_context_size,
            &self.inner.config.extra_env,
            embeddings,
        ) {
            Ok(c) => c,
            Err(e) => {
                self.release_port(port).await;
                return Err(WorkerError::Other(format!("spawn llama-server: {e}")));
            }
        };

        // Wait for /health to go 200 before declaring the model loaded.
        if let Err(e) = wait_for_health(
            &self.inner.client,
            port,
            self.inner.config.model_load_timeout,
            &mut child,
        )
        .await
        {
            self.release_port(port).await;
            return Err(WorkerError::Other(format!(
                "llama-server health check: {e}"
            )));
        }

        let failed = Arc::new(Notify::new());
        let failed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // The map entry is keyed by the composite `load_key`, but the
        // `model_id` carried into telemetry stays the bare alias so logs
        // read naturally. The supervisor re-spawns with the same
        // `embeddings` flag so a crashed embedding instance comes back as an
        // embedding instance.
        let model_id_owned = model_id.to_string();
        let supervisor_input = SupervisorInput {
            model_id: model_id_owned.clone(),
            port,
            failed: failed.clone(),
            failed_flag: failed_flag.clone(),
            client: self.inner.client.clone(),
            config: self.inner.config.clone(),
            model_path: model_path.clone(),
            embeddings,
        };

        // The supervisor task gets the child handle (so it can wait/kill).
        // The `LoadedModel`'s `Mutex<Option<Child>>` starts empty — the
        // supervisor "owns" the process for its lifetime. Re-spawned
        // children are also held inside the supervisor.
        let supervisor = tokio::spawn(run_supervisor(supervisor_input, child));

        let loaded = Arc::new(LoadedModel {
            port,
            last_used: Mutex::new(Instant::now()),
            failed,
            failed_flag,
            supervisor,
        });
        // `load_gate` makes this insert a single-flight commit. Keep the
        // replacement guard as defense in depth if the synchronization model
        // changes later.
        if let Some(prev) = self.inner.loaded_models.insert(load_key, loaded.clone()) {
            if prev.port != port {
                prev.shutdown();
                self.release_port(prev.port).await;
            }
        }
        Ok(loaded)
    }

    /// SEC-07: allocate a port not currently bound by a live child. Scans
    /// the configured range for the first free slot and reserves it. When
    /// every port in the range is in use, returns [`WorkerError::Capacity`]
    /// rather than wrapping onto a live port (the old `fetch_add % span`
    /// behaviour, which silently collided and churned spawn/fail).
    async fn allocate_port(&self) -> Result<u16, WorkerError> {
        let range = self.inner.config.server_port_range.clone();
        let mut in_use = self.inner.ports_in_use.lock().await;
        for port in range.clone() {
            if in_use.contains(&port) {
                continue;
            }
            // Prove that the OS currently considers the loopback address
            // bindable, not merely that this process has not recorded it.
            // llama-server cannot inherit this listener, so a narrow
            // close-to-child-bind race remains; child liveness checks around
            // `/health` below prevent a normally failing child from accepting
            // an unrelated listener as healthy.
            if std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
                in_use.insert(port);
                return Ok(port);
            }
        }
        tracing::warn!(
            range_start = range.start,
            range_end = range.end,
            "llama-server port range exhausted; refusing load"
        );
        Err(WorkerError::Capacity)
    }

    /// Release a previously-[`allocate_port`](Self::allocate_port)ed port
    /// back into the pool on unload/evict/spawn-failure.
    async fn release_port(&self, port: u16) {
        self.inner.ports_in_use.lock().await.remove(&port);
    }

    /// SEC-07: if the worker is already at `max_loaded_models`, evict the
    /// least-recently-used model (by `last_used`) to make room for a new
    /// one. The model currently being (re)loaded — `incoming` — is never a
    /// candidate. The evicted model's subprocess is killed via
    /// [`LoadedModel::shutdown`] and its port released.
    async fn evict_lru_if_at_cap(&self, incoming: &str) {
        let cap = self.inner.config.max_loaded_models.max(1);
        // Evict in a loop in case we're over cap (e.g. cap was lowered or
        // a prior failure left an extra entry). Bounded by the map size.
        loop {
            if self.inner.loaded_models.len() < cap {
                return;
            }
            // Find the LRU victim. `last_used` is a `Mutex<Instant>`; read
            // each under its lock. We hold no DashMap shard lock across the
            // await by collecting candidates first.
            let mut victim: Option<(String, Instant)> = None;
            let candidates: Vec<(String, Arc<LoadedModel>)> = self
                .inner
                .loaded_models
                .iter()
                .filter(|e| e.key() != incoming)
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect();
            for (id, model) in &candidates {
                let used = *model.last_used.lock().await;
                match &victim {
                    Some((_, t)) if *t <= used => {}
                    _ => victim = Some((id.clone(), used)),
                }
            }
            let Some((victim_id, _)) = victim else {
                // Nothing evictable (only the incoming model present, or
                // map empty under a racing remove). Let the load proceed;
                // worst case we momentarily exceed cap by one.
                return;
            };
            if let Some((_, model)) = self.inner.loaded_models.remove(&victim_id) {
                tracing::info!(model = %victim_id, "evicting LRU model to honour max_loaded_models");
                model.shutdown();
                self.release_port(model.port).await;
            }
            // Re-check the cap; another concurrent loader may have changed
            // the map. The loop terminates because each iteration either
            // returns or removes one entry.
        }
    }
}

impl Worker for LlamaCppWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        &[JobSpecKind::Inference, JobSpecKind::Embedding]
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let job_id = JobId(manifest_hash);

        match &job.payload {
            JobSpec::Inference(spec) => {
                let inference = spec.clone();

                // The native `/completion` path below is text-only. Silently
                // discarding protocol-level images would execute a different
                // job than the signed manifest describes, so reject before
                // either model loading or backend dispatch.
                if inference
                    .messages
                    .iter()
                    .any(|message| !message.images.is_empty())
                {
                    return Err(WorkerError::BadManifest(
                        "multimodal images are not supported by the llama.cpp text backend"
                            .to_string(),
                    ));
                }

                // Validate and freeze the exact backend request before model
                // loading. Sampling fields cross a trust boundary here: they
                // must not overwrite server-owned fields or rely on
                // llama.cpp-specific permissive defaults.
                let request = build_completion_request(&inference)
                    .map_err(|e| WorkerError::BadManifest(format!("invalid sampling: {e}")))?;

                // Load the model up front so dispatch-time errors are
                // returned through `WorkerError` rather than as a single
                // `Final::Error` event with no chunks. Once we get past
                // this point the only failure mode is in-stream. The chat
                // path always loads the non-embedding flavour.
                let model = self.ensure_loaded(&inference.model_cid, false).await?;

                let (handle, producer) = JobHandle::new(job_id);
                let identity = self.inner.identity.clone();
                let client = self.inner.client.clone();
                let idle_timeout = self.inner.config.per_request_idle_timeout;

                let stream: JobStream = Box::pin(run_inference(
                    client,
                    model,
                    request,
                    manifest_hash,
                    producer,
                    identity,
                    idle_timeout,
                ));
                Ok((handle, stream))
            }
            JobSpec::Embedding(spec) => {
                let embedding = spec.clone();
                validate_embedding_spec(&embedding).map_err(WorkerError::BadManifest)?;

                // Embedding loads spin up a *separate* llama-server with
                // `--embeddings`, keyed apart from any chat instance of the
                // same model (see `ensure_loaded`). Same up-front-load
                // contract as the inference path.
                let model = self.ensure_loaded(&embedding.model_cid, true).await?;

                let (handle, producer) = JobHandle::new(job_id);
                let identity = self.inner.identity.clone();
                let client = self.inner.client.clone();
                let request_timeout = self.inner.config.per_request_idle_timeout;

                let stream: JobStream = Box::pin(run_embedding(
                    client,
                    model,
                    embedding,
                    manifest_hash,
                    producer,
                    identity,
                    request_timeout,
                ));
                Ok((handle, stream))
            }
            other => Err(WorkerError::Unsupported { kind: other.kind() }),
        }
    }
}

fn validate_embedding_spec(spec: &EmbeddingJobSpec) -> Result<(), String> {
    if spec.input.is_empty() {
        return Err("embedding input must contain at least one non-empty entry".to_string());
    }
    if spec.input.len() > MAX_EMBEDDING_INPUTS {
        return Err(format!(
            "embedding input count {} exceeds {MAX_EMBEDDING_INPUTS}",
            spec.input.len()
        ));
    }
    let mut total_chars = 0_usize;
    for (index, entry) in spec.input.iter().enumerate() {
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
        if total_chars > MAX_EMBEDDING_TOTAL_CHARS {
            return Err(format!(
                "embedding input exceeds {MAX_EMBEDDING_TOTAL_CHARS} aggregate characters"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Subprocess management
// ---------------------------------------------------------------------------

/// SEC-04: resolve a caller-supplied `model_id` into a GGUF path that is
/// **provably inside** `model_dir`, or reject it.
///
/// The `model_id` arrives from the local Ollama HTTP API (and, gated by a
/// model-loaded check, from relay peers) and is therefore untrusted. A
/// naive `model_dir.join(model_id)` lets `"../../etc/passwd"` escape the
/// directory (`Path::join` does not normalise `..`) and lets an absolute
/// id replace the base entirely — feeding arbitrary files into
/// llama.cpp's C++ GGUF parser (a memory-unsafe mmap/parse surface with
/// CVE history) and turning the existence check into a filesystem oracle.
///
/// We confine in two layers:
///
/// 1. **Reject hostile shapes outright.** Empty, NUL, any path separator
///    (`/` or `\`), a `..` component, or a leading `-` (which would also
///    let the id masquerade as a `--flag` to `llama-server` — arg
///    injection) are refused before touching the filesystem.
/// 2. **Canonicalize-and-confine.** Join `model_dir/<id>.gguf`,
///    `canonicalize()` both it and `model_dir`, and require the resolved
///    path to start with the resolved base. This closes symlink-escape
///    even for ids that pass the shape check.
///
/// On any violation returns `Err(reason)`; the caller maps every reason
/// to the same generic client-facing error (oracle closed) and logs the
/// detail server-side.
fn resolve_model_path(model_dir: &Path, model_id: &str) -> Result<PathBuf, String> {
    if model_id.is_empty()
        || model_id.contains('\0')
        || model_id.contains('/')
        || model_id.contains('\\')
        || model_id.contains("..")
        || model_id.starts_with('-')
    {
        return Err(format!("invalid model id: {model_id:?}"));
    }

    let candidate = model_dir.join(format!("{model_id}.gguf"));
    let canon = candidate
        .canonicalize()
        .map_err(|e| format!("model not found ({}): {e}", candidate.display()))?;
    let base = model_dir
        .canonicalize()
        .map_err(|e| format!("bad model_dir ({}): {e}", model_dir.display()))?;
    if !canon.starts_with(&base) {
        return Err(format!(
            "resolved path {} escapes model_dir {}",
            canon.display(),
            base.display()
        ));
    }
    Ok(canon)
}

/// Spawn the actual subprocess. Returns immediately — caller waits on
/// `/health` separately.
fn spawn_llama_server(
    binary: &Path,
    model: &Path,
    port: u16,
    n_gpu_layers: i32,
    ctx_size: usize,
    extra_env: &[(String, String)],
    embeddings: bool,
) -> std::io::Result<Child> {
    // SEC-04 (L8): require an absolute binary path. Resolving `llama-server`
    // via the inherited `$PATH` is a binary-hijack vector (an attacker who
    // can prepend a dir to PATH gets code execution as lucidd). The startup
    // path canonicalizes + existence-checks the configured binary; reject
    // anything relative here as defence-in-depth.
    if !binary.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "llama-server binary path must be absolute, got {}",
                binary.display()
            ),
        ));
    }

    let mut cmd = Command::new(binary);

    // SEC-04 (L8): do not leak lucidd's entire environment (which may hold
    // identity-key paths, tokens, etc.) into the subprocess, and don't let
    // an inherited PATH influence anything the child shells out to. Start
    // from an empty environment and add back only a minimal, known PATH
    // plus any explicitly-configured `extra_env`.
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/local/bin");

    cmd.arg("--model").arg(model);
    cmd.arg("--host").arg("127.0.0.1");
    cmd.arg("--port").arg(port.to_string());
    cmd.arg("--ctx-size").arg(ctx_size.to_string());
    if n_gpu_layers == i32::MAX {
        cmd.arg("--n-gpu-layers").arg("all");
    } else {
        cmd.arg("--n-gpu-layers").arg(n_gpu_layers.to_string());
    }
    // Embedding loads are a different beast: `--embeddings` turns on the
    // `/embedding` endpoint, and `--pooling mean` makes the server return
    // one fixed-width vector per input rather than per-token states. We
    // never set these on the chat path — they'd disable generation.
    if embeddings {
        cmd.arg("--embeddings");
        cmd.arg("--pooling").arg("mean");
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // `--jinja` enables modern chat templates + tool calling. Research
    // brief flags this as "always set"; without it tool calls get silently
    // dropped on the OpenAI-compat path. We don't use that path today but
    // the cost of enabling it is zero.
    cmd.arg("--jinja");
    // Capture stdout/stderr so the supervisor can drain them (otherwise
    // a chatty child fills its pipe and blocks).
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // Process-group isolation (`setsid` on Unix) would be nice so a
    // signal targeted at lucidd doesn't propagate willy-nilly through
    // the subprocess tree, but adding the `libc` dependency just for
    // that is overkill — `kill_on_drop(true)` plus an explicit
    // `child.kill()` in the supervisor already handles every failure
    // mode we care about in practice.
    cmd.kill_on_drop(true);
    cmd.spawn()
}

/// Block (asynchronously) until `GET /health` returns 200 or `timeout`
/// elapses. 503 means "still loading" per the research brief — keep
/// polling. Connect-refused with no response after the timeout is fatal.
async fn wait_for_health(
    client: &reqwest::Client,
    port: u16,
    deadline: Duration,
    child: &mut Child,
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/health");
    let started = Instant::now();
    let poll_interval = Duration::from_millis(200);
    let mut last_err = String::from("never responded");
    while started.elapsed() < deadline {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("check child liveness: {error}"))?
        {
            return Err(format!(
                "llama-server exited before health check completed ({status})"
            ));
        }
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                // Re-check after receiving the response. This closes the
                // practical impostor case where llama-server lost its bind
                // race and exited while another loopback process answered.
                // A short settlement window gives the spawned process time
                // to report an asynchronous bind failure before we trust the
                // health response. Direct socket-owner attestation is not
                // portable, so the close→bind race remains a narrow,
                // explicitly documented residual.
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| format!("check child liveness: {error}"))?
                {
                    return Err(format!(
                        "llama-server exited while health endpoint responded ({status})"
                    ));
                }
                return Ok(());
            }
            Ok(resp) => {
                // 503 = still loading; anything else = real failure.
                last_err = format!("status {}", resp.status());
            }
            Err(e) => {
                last_err = e.to_string();
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
    Err(format!("did not become healthy in time ({last_err})"))
}

/// Inputs the supervisor task needs to do its work.
struct SupervisorInput {
    model_id: String,
    port: u16,
    failed: Arc<Notify>,
    failed_flag: Arc<std::sync::atomic::AtomicBool>,
    client: reqwest::Client,
    config: LlamaCppConfig,
    model_path: PathBuf,
    /// Whether this instance was spawned with `--embeddings`. Re-spawns on
    /// crash must reuse the same flavour or the endpoint shape would flip.
    embeddings: bool,
}

/// Long-running task: watch the child, restart on crash up to 3 times in
/// 60 s, periodically poll `/health`. Owns the `Child` for the lifetime
/// of the loaded model.
async fn run_supervisor(input: SupervisorInput, initial_child: Child) {
    let SupervisorInput {
        model_id,
        port,
        failed,
        failed_flag,
        client,
        config,
        model_path,
        embeddings,
    } = input;

    // Sliding window of recent crash timestamps. If we accumulate three
    // entries inside a 60 s window, give up.
    let mut crash_times: Vec<Instant> = Vec::new();
    let crash_window = Duration::from_secs(60);
    let mut current_child: Option<Child> = Some(initial_child);

    // Drain stdout/stderr so the child doesn't block on a full pipe.
    // We re-create these drainers on every restart.
    drain_child_io(&mut current_child);

    loop {
        // Two concurrent things: wait for the child to exit, and run
        // periodic /health checks. Whichever fires first decides what
        // happens next.
        let mut child = match current_child.take() {
            Some(c) => c,
            None => break,
        };

        let health_url = format!("http://127.0.0.1:{port}/health");
        let mut consecutive_health_fail: u32 = 0;
        let health_interval = Duration::from_secs(30);
        let mut health_timer = tokio::time::interval(health_interval);
        // Tick once immediately to clear the "first tick is now" behaviour.
        health_timer.tick().await;

        let exit_kind: ChildExit = loop {
            tokio::select! {
                biased;
                wait_res = child.wait() => {
                    match wait_res {
                        Ok(status) if status.success() => {
                            break ChildExit::CleanExit;
                        }
                        Ok(status) => {
                            tracing::warn!(model = %model_id, ?status, "llama-server exited non-zero");
                            break ChildExit::Crash;
                        }
                        Err(e) => {
                            tracing::warn!(model = %model_id, error = %e, "child.wait() failed");
                            break ChildExit::Crash;
                        }
                    }
                }
                _ = health_timer.tick() => {
                    match client.get(&health_url).timeout(Duration::from_secs(5)).send().await {
                        Ok(r) if r.status().is_success() => {
                            consecutive_health_fail = 0;
                        }
                        _ => {
                            consecutive_health_fail += 1;
                            if consecutive_health_fail >= 5 {
                                tracing::warn!(
                                    model = %model_id,
                                    "5 consecutive /health failures; killing child"
                                );
                                let _ = child.kill().await;
                                break ChildExit::HealthDead;
                            }
                        }
                    }
                }
            }
        };

        match exit_kind {
            ChildExit::CleanExit => {
                // Caller explicitly killed it via Drop or unload. Don't
                // restart.
                tracing::info!(model = %model_id, "llama-server exited cleanly");
                failed_flag.store(true, std::sync::atomic::Ordering::Release);
                failed.notify_waiters();
                return;
            }
            ChildExit::Crash | ChildExit::HealthDead => {
                let now = Instant::now();
                crash_times.retain(|t| now.duration_since(*t) < crash_window);
                crash_times.push(now);
                if crash_times.len() >= 3 {
                    tracing::error!(
                        model = %model_id,
                        crashes = crash_times.len(),
                        "model crashed 3 times in 60s; giving up"
                    );
                    failed_flag.store(true, std::sync::atomic::Ordering::Release);
                    failed.notify_waiters();
                    return;
                }
                // Exponential backoff: 1s, 2s, 4s.
                let backoff_secs = 1u64 << crash_times.len().saturating_sub(1);
                let backoff = Duration::from_secs(backoff_secs).min(Duration::from_secs(8));
                tracing::info!(model = %model_id, backoff_ms = backoff.as_millis() as u64, "restarting llama-server");
                tokio::time::sleep(backoff).await;
                let respawned = spawn_llama_server(
                    &config.server_binary_path,
                    &model_path,
                    port,
                    config.default_n_gpu_layers,
                    config.default_context_size,
                    &config.extra_env,
                    embeddings,
                );
                match respawned {
                    Ok(c) => {
                        let mut respawned_opt = Some(c);
                        drain_child_io(&mut respawned_opt);
                        let mut c = respawned_opt.expect("respawned child remains present");
                        if let Err(e) =
                            wait_for_health(&client, port, config.model_load_timeout, &mut c).await
                        {
                            tracing::warn!(
                                model = %model_id,
                                error = %e,
                                "respawned llama-server failed health check"
                            );
                            let _ = c.kill().await;
                            crash_times.push(Instant::now());
                            if crash_times.len() >= 3 {
                                failed_flag.store(true, std::sync::atomic::Ordering::Release);
                                failed.notify_waiters();
                                return;
                            }
                            failed_flag.store(true, std::sync::atomic::Ordering::Release);
                            failed.notify_waiters();
                            return;
                        }
                        current_child = Some(c);
                    }
                    Err(e) => {
                        tracing::error!(model = %model_id, error = %e, "failed to respawn");
                        failed_flag.store(true, std::sync::atomic::Ordering::Release);
                        failed.notify_waiters();
                        return;
                    }
                }
            }
        }
    }
}

enum ChildExit {
    CleanExit,
    Crash,
    HealthDead,
}

/// Drain a child's stdout/stderr in the background so a chatty subprocess
/// can't block on a full pipe. Logs lines at TRACE so they're visible
/// under `RUST_LOG=lucidd::worker_llama=trace` without polluting INFO.
fn drain_child_io(child: &mut Option<Child>) {
    let Some(child) = child.as_mut() else { return };
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(drain_child_reader(stdout, "stdout"));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(drain_child_reader(stderr, "stderr"));
    }
}

/// Drain a child pipe without `AsyncBufReadExt::lines()`: that convenience API
/// grows its internal `String` until a newline and therefore lets a malformed
/// subprocess allocate unbounded memory. This loop keeps at most one bounded
/// line and discards overflow until the next newline.
async fn drain_child_reader<R>(mut reader: R, child_stream: &'static str)
where
    R: AsyncRead + Unpin,
{
    let mut read_buf = [0u8; CHILD_LOG_READ_BUFFER_BYTES];
    let mut line = Vec::with_capacity(MAX_CHILD_LOG_LINE_BYTES.min(1_024));
    let mut truncated = false;

    loop {
        let read = match reader.read(&mut read_buf).await {
            Ok(0) => {
                if !line.is_empty() || truncated {
                    log_child_line(child_stream, &line, truncated);
                }
                return;
            }
            Ok(read) => read,
            Err(error) => {
                tracing::warn!(
                    target: "lucidd::llama_server",
                    stream = child_stream,
                    error = %error,
                    "failed reading bounded child output"
                );
                return;
            }
        };

        for byte in &read_buf[..read] {
            if *byte == b'\n' {
                log_child_line(child_stream, &line, truncated);
                line.clear();
                truncated = false;
            } else if line.len() < MAX_CHILD_LOG_LINE_BYTES {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }
}

fn log_child_line(child_stream: &'static str, line: &[u8], truncated: bool) {
    let sanitized = sanitize_bounded_log_text(line, MAX_CHILD_LOG_TEXT_CHARS, truncated);
    tracing::trace!(
        target: "lucidd::llama_server",
        stream = child_stream,
        line = %sanitized,
        truncated,
        "llama-server output"
    );
}

fn sanitize_bounded_log_text(input: &[u8], max_chars: usize, truncated: bool) -> String {
    const TRUNCATED_SUFFIX: &str = "...[truncated]";
    let suffix_chars = TRUNCATED_SUFFIX.chars().count();
    let content_limit = if truncated {
        max_chars.saturating_sub(suffix_chars)
    } else {
        max_chars
    };
    let decoded = String::from_utf8_lossy(input);
    let mut sanitized = String::with_capacity(input.len().min(max_chars));
    let mut source_chars = decoded.chars();

    for character in source_chars.by_ref().take(content_limit) {
        if character.is_control() {
            sanitized.push('\u{fffd}');
        } else {
            sanitized.push(character);
        }
    }

    let was_truncated = truncated || source_chars.next().is_some();
    if was_truncated && max_chars >= suffix_chars {
        // If the character cap, rather than the byte reader, caused
        // truncation, reserve room for the visible marker.
        while sanitized.chars().count() > max_chars - suffix_chars {
            sanitized.pop();
        }
        sanitized.push_str(TRUNCATED_SUFFIX);
    }
    sanitized
}

// ---------------------------------------------------------------------------
// Inference path
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum BackendBodyReadError {
    TooLarge { limit: usize },
    Transport(String),
}

impl std::fmt::Display for BackendBodyReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(
                    formatter,
                    "backend response body exceeded {limit} byte limit"
                )
            }
            Self::Transport(error) => write!(formatter, "backend response read failed: {error}"),
        }
    }
}

fn extend_limited_body(
    body: &mut BytesMut,
    chunk: &[u8],
    limit: usize,
) -> Result<(), BackendBodyReadError> {
    if chunk.len() > limit.saturating_sub(body.len()) {
        return Err(BackendBodyReadError::TooLarge { limit });
    }
    body.extend_from_slice(chunk);
    Ok(())
}

async fn read_limited_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, BackendBodyReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(BackendBodyReadError::TooLarge { limit });
    }
    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut body = BytesMut::with_capacity(initial_capacity);
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| BackendBodyReadError::Transport(error.to_string()))?;
        extend_limited_body(&mut body, &chunk, limit)?;
    }
    Ok(body.freeze())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SseFrameTooLarge;

impl std::fmt::Display for SseFrameTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "llama-server SSE frame exceeded {MAX_SSE_FRAME_BYTES} byte limit"
        )
    }
}

/// Refuse an oversized first frame before copying a delimiter-free chunk into
/// the accumulation buffer. A subsequent frame in a multi-frame chunk is
/// checked by [`take_next_sse_frame`] after its predecessor is removed.
fn append_sse_chunk(buffer: &mut BytesMut, chunk: &[u8]) -> Result<(), SseFrameTooLarge> {
    let first_boundary = if buffer.last() == Some(&b'\n') && chunk.first() == Some(&b'\n') {
        Some(buffer.len().saturating_sub(1))
    } else {
        find_double_newline(chunk).map(|position| buffer.len().saturating_add(position))
    };
    let accumulated = buffer.len().saturating_add(chunk.len());
    if first_boundary
        .map(|position| position > MAX_SSE_FRAME_BYTES)
        .unwrap_or(accumulated > MAX_SSE_FRAME_BYTES)
    {
        return Err(SseFrameTooLarge);
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

fn take_next_sse_frame(buffer: &mut BytesMut) -> Result<Option<Bytes>, SseFrameTooLarge> {
    match find_double_newline(buffer) {
        Some(position) if position > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        Some(position) => {
            let mut framed = buffer.split_to(position + 2);
            framed.truncate(position);
            Ok(Some(framed.freeze()))
        }
        None if buffer.len() > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        None => Ok(None),
    }
}

/// Build the native llama.cpp completion request from a validated allowlist.
///
/// `SamplingParams` is intentionally extensible at the protocol layer, but the
/// backend boundary must be closed: an unknown key could become meaningful in
/// a future llama.cpp release, and a reserved key could overwrite prompt or
/// resource controls. Server-owned fields are therefore inserted only after
/// every client field has been validated.
#[derive(Debug)]
struct CompletionRequest {
    body: serde_json::Value,
    prompt_chars: u64,
}

fn build_completion_request(inference: &InferenceJobSpec) -> Result<CompletionRequest, String> {
    let prompt = render_prompt(inference);
    let prompt_chars = prompt.chars().count() as u64;
    let mut body = validated_sampling_params(&inference.sampling)?;

    body.insert("prompt".to_string(), serde_json::Value::String(prompt));
    body.insert("stream".to_string(), serde_json::Value::Bool(true));
    body.insert("cache_prompt".to_string(), serde_json::Value::Bool(true));

    // Never omit `n_predict`: llama.cpp may interpret omission as unlimited.
    // Clamp here as defense in depth for callers that invoke the worker
    // directly without passing through the router's operator policy.
    let n_predict = inference
        .max_tokens
        .unwrap_or(DEFAULT_N_PREDICT)
        .clamp(1, MAX_N_PREDICT);
    body.insert("n_predict".to_string(), serde_json::json!(n_predict));

    Ok(CompletionRequest {
        body: serde_json::Value::Object(body),
        prompt_chars,
    })
}

fn validated_sampling_params(
    sampling: &SamplingParams,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut validated = serde_json::Map::new();

    for (key, raw) in &sampling.params {
        if matches!(
            key.as_str(),
            "n_predict" | "prompt" | "messages" | "stream" | "cache_prompt"
        ) {
            return Err(format!("sampling parameter '{key}' is server-owned"));
        }
        if !matches!(
            key.as_str(),
            "temperature" | "top_p" | "top_k" | "min_p" | "repetition_penalty" | "seed" | "stop"
        ) {
            // Do not reflect an attacker-controlled unknown key into API
            // errors or logs. Known keys below are fixed safe literals.
            return Err("unsupported sampling parameter".to_string());
        }

        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| format!("sampling parameter '{key}' is not valid JSON"))?;

        match key.as_str() {
            "temperature" => validate_number_range(key, &value, 0.0, 2.0, true)?,
            "top_p" | "min_p" => validate_number_range(key, &value, 0.0, 1.0, true)?,
            "repetition_penalty" => validate_number_range(key, &value, 0.0, 2.0, false)?,
            "top_k" => validate_unsigned_integer(key, &value, MAX_TOP_K)?,
            "seed" => validate_unsigned_integer(key, &value, MAX_SEED)?,
            "stop" => validate_stop_sequences(&value)?,
            _ => return Err("unsupported sampling parameter".to_string()),
        }

        validated.insert(key.clone(), value);
    }

    Ok(validated)
}

fn validate_number_range(
    key: &str,
    value: &serde_json::Value,
    min: f64,
    max: f64,
    include_min: bool,
) -> Result<(), String> {
    let Some(number) = value.as_f64() else {
        return Err(format!("sampling parameter '{key}' must be a number"));
    };
    let above_min = if include_min {
        number >= min
    } else {
        number > min
    };
    if !number.is_finite() || !above_min || number > max {
        let lower = if include_min {
            "inclusive"
        } else {
            "exclusive"
        };
        return Err(format!(
            "sampling parameter '{key}' must be between {min} ({lower}) and {max} (inclusive)"
        ));
    }
    Ok(())
}

fn validate_unsigned_integer(key: &str, value: &serde_json::Value, max: u64) -> Result<(), String> {
    match value.as_u64() {
        Some(number) if number <= max => Ok(()),
        _ => Err(format!(
            "sampling parameter '{key}' must be an integer between 0 and {max}"
        )),
    }
}

fn validate_stop_sequences(value: &serde_json::Value) -> Result<(), String> {
    let Some(sequences) = value.as_array() else {
        return Err("sampling parameter 'stop' must be an array of strings".to_string());
    };
    if sequences.len() > MAX_STOP_SEQUENCES {
        return Err(format!(
            "sampling parameter 'stop' exceeds {MAX_STOP_SEQUENCES} sequences"
        ));
    }
    if sequences.iter().any(|sequence| {
        sequence
            .as_str()
            .is_none_or(|text| text.chars().count() > MAX_STOP_SEQUENCE_CHARS)
    }) {
        return Err(format!(
            "sampling parameter 'stop' entries must be strings of at most {MAX_STOP_SEQUENCE_CHARS} characters"
        ));
    }
    Ok(())
}

/// Drive a single inference: render the prompt, fire `POST /completion`
/// with `stream: true`, decode SSE frames into [`JobEvent::Output`], and
/// produce a signed receipt at the end.
fn run_inference(
    client: reqwest::Client,
    model: Arc<LoadedModel>,
    request: CompletionRequest,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    identity: NodeIdentity,
    idle_timeout: Duration,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    stream! {
        let started_at = Instant::now();
        let CompletionRequest { body, prompt_chars } = request;
        let url = format!("http://127.0.0.1:{}/completion", model.port);

        let response = client.post(&url).json(&body).send().await;
        let resp = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                let body = match read_limited_body(r, MAX_BACKEND_ERROR_BODY_BYTES).await {
                    Ok(body) => sanitize_bounded_log_text(
                        &body,
                        MAX_BACKEND_ERROR_TEXT_CHARS,
                        false,
                    ),
                    Err(error) => {
                        if matches!(error, BackendBodyReadError::TooLarge { .. }) {
                            // A backend that violates its response envelope is
                            // not safe to reuse. Dropping the response cancels
                            // the request; shutdown also terminates the child.
                            model.shutdown();
                        }
                        yield emit_final_error(
                            &mut producer,
                            &identity,
                            manifest_hash,
                            prompt_chars,
                            0,
                            started_at,
                            format!("llama-server returned {status}; {error}"),
                        );
                        return;
                    }
                };
                yield emit_final_error(
                    &mut producer,
                    &identity,
                    manifest_hash,
                    prompt_chars,
                    0,
                    started_at,
                    format!("llama-server returned {status}: {body}"),
                );
                return;
            }
            Err(e) => {
                yield emit_final_error(
                    &mut producer,
                    &identity,
                    manifest_hash,
                    prompt_chars,
                    0,
                    started_at,
                    format!("request to llama-server failed: {e}"),
                );
                return;
            }
        };

        // The streaming body. We treat it as raw bytes and split on the
        // `\n\n` SSE record boundary ourselves — the body may legitimately
        // contain mid-line JSON that confuses a `lines()`-style splitter.
        let mut bytes = resp.bytes_stream();
        let mut buf = BytesMut::with_capacity(4096);
        let mut acc = CommitmentAccumulator::new();
        let mut seq: u64 = 0;
        let mut completion_tokens: u64 = 0;
        let mut cancelled = false;
        let mut saw_terminal_stop = false;
        let mut final_stop_type: Option<String> = None;

        'outer: loop {
            if producer.is_cancelled() {
                cancelled = true;
                break;
            }
            // Bound the wait so a wedged llama-server doesn't keep us
            // here forever. The combination of `producer.is_cancelled()`
            // and this timeout is what makes hang detection cooperative.
            let next = timeout(idle_timeout, bytes.next()).await;
            let chunk = match next {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(e))) => {
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        format!("SSE stream broke: {e}"),
                        acc.peek(),
                    );
                    return;
                }
                Ok(None) => {
                    let reason = match validate_completion_eof(saw_terminal_stop, &buf) {
                        Ok(()) => break,
                        Err(reason) => reason,
                    };
                    model.shutdown();
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        reason,
                        acc.peek(),
                    );
                    return;
                }
                Err(_elapsed) => {
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        format!("no token within {:?} (hang detected)", idle_timeout),
                        acc.peek(),
                    );
                    // Mark the model suspect so the next request will
                    // re-check health rather than reusing a wedged
                    // subprocess.
                    model
                        .failed_flag
                        .store(true, std::sync::atomic::Ordering::Release);
                    model.failed.notify_waiters();
                    return;
                }
            };
            // A transport chunk may coalesce many SSE frames. Feed it through
            // bounded slices so the accumulation buffer never duplicates an
            // arbitrarily large reqwest chunk even when early delimiters are
            // present.
            for segment in chunk.chunks(SSE_INGEST_SLICE_BYTES) {
                if let Err(error) = append_sse_chunk(&mut buf, segment) {
                    model.shutdown();
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        error.to_string(),
                        acc.peek(),
                    );
                    return;
                }
                loop {
                    let frame = match take_next_sse_frame(&mut buf) {
                        Ok(Some(frame)) => frame,
                        Ok(None) => break,
                        Err(error) => {
                            model.shutdown();
                            yield emit_final_error_with_output(
                                &mut producer,
                                &identity,
                                manifest_hash,
                                prompt_chars,
                                completion_tokens,
                                started_at,
                                error.to_string(),
                                acc.peek(),
                            );
                            return;
                        }
                    };
                    if let Some(json_part) = strip_sse_data_prefix(&frame) {
                        match decode_completion_frame(json_part) {
                            Ok(f) => {
                                if !f.content.is_empty() {
                                    let chunk = OutputChunk {
                                        kind: "token".to_string(),
                                        data: Bytes::copy_from_slice(f.content.as_bytes()),
                                        seq,
                                    };
                                    acc.update(&chunk);
                                    seq += 1;
                                    completion_tokens += 1;
                                    yield JobEvent::Output(chunk);
                                }
                                if f.stop {
                                    saw_terminal_stop = true;
                                    if let Some(st) = f.stop_type {
                                        final_stop_type = Some(st);
                                    }
                                    break 'outer;
                                }
                            }
                            Err(e) => {
                                model.shutdown();
                                yield emit_final_error_with_output(
                                    &mut producer,
                                    &identity,
                                    manifest_hash,
                                    prompt_chars,
                                    completion_tokens,
                                    started_at,
                                    e,
                                    acc.peek(),
                                );
                                return;
                            }
                        }
                    }
                }
            }
        }

        {
            let mut last = model.last_used.lock().await;
            *last = Instant::now();
        }

        let completion = if cancelled {
            Completion::Cancelled
        } else if !saw_terminal_stop {
            // This is unreachable through the transport loop above, but keep
            // the success receipt boundary fail-closed if that loop changes.
            yield emit_final_error_with_output(
                &mut producer,
                &identity,
                manifest_hash,
                prompt_chars,
                completion_tokens,
                started_at,
                "llama-server stream ended without explicit stop:true terminal frame".to_string(),
                acc.peek(),
            );
            return;
        } else {
            match final_stop_type.as_deref() {
                Some("limit") | Some("length") => Completion::Length,
                _ => Completion::Stop,
            }
        };
        let (commitment, count) = acc.finalize();

        let result = JobResult {
            job_spec_hash: manifest_hash,
            output_commitment: commitment,
            output_chunk_count: count,
            completion,
            resumption: None,
            metrics: JobMetrics {
                total_duration_ms: started_at.elapsed().as_millis() as u64,
                prompt_tokens: prompt_chars,
                completion_tokens,
                ..Default::default()
            },
        };

        let receipt = ReceiptBuilder::new(result.clone(), manifest_hash)
            .sign_with(&identity)
            .expect("sign receipt (Serialize impls are infallible)");
        producer.deliver_receipt(receipt);

        yield JobEvent::Final { result, error: None };
    }
}

// ---------------------------------------------------------------------------
// Embedding path
// ---------------------------------------------------------------------------

/// Drive an embedding job: for each input string, `POST /embedding` against
/// the model's (embedding-flavoured) llama-server subprocess, decode the
/// returned vector, and emit it as one `OutputChunk { kind: "embedding" }`
/// per the shared embedding wire convention. The chunk `seq` is the input's
/// index so the HTTP collector can re-order; `data` is the JSON encoding of
/// `Vec<f32>`. Folds every chunk into a `CommitmentAccumulator` so the
/// receipt's `output_commitment` covers exactly what we shipped — the same
/// machinery `run_inference` uses for token streaming.
///
/// `/embedding` is request/response (no SSE), so we cap each call with the
/// per-request timeout rather than the inter-token idle watchdog. On any
/// per-input failure we emit a terminal `Final` with `Completion::Error`
/// rather than panicking, mirroring `run_inference`'s error discipline.
fn run_embedding(
    client: reqwest::Client,
    model: Arc<LoadedModel>,
    embedding: EmbeddingJobSpec,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    identity: NodeIdentity,
    request_timeout: Duration,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    stream! {
        let started_at = Instant::now();
        let url = format!("http://127.0.0.1:{}/embedding", model.port);
        // Sum of input lengths stands in for "prompt tokens" — embeddings
        // have no completion side, so `completion_tokens` stays 0 while the
        // emitted chunk count (one per input) lives in `output_chunk_count`.
        let prompt_chars: u64 = embedding
            .input
            .iter()
            .map(|s| s.chars().count() as u64)
            .sum();

        let mut acc = CommitmentAccumulator::new();
        let mut cancelled = false;

        for (i, text) in embedding.input.iter().enumerate() {
            if producer.is_cancelled() {
                cancelled = true;
                break;
            }

            // llama-server's `/embedding` takes a `content` field. Bound the
            // call with the per-request timeout so a wedged server can't pin
            // the job forever.
            let body = serde_json::json!({ "content": text });
            let response = client
                .post(&url)
                .json(&body)
                .timeout(request_timeout)
                .send()
                .await;
            let resp = match response {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let status = r.status();
                    let body = match read_limited_body(r, MAX_BACKEND_ERROR_BODY_BYTES).await {
                        Ok(body) => sanitize_bounded_log_text(
                            &body,
                            MAX_BACKEND_ERROR_TEXT_CHARS,
                            false,
                        ),
                        Err(error) => {
                            if matches!(error, BackendBodyReadError::TooLarge { .. }) {
                                model.shutdown();
                            }
                            yield emit_final_error_with_output(
                                &mut producer,
                                &identity,
                                manifest_hash,
                                prompt_chars,
                                0,
                                started_at,
                                format!(
                                    "llama-server /embedding returned {status} for input {i}; {error}"
                                ),
                                acc.peek(),
                            );
                            return;
                        }
                    };
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        0,
                        started_at,
                        format!("llama-server /embedding returned {status} for input {i}: {body}"),
                        acc.peek(),
                    );
                    return;
                }
                Err(e) => {
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        0,
                        started_at,
                        format!("request to llama-server /embedding failed for input {i}: {e}"),
                        acc.peek(),
                    );
                    return;
                }
            };

            let raw = match read_limited_body(resp, MAX_EMBEDDING_BODY_BYTES).await {
                Ok(body) => body,
                Err(error) => {
                    if matches!(error, BackendBodyReadError::TooLarge { .. }) {
                        model.shutdown();
                    }
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        0,
                        started_at,
                        format!("reading /embedding body for input {i} failed: {error}"),
                        acc.peek(),
                    );
                    return;
                }
            };

            let vector = match parse_embedding_response(&raw) {
                Some(v) => v,
                None => {
                    yield emit_final_error_with_output(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        0,
                        started_at,
                        format!("could not parse embedding from /embedding response for input {i}"),
                        acc.peek(),
                    );
                    return;
                }
            };

            // Encode the vector exactly as the wire convention requires:
            // `serde_json::to_vec(&Vec<f32>)`. The HTTP collector decodes
            // each chunk with the symmetric `from_slice::<Vec<f32>>`.
            let data = serde_json::to_vec(&vector)
                .expect("Vec<f32> serializes to JSON infallibly");
            let chunk = OutputChunk {
                kind: "embedding".to_string(),
                data: Bytes::from(data),
                seq: i as u64,
            };
            acc.update(&chunk);
            yield JobEvent::Output(chunk);
        }

        {
            let mut last = model.last_used.lock().await;
            *last = Instant::now();
        }

        let (commitment, count) = acc.finalize();
        let completion = if cancelled {
            Completion::Cancelled
        } else {
            Completion::Stop
        };

        let result = JobResult {
            job_spec_hash: manifest_hash,
            output_commitment: commitment,
            output_chunk_count: count,
            completion,
            resumption: None,
            metrics: JobMetrics {
                total_duration_ms: started_at.elapsed().as_millis() as u64,
                prompt_tokens: prompt_chars,
                completion_tokens: 0,
                ..Default::default()
            },
        };

        let receipt = ReceiptBuilder::new(result.clone(), manifest_hash)
            .sign_with(&identity)
            .expect("sign receipt (Serialize impls are infallible)");
        producer.deliver_receipt(receipt);

        yield JobEvent::Final { result, error: None };
    }
}

/// Extract a single embedding vector from a `/embedding` response body.
///
/// The endpoint's JSON shape varies across llama-server versions, so we
/// accept the three we've observed in the wild and pick the first vector:
///
/// 1. A top-level object: `{"embedding": [..]}`.
/// 2. An array of per-input objects: `[{"embedding": [..], "index": 0}, ..]`
///    (what newer servers return even for a single `content`).
/// 3. An OpenAI-flavoured wrapper: `{"data": [{"embedding": [..]}, ..]}`.
///
/// Returns `None` if none of these yield a numeric array — the caller turns
/// that into a `Completion::Error` rather than guessing.
fn parse_embedding_response(raw: &[u8]) -> Option<Vec<f32>> {
    let value: serde_json::Value = serde_json::from_slice(raw).ok()?;
    // Shape 1: {"embedding": [...]}.
    if let Some(v) = value.get("embedding").and_then(json_to_vec_f32) {
        return Some(v);
    }
    // Shape 3: {"data": [{"embedding": [...]}, ...]} — take the first entry.
    if let Some(first) = value
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
    {
        if let Some(v) = first.get("embedding").and_then(json_to_vec_f32) {
            return Some(v);
        }
    }
    // Shape 2: [{"embedding": [...], "index": 0}, ...] — take the first entry.
    if let Some(first) = value.as_array().and_then(|a| a.first()) {
        if let Some(v) = first.get("embedding").and_then(json_to_vec_f32) {
            return Some(v);
        }
    }
    None
}

/// Decode a JSON value that should be an embedding vector into `Vec<f32>`.
///
/// Handles both shapes llama.cpp emits: the OpenAI `/v1/embeddings` form is a
/// flat `[f, f, ...]`, while the native `/embedding` endpoint wraps the
/// (mean-pooled) vector in an OUTER array — `[[f, f, ...]]` — even for a single
/// `content`. We descend one level when the first element is itself an array
/// (taking row 0, the pooled vector). Returns `None` for an empty or
/// non-numeric array.
fn json_to_vec_f32(value: &serde_json::Value) -> Option<Vec<f32>> {
    let arr = value.as_array()?;
    let row: &[serde_json::Value] = match arr.first() {
        // Native /embedding: `[[...]]` — descend into the first (pooled) row.
        Some(v) if v.is_array() => v.as_array()?,
        // Flat `[...]` — already a vector.
        Some(_) => arr,
        // Empty array carries no vector.
        None => return None,
    };
    row.iter()
        .map(|n| n.as_f64().map(|f| f as f32))
        .collect::<Option<Vec<f32>>>()
}

/// Build the terminal `JobEvent::Final` for an error path and deliver the
/// signed receipt out-of-band. Pure side-effecting helper so the happy
/// and sad paths in `run_inference` look the same.
fn emit_final_error(
    producer: &mut JobHandleProducer,
    identity: &NodeIdentity,
    manifest_hash: [u8; 32],
    prompt_tokens: u64,
    completion_tokens: u64,
    started_at: Instant,
    error: String,
) -> JobEvent {
    emit_final_error_with_output(
        producer,
        identity,
        manifest_hash,
        prompt_tokens,
        completion_tokens,
        started_at,
        error,
        CommitmentAccumulator::new().finalize(),
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_final_error_with_output(
    producer: &mut JobHandleProducer,
    identity: &NodeIdentity,
    manifest_hash: [u8; 32],
    prompt_tokens: u64,
    completion_tokens: u64,
    started_at: Instant,
    error: String,
    (commitment, count): ([u8; 32], u64),
) -> JobEvent {
    let result = JobResult {
        job_spec_hash: manifest_hash,
        output_commitment: commitment,
        output_chunk_count: count,
        completion: Completion::Error,
        resumption: None,
        metrics: JobMetrics {
            total_duration_ms: started_at.elapsed().as_millis() as u64,
            prompt_tokens,
            completion_tokens,
            ..Default::default()
        },
    };
    if let Ok(receipt) = ReceiptBuilder::new(result.clone(), manifest_hash).sign_with(identity) {
        producer.deliver_receipt(receipt);
    }
    JobEvent::Final {
        result,
        error: Some(error),
    }
}

/// One streamed frame on `POST /completion`. We only care about three
/// fields — anything else (timings, generation_settings, etc.) we skip.
#[derive(Debug, Deserialize)]
struct CompletionFrame {
    #[serde(default)]
    content: String,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    stop_type: Option<String>,
}

fn decode_completion_frame(json: &[u8]) -> Result<CompletionFrame, String> {
    serde_json::from_slice(json)
        .map_err(|error| format!("malformed llama-server SSE data frame: {error}"))
}

fn validate_completion_eof(saw_terminal_stop: bool, buffered: &[u8]) -> Result<(), String> {
    if saw_terminal_stop {
        return Ok(());
    }
    if buffered.is_empty() {
        Err("llama-server SSE stream ended without explicit stop:true terminal frame".to_string())
    } else {
        Err(format!(
            "llama-server SSE stream ended with {} unterminated byte(s) and no stop:true terminal frame",
            buffered.len()
        ))
    }
}

/// Find the first `\n\n` separator in a buffer (the SSE record boundary).
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

/// Strip the `data: ` SSE prefix. Returns `None` for non-data frames
/// (comments, retries, named events) — we don't care about any of them.
fn strip_sse_data_prefix(frame: &[u8]) -> Option<&[u8]> {
    // Skip leading whitespace / stray CR.
    let mut start = 0;
    while start < frame.len() && (frame[start] == b'\r' || frame[start] == b'\n') {
        start += 1;
    }
    let body = &frame[start..];
    let prefix = b"data: ";
    if body.starts_with(prefix) {
        Some(&body[prefix.len()..])
    } else if body.starts_with(b"data:") {
        // Some servers omit the space after the colon.
        Some(&body[b"data:".len()..])
    } else {
        None
    }
}

/// Render an `InferenceJobSpec` to a single prompt string suitable for
/// the native `/completion` endpoint. Keeps the chat-template rendering
/// client-side (see module-level docs).
///
/// Format mirrors the conservative Alpaca-style framing that most open
/// chat models still understand even without `--jinja` doing the heavy
/// lifting:
///
/// ```text
/// <|system|>You are helpful.
/// <|user|>Hello.
/// <|assistant|>
/// ```
fn render_prompt(spec: &InferenceJobSpec) -> String {
    if spec.messages.is_empty() {
        return spec.prompt.clone().unwrap_or_default();
    }
    let mut out = String::new();
    for msg in &spec.messages {
        out.push_str(role_tag(&msg.role));
        out.push_str(&msg.content);
        out.push('\n');
    }
    out.push_str("<|assistant|>\n");
    out
}

fn role_tag(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "<|system|>",
        ChatRole::User => "<|user|>",
        ChatRole::Assistant => "<|assistant|>",
        ChatRole::Tool => "<|tool|>",
    }
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

impl Drop for Inner {
    fn drop(&mut self) {
        // Cancel all supervisors. `kill_on_drop(true)` on the child
        // commands handles the actual subprocess termination, but
        // aborting the supervisor task suppresses log noise from the
        // about-to-be-killed children.
        for entry in self.loaded_models.iter() {
            entry.value().supervisor.abort();
            entry
                .value()
                .failed_flag
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

// We never let `ChatMessage`'s images field bleed into the prompt — text
// only for the native `/completion` path. Multimodal would route through
// `/v1/chat/completions` with explicit `multimodal_data` once we wire
// vision models in a later milestone.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{ChatMessage, SignedReceipt};

    async fn exercise_raw_sse(body: &'static [u8]) -> (Vec<JobEvent>, SignedReceipt<JobResult>) {
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test backend");
        let port = listener.local_addr().expect("test backend address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(headers.as_bytes())
                .await
                .expect("write response headers");
            socket.write_all(body).await.expect("write SSE body");
            socket.shutdown().await.expect("close response");
        });

        let failed = Arc::new(Notify::new());
        let failed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let model = Arc::new(LoadedModel {
            port,
            last_used: Mutex::new(Instant::now()),
            failed,
            failed_flag,
            supervisor: tokio::spawn(std::future::pending::<()>()),
        });
        let manifest_hash = [0x61; 32];
        let (handle, producer) = JobHandle::new(JobId(manifest_hash));
        let events = run_inference(
            reqwest::Client::new(),
            model,
            CompletionRequest {
                body: serde_json::json!({"prompt": "test", "stream": true}),
                prompt_chars: 4,
            },
            manifest_hash,
            producer,
            NodeIdentity::generate(),
            Duration::from_secs(1),
        )
        .collect::<Vec<_>>()
        .await;
        let receipt = handle.finish().await.expect("signed failure receipt");
        server.await.expect("test backend exits");
        (events, receipt)
    }

    fn inference_with_sampling(
        entries: &[(&str, &str)],
        max_tokens: Option<u32>,
    ) -> InferenceJobSpec {
        let mut sampling = SamplingParams::default();
        for (key, value) in entries {
            sampling
                .params
                .insert((*key).to_string(), (*value).to_string());
        }
        InferenceJobSpec {
            model_cid: "test-model".to_string(),
            messages: Vec::new(),
            prompt: Some("trusted prompt".to_string()),
            resume_from: None,
            sampling,
            max_tokens,
            stream: true,
        }
    }

    #[test]
    fn completion_request_rejects_server_owned_sampling_fields() {
        for key in ["n_predict", "prompt", "messages", "stream", "cache_prompt"] {
            let spec = inference_with_sampling(&[(key, "1")], Some(32));
            let error = build_completion_request(&spec).expect_err("reserved key must fail");
            assert!(
                error.contains("server-owned"),
                "unexpected error for {key}: {error}"
            );
        }
    }

    #[test]
    fn completion_request_always_sets_bounded_server_n_predict() {
        let absent = inference_with_sampling(&[], None);
        let request = build_completion_request(&absent).expect("default request");
        assert_eq!(
            request.body["n_predict"],
            serde_json::json!(DEFAULT_N_PREDICT)
        );

        let oversized = inference_with_sampling(&[], Some(u32::MAX));
        let request = build_completion_request(&oversized).expect("clamped request");
        assert_eq!(request.body["n_predict"], serde_json::json!(MAX_N_PREDICT));

        let zero = inference_with_sampling(&[], Some(0));
        let request = build_completion_request(&zero).expect("minimum request");
        assert_eq!(request.body["n_predict"], serde_json::json!(1));
    }

    #[test]
    fn completion_request_rejects_negative_oversized_and_unknown_sampling() {
        for (key, value) in [
            ("temperature", "-0.1"),
            ("temperature", "2.1"),
            ("top_p", "1.1"),
            ("min_p", "-0.1"),
            ("repetition_penalty", "0"),
            ("top_k", "-1"),
            ("top_k", "1001"),
            ("seed", "-1"),
            ("seed", "2147483648"),
            ("temperature", r#""0.7""#),
            ("top_k", "1.5"),
            ("stop", r#""END""#),
            ("future_llama_flag", "true"),
        ] {
            let spec = inference_with_sampling(&[(key, value)], Some(32));
            assert!(
                build_completion_request(&spec).is_err(),
                "expected {key}={value} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn execute_rejects_reserved_sampling_before_model_load() {
        let spec = inference_with_sampling(&[("n_predict", "-1")], Some(u32::MAX));
        let manifest = ManifestBuilder::new(JobSpec::Inference(spec))
            .sign_with(&NodeIdentity::generate())
            .expect("sign manifest");
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

        let error = match worker.execute(manifest).await {
            Ok(_) => panic!("reserved sampling key must fail dispatch"),
            Err(error) => error,
        };
        assert!(matches!(error, WorkerError::BadManifest(_)));
    }

    #[tokio::test]
    async fn execute_rejects_multimodal_images_before_model_load() {
        let mut spec = inference_with_sampling(&[], Some(32));
        spec.messages.push(ChatMessage {
            role: ChatRole::User,
            content: "describe this".to_string(),
            images: vec!["not-decoded-or-forwarded".to_string()],
        });
        let manifest = ManifestBuilder::new(JobSpec::Inference(spec))
            .sign_with(&NodeIdentity::generate())
            .expect("sign manifest");
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

        let error = match worker.execute(manifest).await {
            Ok(_) => panic!("multimodal input must fail before model lookup"),
            Err(error) => error,
        };
        assert!(
            matches!(error, WorkerError::BadManifest(message) if message.contains("multimodal"))
        );
    }

    #[tokio::test]
    async fn embedding_bounds_fail_before_model_load() {
        let identity = NodeIdentity::generate();
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
        for input in [
            Vec::new(),
            vec![String::new()],
            vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS + 1)],
            vec!["x".to_string(); MAX_EMBEDDING_INPUTS + 1],
            vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS); 5],
        ] {
            let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
                model_cid: "does-not-load".to_string(),
                input,
            }))
            .sign_with(&identity)
            .expect("sign manifest");
            let error = match worker.execute(manifest).await {
                Ok(_) => panic!("invalid embedding must fail before model lookup"),
                Err(error) => error,
            };
            assert!(matches!(error, WorkerError::BadManifest(_)));
        }

        assert!(validate_embedding_spec(&EmbeddingJobSpec {
            model_cid: "cid".to_string(),
            input: vec!["ok".to_string()],
        })
        .is_ok());
    }

    #[tokio::test]
    async fn occupied_loopback_port_is_never_reserved_for_llama() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("occupy loopback port");
        let port = listener.local_addr().expect("occupied address").port();
        assert!(port < u16::MAX, "ephemeral port must have a successor");
        let worker = LlamaCppWorker::new(
            NodeIdentity::generate(),
            LlamaCppConfig {
                server_port_range: port..port + 1,
                ..LlamaCppConfig::default()
            },
        );

        assert!(matches!(
            worker.allocate_port().await,
            Err(WorkerError::Capacity)
        ));
        assert!(worker.inner.ports_in_use.lock().await.is_empty());
    }

    #[tokio::test]
    async fn loopback_client_refuses_backend_redirects() {
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect server");
        let port = listener.local_addr().expect("redirect address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1/exfiltrate\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write redirect");
        });
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

        let response = worker
            .inner
            .client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .expect("receive redirect response");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.await.expect("redirect server exits");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn loopback_client_ignores_environment_proxy() {
        use tokio::io::AsyncWriteExt as _;

        const CHILD_MARKER: &str = "LUCID_LLAMA_NO_PROXY_CHILD";
        const TARGET_URL: &str = "LUCID_LLAMA_NO_PROXY_TARGET";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
            let response = worker
                .inner
                .client
                .get(std::env::var(TARGET_URL).expect("child target URL"))
                .send()
                .await
                .expect("direct loopback request");
            assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
            return;
        }

        let target = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind direct target");
        let target_port = target.local_addr().expect("target address").port();
        let target_task = tokio::spawn(async move {
            let (mut socket, _) = target.accept().await.expect("accept direct request");
            let mut request = [0_u8; 1024];
            let _ = socket
                .read(&mut request)
                .await
                .expect("read direct request");
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write direct response");
        });
        let hostile_proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hostile proxy");
        let proxy_port = hostile_proxy.local_addr().expect("proxy address").port();
        let proxy_url = format!("http://127.0.0.1:{proxy_port}");
        let test_name = "worker_llama::tests::loopback_client_ignores_environment_proxy";
        let output =
            tokio::process::Command::new(std::env::current_exe().expect("current test executable"))
                .arg("--exact")
                .arg(test_name)
                .arg("--nocapture")
                .env(CHILD_MARKER, "1")
                .env(TARGET_URL, format!("http://127.0.0.1:{target_port}/health"))
                .env("HTTP_PROXY", &proxy_url)
                .env("http_proxy", &proxy_url)
                .env("ALL_PROXY", &proxy_url)
                .env("all_proxy", &proxy_url)
                .env("NO_PROXY", "")
                .env("no_proxy", "")
                .output()
                .await
                .expect("run isolated proxy child test");
        assert!(
            output.status.success(),
            "proxy child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::time::timeout(Duration::from_secs(2), target_task)
            .await
            .expect("client bypasses proxy and reaches target")
            .expect("target task exits");
        drop(hostile_proxy);
    }

    #[tokio::test]
    async fn concurrent_cold_loads_are_serialized_before_capacity_mutation() {
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
        let guard = worker.inner.load_gate.lock().await;
        let contender = worker.clone();
        let task =
            tokio::spawn(async move { contender.ensure_loaded("missing-model", false).await });

        assert!(tokio::time::timeout(Duration::from_millis(25), task)
            .await
            .is_err());
        drop(guard);

        // The timed-out JoinHandle was dropped, not the underlying task. A
        // second direct call proves the gate is usable and fails at the model
        // boundary rather than leaking a port reservation.
        assert!(matches!(
            worker.ensure_loaded("missing-model", false).await,
            Err(WorkerError::ArtifactUnavailable(_))
        ));
        assert!(worker.inner.ports_in_use.lock().await.is_empty());
    }

    #[test]
    fn completion_request_rejects_oversized_stop_sequences() {
        let too_many = serde_json::to_string(&vec!["x"; MAX_STOP_SEQUENCES + 1]).unwrap();
        let spec = inference_with_sampling(&[("stop", &too_many)], Some(32));
        assert!(build_completion_request(&spec).is_err());

        let too_long = "x".repeat(MAX_STOP_SEQUENCE_CHARS + 1);
        let encoded = serde_json::to_string(&vec![too_long]).unwrap();
        let spec = inference_with_sampling(&[("stop", &encoded)], Some(32));
        assert!(build_completion_request(&spec).is_err());
    }

    #[test]
    fn completion_request_forwards_only_validated_sampling() {
        let spec = inference_with_sampling(
            &[
                ("temperature", "0.7"),
                ("top_p", "0.9"),
                ("top_k", "40"),
                ("min_p", "0.05"),
                ("repetition_penalty", "1.1"),
                ("seed", "42"),
                ("stop", r#"["END","STOP"]"#),
            ],
            Some(64),
        );
        let request = build_completion_request(&spec).expect("valid request");
        let body = request.body;

        assert_eq!(body["prompt"], serde_json::json!("trusted prompt"));
        assert_eq!(body["stream"], serde_json::json!(true));
        assert_eq!(body["cache_prompt"], serde_json::json!(true));
        assert_eq!(body["n_predict"], serde_json::json!(64));
        assert_eq!(body["temperature"], serde_json::json!(0.7));
        assert_eq!(body["top_p"], serde_json::json!(0.9));
        assert_eq!(body["top_k"], serde_json::json!(40));
        assert_eq!(body["min_p"], serde_json::json!(0.05));
        assert_eq!(body["repetition_penalty"], serde_json::json!(1.1));
        assert_eq!(body["seed"], serde_json::json!(42));
        assert_eq!(body["stop"], serde_json::json!(["END", "STOP"]));
        assert_eq!(request.prompt_chars, 14);
    }

    #[test]
    fn resolve_model_path_accepts_real_file_in_dir() {
        // SEC-04: a legitimate id resolves to `model_dir/<id>.gguf`.
        let dir = tempfile::tempdir().expect("tempdir");
        let model = dir.path().join("qwen3.gguf");
        std::fs::write(&model, b"gguf").expect("touch model");
        let resolved = resolve_model_path(dir.path(), "qwen3").expect("resolve ok");
        // Canonicalized on both sides — compare against the canonical file.
        assert_eq!(resolved, model.canonicalize().unwrap());
        assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[test]
    fn resolve_model_path_rejects_traversal_and_separators() {
        // SEC-04: traversal / separators / absolute ids never resolve.
        let dir = tempfile::tempdir().expect("tempdir");
        for bad in [
            "../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "/etc/passwd",
            "/tmp/x.gguf",
        ] {
            assert!(
                resolve_model_path(dir.path(), bad).is_err(),
                "expected reject for {bad:?}"
            );
        }
    }

    #[test]
    fn resolve_model_path_rejects_leading_dash_empty_and_nul() {
        // SEC-04: leading `-` (arg injection), empty, and embedded NUL.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(resolve_model_path(dir.path(), "--flag").is_err());
        assert!(resolve_model_path(dir.path(), "-rf").is_err());
        assert!(resolve_model_path(dir.path(), "").is_err());
        assert!(resolve_model_path(dir.path(), "x\0y").is_err());
    }

    #[test]
    fn resolve_model_path_oracle_closed_same_client_error() {
        // SEC-04: a not-found id and a parse-/shape-rejected id collapse to
        // the SAME generic client-facing error string. `ensure_loaded` maps
        // both Err reasons to `ArtifactUnavailable("model unavailable")`.
        let dir = tempfile::tempdir().expect("tempdir");
        // Both of these are `Err(_)` at the resolver; the caller maps them
        // to one constant string, so the client cannot distinguish them.
        let not_found = resolve_model_path(dir.path(), "definitely-not-here");
        let bad_shape = resolve_model_path(dir.path(), "../escape");
        assert!(not_found.is_err());
        assert!(bad_shape.is_err());
        let client_err =
            |_: String| WorkerError::ArtifactUnavailable("model unavailable".into()).to_string();
        assert_eq!(
            client_err(not_found.unwrap_err()),
            client_err(bad_shape.unwrap_err())
        );
    }

    #[test]
    fn strip_sse_data_prefix_handles_space() {
        let frame = b"data: {\"x\":1}";
        assert_eq!(strip_sse_data_prefix(frame), Some(&b"{\"x\":1}"[..]));
    }

    #[test]
    fn strip_sse_data_prefix_handles_no_space() {
        let frame = b"data:{\"x\":1}";
        assert_eq!(strip_sse_data_prefix(frame), Some(&b"{\"x\":1}"[..]));
    }

    #[test]
    fn strip_sse_data_prefix_skips_non_data() {
        let frame = b": comment";
        assert_eq!(strip_sse_data_prefix(frame), None);
    }

    #[test]
    fn malformed_sse_data_frame_is_a_terminal_protocol_error() {
        let error = decode_completion_frame(br#"{"content":"partial","stop":tru}"#)
            .expect_err("malformed JSON must never be skipped");
        assert!(error.contains("malformed llama-server SSE data frame"));
    }

    #[test]
    fn eof_without_explicit_stop_frame_can_never_be_success() {
        assert_eq!(validate_completion_eof(true, b""), Ok(()));

        let clean_eof = validate_completion_eof(false, b"")
            .expect_err("clean transport EOF is not a model terminal event");
        assert!(clean_eof.contains("without explicit stop:true"));

        let partial_eof = validate_completion_eof(false, br#"data: {"content":"partial"}"#)
            .expect_err("unterminated frame must fail closed");
        assert!(partial_eof.contains("unterminated byte(s)"));
        assert!(partial_eof.contains("no stop:true terminal frame"));
    }

    #[tokio::test]
    async fn backend_eof_without_stop_emits_only_signed_error_terminal() {
        let (events, receipt) =
            exercise_raw_sse(b"data: {\"content\":\"partial\",\"stop\":false}\n\n").await;
        assert!(matches!(events.first(), Some(JobEvent::Output(_))));
        assert!(matches!(
            events.last(),
            Some(JobEvent::Final {
                result: JobResult {
                    completion: Completion::Error,
                    ..
                },
                error: Some(error),
            }) if error.contains("without explicit stop:true")
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            JobEvent::Final {
                result: JobResult {
                    completion: Completion::Stop | Completion::Length,
                    ..
                },
                ..
            }
        )));
        assert_eq!(receipt.result.completion, Completion::Error);
        let mut replay = CommitmentAccumulator::new();
        for event in &events {
            if let JobEvent::Output(chunk) = event {
                replay.update(chunk);
            }
        }
        let (commitment, count) = replay.finalize();
        assert_eq!(receipt.result.output_commitment, commitment);
        assert_eq!(receipt.result.output_chunk_count, count);
    }

    #[tokio::test]
    async fn malformed_backend_sse_emits_only_signed_error_terminal() {
        let (events, receipt) =
            exercise_raw_sse(b"data: {\"content\":\"partial\",\"stop\":tru}\n\n").await;
        assert!(matches!(
            events.as_slice(),
            [JobEvent::Final {
                result: JobResult {
                    completion: Completion::Error,
                    ..
                },
                error: Some(error),
            }] if error.contains("malformed llama-server SSE data frame")
        ));
        assert_eq!(receipt.result.completion, Completion::Error);
    }

    #[test]
    fn find_double_newline_finds_first() {
        let buf = b"hello\n\nworld\n\nbye";
        assert_eq!(find_double_newline(buf), Some(5));
    }

    #[test]
    fn delimiter_free_oversized_sse_is_rejected_before_buffer_growth() {
        let existing = vec![b'x'; MAX_SSE_FRAME_BYTES];
        let mut buffer = BytesMut::from(existing.as_slice());

        assert_eq!(append_sse_chunk(&mut buffer, b"x"), Err(SseFrameTooLarge));
        assert_eq!(buffer.len(), MAX_SSE_FRAME_BYTES);

        let mut oversized_frame = vec![b'x'; MAX_SSE_FRAME_BYTES + 1];
        oversized_frame.extend_from_slice(b"\n\n");
        let mut empty = BytesMut::new();
        assert_eq!(
            append_sse_chunk(&mut empty, &oversized_frame),
            Err(SseFrameTooLarge)
        );
        assert!(empty.is_empty());
    }

    #[test]
    fn limited_body_rejects_oversized_chunks_without_partial_append() {
        let mut body = BytesMut::new();
        extend_limited_body(&mut body, b"12345678", 8).expect("exact limit is accepted");
        assert_eq!(&body[..], b"12345678");

        assert_eq!(
            extend_limited_body(&mut body, b"9", 8),
            Err(BackendBodyReadError::TooLarge { limit: 8 })
        );
        assert_eq!(&body[..], b"12345678");
    }

    #[test]
    fn child_log_text_is_control_safe_and_character_bounded() {
        let mut hostile = b"prefix\r\n\x1b[31m\0".to_vec();
        hostile.extend(std::iter::repeat_n(b'x', 128));

        let sanitized = sanitize_bounded_log_text(&hostile, 32, false);
        assert!(sanitized.chars().count() <= 32);
        assert!(sanitized.chars().all(|character| !character.is_control()));
        assert!(sanitized.ends_with("...[truncated]"));
        assert!(!sanitized.contains('\u{1b}'));
        assert!(!sanitized.contains('\n'));

        let explicitly_truncated = sanitize_bounded_log_text(b"short", 32, true);
        assert!(explicitly_truncated.ends_with("...[truncated]"));
    }

    #[test]
    fn parse_embedding_accepts_top_level_object() {
        let raw = br#"{"embedding":[0.1,0.2,0.3]}"#;
        let v = parse_embedding_response(raw).expect("parse");
        assert_eq!(v, vec![0.1f32, 0.2, 0.3]);
    }

    #[test]
    fn parse_embedding_accepts_array_of_objects() {
        let raw = br#"[{"embedding":[1.0,2.0],"index":0}]"#;
        let v = parse_embedding_response(raw).expect("parse");
        assert_eq!(v, vec![1.0f32, 2.0]);
    }

    #[test]
    fn parse_embedding_accepts_data_wrapper() {
        let raw = br#"{"data":[{"embedding":[4.0,5.0,6.0]}]}"#;
        let v = parse_embedding_response(raw).expect("parse");
        assert_eq!(v, vec![4.0f32, 5.0, 6.0]);
    }

    #[test]
    fn parse_embedding_accepts_nested_array_of_objects() {
        // The REAL llama.cpp native /embedding endpoint wraps the pooled
        // vector in an OUTER array: `[{"index":0,"embedding":[[...]]}]`. The
        // Earlier fixture coverage used a flat array and missed this; a live
        // nomic-embed run returned zero vectors until the parser descended one
        // level. Take row 0 (the pooled vector).
        let raw = br#"[{"index":0,"embedding":[[7.0,8.0,9.0]]}]"#;
        let v = parse_embedding_response(raw).expect("parse");
        assert_eq!(v, vec![7.0f32, 8.0, 9.0]);
    }

    #[test]
    fn parse_embedding_accepts_nested_top_level_object() {
        // Same nesting under the top-level `{"embedding":[[...]]}` shape.
        let raw = br#"{"embedding":[[1.5,2.5]]}"#;
        let v = parse_embedding_response(raw).expect("parse");
        assert_eq!(v, vec![1.5f32, 2.5]);
    }

    #[test]
    fn parse_embedding_rejects_non_numeric_and_missing() {
        assert!(parse_embedding_response(br#"{"nope":1}"#).is_none());
        assert!(parse_embedding_response(br#"{"embedding":["x"]}"#).is_none());
        assert!(parse_embedding_response(br#"{"embedding":[]}"#).is_none());
        assert!(parse_embedding_response(b"not json").is_none());
    }

    #[test]
    fn render_prompt_empty_messages_uses_prompt() {
        let spec = InferenceJobSpec {
            model_cid: "x".to_string(),
            messages: vec![],
            prompt: Some("Hello.".to_string()),
            resume_from: None,
            sampling: Default::default(),
            max_tokens: None,
            stream: true,
        };
        assert_eq!(render_prompt(&spec), "Hello.");
    }

    #[test]
    fn render_prompt_renders_chat_roles() {
        let spec = InferenceJobSpec {
            model_cid: "x".to_string(),
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Be helpful.".to_string(),
                    images: vec![],
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: "Hi.".to_string(),
                    images: vec![],
                },
            ],
            prompt: None,
            resume_from: None,
            sampling: Default::default(),
            max_tokens: None,
            stream: true,
        };
        let rendered = render_prompt(&spec);
        assert!(rendered.contains("<|system|>Be helpful."));
        assert!(rendered.contains("<|user|>Hi."));
        assert!(rendered.ends_with("<|assistant|>\n"));
    }
}
