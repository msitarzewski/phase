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
//! Per-request hang detection lives in [`run_inference`]: if no SSE frame
//! arrives for 30 s the request is aborted and the model's supervisor is
//! sent an explicit kill signal, so the wedged `llama-server` is SIGKILLed,
//! reaped, and its port returned to the pool instead of silently pinning
//! VRAM/RAM until the host runs out of memory.
//!
//! Port reservations are RAII ([`PortGuard`]): whichever task currently
//! owns a spawned (or spawning) child also owns its port, so a failed or
//! cancelled cold load can never leak a port from `server_port_range`.
//! LRU eviction of a warm model is deferred until the replacement cold load
//! has passed its `/health` gate, so a failed or cancelled cold load never
//! evicts a healthy warm model.
//!
//! ## Loopback lockdown
//!
//! Loopback is not an authentication boundary: any local process — and any
//! web page via DNS rebinding or `fetch()` to `127.0.0.1` — can reach the
//! child's port. Every spawn therefore gets a fresh 256-bit API key passed
//! through the `LLAMA_API_KEY` environment variable (never argv, which is
//! world-readable through `ps`), every authenticated request carries
//! `Authorization: Bearer <key>`, and the web UI, `/slots`, and credentialed
//! CORS are disabled.
//!
//! ## Why `POST /completion` rather than `/v1/chat/completions`?
//!
//! Both stream; both work. The native `/completion` endpoint has a simpler
//! frame shape (`{"content": "...", "stop": bool}`) that we don't have to
//! reassemble from `delta.content` like the OpenAI flavour, and it doesn't
//! emit the `data: [DONE]` sentinel — easier to parse correctly with the
//! tiny SSE splitter below. Chat-template formatting is handled
//! by the owned server through `/apply-template`, using the loaded GGUF
//! metadata and `--jinja`, before the same native completion stream starts.
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
use std::time::{Duration, Instant, SystemTime};

use async_stream::stream;
use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures::StreamExt;
use phase_identity::NodeIdentity;
use phase_protocol::{
    CommitmentAccumulator, Completion, EmbeddingJobSpec, InferenceJobSpec, JobEvent, JobHandle,
    JobHandleProducer, JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream, OutputChunk,
    SamplingParams, SignedManifest, Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex, Notify};
use tokio::time::timeout;

/// Environment variable `llama-server` reads its API key(s) from. Passing the
/// key through the environment rather than `--api-key` keeps it out of the
/// world-readable process argument list.
const LLAMA_API_KEY_ENV: &str = "LLAMA_API_KEY";

/// Hardening flags passed to every `llama-server` spawn (verified against the
/// pinned `b12651` CLI). `--cors-origins localhost` is not a valid browser
/// origin (`scheme://host[:port]`), so no page origin can read responses;
/// credentialed CORS, the bundled web UI, and the `/slots` monitoring
/// endpoint are disabled outright.
const LLAMA_SERVER_HARDENING_ARGS: [&str; 5] = [
    "--cors-origins",
    "localhost",
    "--no-cors-credentials",
    "--no-webui",
    "--no-slots",
];

/// Bounded wait for a killed supervisor to reap its child and release the
/// port before the slot is reused. Kill + reap is normally milliseconds.
const SUPERVISOR_STOP_TIMEOUT: Duration = Duration::from_secs(5);

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

use crate::backend_io::{
    append_sse_chunk, read_limited_body, take_next_sse_frame, BackendBodyReadError,
};
#[cfg(test)]
use crate::backend_io::{extend_limited_body, SseFrameTooLarge, MAX_SSE_FRAME_BYTES};

/// Native llama.cpp responses cross a process boundary and are untrusted even
/// though the HTTP socket is loopback-only.
const SSE_INGEST_SLICE_BYTES: usize = 4 * 1024;
const MAX_BACKEND_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_BACKEND_ERROR_TEXT_CHARS: usize = 1_024;
const MAX_EMBEDDING_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CHAT_TEMPLATE_BODY_BYTES: usize = 4 * 1024 * 1024;
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
    ///
    /// Because warm-model eviction is deferred until a replacement cold load
    /// is healthy, a full worker briefly holds `max_loaded_models + 1`
    /// children; the range must contain at least that many ports or a cold
    /// load at cap returns [`WorkerError::Capacity`].
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

    /// Optional loopback-only (`127.0.0.1:<PORT>` / `[::1]:<PORT>`) `ggml-rpc-server`
    /// tunnel endpoints for Tier-1 Trusted-Circle layer slicing (`--rpc`).
    /// Non-loopback addresses are rejected defense-in-depth because `ggml-rpc`
    /// has no native auth/TLS and must always traverse an encrypted tunnel.
    pub rpc_servers: Vec<String>,

    /// Optional layer offload to enrolled peers over the Noise-encrypted
    /// `/phase/shard-rpc/1.0.0` tunnel. Every spawn binds its own loopback
    /// proxy per peer and appends those endpoints to `--rpc` after
    /// `rpc_servers`; see [`rpc_peers`].
    pub rpc_peer_offload: Option<rpc_peers::RpcPeerOffload>,

    /// Optional comma-separated GPU layer split ratios (`--tensor-split`, e.g. `"30,70"`).
    /// Only meaningful together with `rpc_servers`.
    pub tensor_split: Option<String>,

    /// Runtime opt-in check for Tier-1 Trusted-Circle RPC layer offload.
    /// Consulted on every job (and before every supervisor respawn) whenever
    /// `rpc_servers` is non-empty. `None` fails closed: an RPC-configured
    /// worker without a gate refuses every job.
    pub rpc_cluster_gate: Option<RpcClusterGate>,
}

/// Runtime gate for Tier-1 Trusted-Circle `ggml-rpc` layer offload. Layer
/// offload ships activations — derived from the prompt — to other machines,
/// so it must stay behind the operator's explicit `cluster_enabled = true`
/// opt-in, which can be toggled at runtime from `node.toml` / the settings
/// API.
#[derive(Clone)]
pub struct RpcClusterGate(Arc<dyn Fn() -> bool + Send + Sync>);

impl RpcClusterGate {
    /// Gate backed by an arbitrary predicate (tests, embedders).
    pub fn new(is_open: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(is_open))
    }

    /// Gate backed by the live `cluster_enabled` flag of `node.toml`.
    pub fn from_node_config(store: Arc<crate::node_config::NodeConfigStore>) -> Self {
        Self::new(move || store.snapshot().cluster_enabled)
    }

    fn is_open(&self) -> bool {
        (self.0)()
    }
}

impl std::fmt::Debug for RpcClusterGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RpcClusterGate(..)")
    }
}

/// Backend label reported for models held by [`LlamaCppWorker`]; matches the
/// `backend` of the `ModelCapabilities` this node advertises for them.
pub const LLAMA_CPP_BACKEND: &str = "llama.cpp";

/// What a resident backend instance serves. One `model_id` can be resident
/// twice: llama.cpp needs a separate `--embeddings` process for embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidentModelKind {
    /// Text generation (completion and chat).
    Generation,
    /// Embedding vectors (`llama-server --embeddings`).
    Embedding,
}

/// One model a worker currently holds in a running backend process that has
/// passed its readiness check. Every field is observed by the worker's own
/// process supervision; nothing is estimated. In particular there are no
/// memory or VRAM figures: no backend here reports them reliably, and a
/// guess would be fabricated telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentModelInfo {
    /// Identifier the model was loaded under: the job's `model_cid`, which in
    /// production is the lowercase-hex content address of the verified model.
    pub model_id: String,
    /// Backend holding the model ([`LLAMA_CPP_BACKEND`] or
    /// [`crate::worker_mlx::MLX_BACKEND`]).
    pub backend: &'static str,
    pub kind: ResidentModelKind,
    /// `true` when the process runs with Tier-1 trusted-circle `ggml-rpc`
    /// devices (`--rpc`), so prompt-derived activations may leave this host.
    /// Enrolled-peer devices (`--llama-rpc-peers`) are verified attached: a
    /// load or respawn that fails to attach one is refused. Operator loopback
    /// `--llama-rpc-servers` cannot be observed from here, and the pinned
    /// `llama-server` drops an unreachable one and runs its layers locally,
    /// so for those `true` can overstate where layers ran but never
    /// understate it.
    pub trusted_circle_sharded: bool,
    /// Wall-clock time the backend process now serving the model passed its
    /// readiness check. A crashed process that was respawned reports the
    /// respawn's time.
    pub loaded_since: SystemTime,
}

/// Cloneable, backend-neutral source of a worker's [`ResidentModelInfo`]
/// list, for `/api/ps`. Worker-backed sources hold only a weak reference, so
/// they never keep a dropped worker, or its child processes, alive.
#[derive(Clone)]
pub struct ResidentModels(Arc<dyn Fn() -> Vec<ResidentModelInfo> + Send + Sync>);

impl ResidentModels {
    pub fn new(source: impl Fn() -> Vec<ResidentModelInfo> + Send + Sync + 'static) -> Self {
        Self(Arc::new(source))
    }

    /// Source for a node whose worker holds no model processes (echo,
    /// consume-only): always empty, which is the truthful answer there.
    pub fn none() -> Self {
        Self::new(Vec::new)
    }

    /// Models resident right now, ordered by `(model_id, kind)`.
    pub fn snapshot(&self) -> Vec<ResidentModelInfo> {
        (self.0)()
    }
}

impl Default for ResidentModels {
    fn default() -> Self {
        Self::none()
    }
}

impl std::fmt::Debug for ResidentModels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResidentModels(..)")
    }
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
            rpc_servers: Vec::new(),
            rpc_peer_offload: None,
            tensor_split: None,
            rpc_cluster_gate: None,
        }
    }
}

/// Upper bound on `--rpc` endpoints (explicit loopback servers plus enrolled
/// peer proxies) handed to one `llama-server`.
pub const MAX_RPC_ENDPOINTS: usize = 16;

/// Enrolled peers whose RPC device the current `llama-server` child did not
/// attach (empty without peer offload). The pinned `b12651` build does not
/// fail a load on an unreachable `--rpc` device: it drops the device and
/// runs those layers locally, which must not pass as trusted-circle work.
fn unattached_rpc_peers(proxies: Option<&rpc_peers::RpcPeerProxySet>) -> Vec<String> {
    proxies
        .map(|proxies| {
            proxies
                .unattached_peers()
                .iter()
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Caller-facing refusal for a load whose peer devices did not all attach.
/// Peer ids go to the log only: the caller may be a remote requester, and
/// circle membership is not theirs to learn.
fn rpc_peers_not_attached() -> WorkerError {
    WorkerError::Other(
        "trusted-circle RPC layer offload did not attach every enrolled peer (unreachable, refused, or assigned no layers); refusing to run those layers on this host instead"
            .into(),
    )
}

fn rpc_offload_refused() -> WorkerError {
    WorkerError::Other(
        "trusted-circle RPC layer offload is configured but cluster_enabled is off; refusing to ship model activations to RPC peers"
            .into(),
    )
}

/// Validate the complete Tier-1 RPC offload flag set: every RPC endpoint is
/// strictly loopback, `tensor_split` is well formed, and `tensor_split` is
/// never supplied without `rpc_servers` (it would otherwise be silently
/// ignored and the operator would believe layers are being split).
pub fn validate_rpc_offload_flags(
    rpc_servers: &[String],
    tensor_split: Option<&str>,
) -> std::io::Result<()> {
    if tensor_split.is_some() && rpc_servers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--llama-tensor-split requires --llama-rpc-servers or --llama-rpc-peers",
        ));
    }
    validate_loopback_rpc_servers(rpc_servers)?;
    if let Some(split) = tensor_split {
        validate_tensor_split(split)?;
    }
    Ok(())
}

/// Validate that every configured `ggml-rpc` server address is strictly loopback
/// (`127.0.0.1:<port>` or `[::1]:<port>`, non-zero port, at most 16 endpoints).
pub fn validate_loopback_rpc_servers(servers: &[String]) -> std::io::Result<()> {
    if servers.len() > MAX_RPC_ENDPOINTS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("at most {MAX_RPC_ENDPOINTS} loopback ggml-rpc servers are supported"),
        ));
    }
    for raw in servers {
        let addr: std::net::SocketAddr = raw.trim().parse().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid ggml-rpc server socket address: {raw:?}"),
            )
        })?;
        if !addr.ip().is_loopback() || addr.port() == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "ggml-rpc server address must be a non-zero loopback endpoint (127.0.0.1:PORT or [::1]:PORT), got {raw:?}"
                ),
            ));
        }
    }
    Ok(())
}

/// Validate `--tensor-split` syntax: 1..=16 comma-separated finite non-negative
/// weights with at least one positive weight.
pub fn validate_tensor_split(spec: &str) -> std::io::Result<()> {
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 16 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tensor_split must contain 1..=16 comma-separated weights",
        ));
    }
    let mut sum = 0.0f64;
    for part in parts {
        let v: f64 = part.parse().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid tensor_split weight: {part:?}"),
            )
        })?;
        if !v.is_finite() || v < 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("tensor_split weight must be finite and >= 0, got {part:?}"),
            ));
        }
        sum += v;
    }
    if sum <= 0.0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tensor_split must have at least one positive weight",
        ));
    }
    Ok(())
}

/// Per-load `llama-server` API key: 256 bits from the OS CSPRNG, hex
/// encoded. Never logged — `Debug` is redacted.
#[derive(Clone)]
struct LlamaApiKey(Arc<str>);

impl LlamaApiKey {
    fn generate() -> Result<Self, WorkerError> {
        use ring::rand::SecureRandom as _;
        let mut bytes = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| {
                WorkerError::Other("OS CSPRNG unavailable for llama-server API key".into())
            })?;
        Ok(Self(Arc::from(hex::encode(bytes))))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LlamaApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LlamaApiKey(<redacted>)")
    }
}

/// RAII reservation of one port from the worker's `llama-server` pool.
/// Dropping the guard returns the port. The guard always travels with the
/// child bound to that port — first the cold-load future, then the
/// supervisor task — so every exit path (error, cancellation, kill, abort)
/// releases it exactly once.
struct PortGuard {
    port: u16,
    pool: Arc<std::sync::Mutex<std::collections::HashSet<u16>>>,
}

impl PortGuard {
    fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortGuard {
    fn drop(&mut self) {
        // A poisoned pool only means another thread panicked mid-update; the
        // set itself is still a valid `HashSet`, and leaking the port would
        // be strictly worse.
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.remove(&self.port);
    }
}

/// State of a single loaded model. Held inside the worker's `DashMap`
/// keyed by `model_id`. The supervisor task owns the `llama-server` child
/// and its [`PortGuard`]; this handle owns the kill switch.
struct LoadedModel {
    /// Bound port — used to construct `http://127.0.0.1:{port}/completion`.
    port: u16,
    /// API key the child was spawned with; attached as a bearer token to
    /// every authenticated request (everything except `/health`).
    api_key: LlamaApiKey,
    /// Updated on every successful inference for the current LRU eviction
    /// policy.
    last_used: Mutex<Instant>,
    /// Signalled when the supervisor task has given up (3 crashes in 60s,
    /// or unload requested). All in-flight requests should bail.
    failed: Arc<Notify>,
    /// Set to true once the model is declared dead. Reads under
    /// acquire/release ordering; a stale `false` just means one extra
    /// request that the killed child refuses.
    failed_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Kill switch into the supervisor task. Sending `true` — or dropping
    /// this handle — makes the supervisor SIGKILL and reap the child, then
    /// release its port.
    kill: watch::Sender<bool>,
    /// Flips to `true` (or closes) once the supervisor has finished: the
    /// child is reaped and the port is back in the pool.
    stopped: watch::Receiver<bool>,
    /// Join handle for the supervisor task. Held so `Drop` of the worker can
    /// abort it (`kill_on_drop(true)` then terminates the child).
    supervisor: tokio::task::JoinHandle<()>,
    /// What this instance is and whether its child is healthy right now;
    /// shared with the supervisor, which keeps it current.
    residency: Arc<Residency>,
}

/// Residency record of one loaded `llama-server` instance: fixed facts about
/// the instance plus the health state its supervisor observes.
#[derive(Debug)]
struct Residency {
    /// Bare model id (the job's `model_cid`), without the embedding suffix of
    /// the `loaded_models` key.
    model_id: String,
    kind: ResidentModelKind,
    /// Spawned with at least one `--rpc` endpoint (see
    /// [`ResidentModelInfo::trusted_circle_sharded`]).
    rpc_layer_offload: bool,
    state: std::sync::Mutex<ResidencyState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidencyState {
    /// The current child passed `/health` at `since` and has passed every
    /// periodic probe since.
    Healthy { since: SystemTime },
    /// The current child is still running, but its latest periodic `/health`
    /// probe failed.
    Degraded { since: SystemTime },
    /// No healthy child: it exited, is being respawned, or was stopped.
    Down,
}

impl Residency {
    /// Record for a child that passed its `/health` gate at `since`.
    fn healthy(
        model_id: &str,
        embeddings: bool,
        rpc_layer_offload: bool,
        since: SystemTime,
    ) -> Self {
        Self {
            model_id: model_id.to_string(),
            kind: if embeddings {
                ResidentModelKind::Embedding
            } else {
                ResidentModelKind::Generation
            },
            rpc_layer_offload,
            state: std::sync::Mutex::new(ResidencyState::Healthy { since }),
        }
    }

    fn update(&self, next: impl FnOnce(ResidencyState) -> ResidencyState) {
        // A poisoned lock only means a panic elsewhere mid-update; the state
        // is a plain `Copy` value and stays meaningful.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = next(*state);
    }

    /// A (re)spawned child passed its `/health` gate.
    fn mark_healthy_now(&self) {
        self.update(|_| ResidencyState::Healthy {
            since: SystemTime::now(),
        });
    }

    fn record_probe(&self, ok: bool) {
        self.update(|state| match (state, ok) {
            (ResidencyState::Degraded { since }, true) => ResidencyState::Healthy { since },
            (ResidencyState::Healthy { since }, false) => ResidencyState::Degraded { since },
            (unchanged, _) => unchanged,
        });
    }

    fn mark_down(&self) {
        self.update(|_| ResidencyState::Down);
    }

    /// `Some(since)` only while the current child is healthy.
    fn healthy_since(&self) -> Option<SystemTime> {
        match *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ResidencyState::Healthy { since } => Some(since),
            ResidencyState::Degraded { .. } | ResidencyState::Down => None,
        }
    }
}

impl LoadedModel {
    /// Tear this model down: mark it failed (so any in-flight request
    /// bails and a concurrent `ensure_loaded` won't hand it out), then
    /// signal the supervisor to SIGKILL + reap the `llama-server` child and
    /// release its port. Idempotent.
    fn shutdown(&self) {
        self.failed_flag
            .store(true, std::sync::atomic::Ordering::Release);
        self.failed.notify_waiters();
        self.kill.send_replace(true);
    }

    /// Wait (bounded) until the supervisor has reaped the child and released
    /// the port. Returns `false` on timeout.
    async fn wait_stopped(&self, deadline: Duration) -> bool {
        let mut stopped = self.stopped.clone();
        // `wait_for` errs once the supervisor's sender is gone, which also
        // means it has stopped (finished or aborted).
        let finished = timeout(deadline, stopped.wait_for(|done| *done))
            .await
            .is_ok();
        finished
    }

    fn is_failed(&self) -> bool {
        self.failed_flag.load(std::sync::atomic::Ordering::Acquire)
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
    /// Set of ports currently reserved for live or loading `llama-server`
    /// children. A port is inserted by [`LlamaCppWorker::allocate_port`] and
    /// removed only when its [`PortGuard`] drops, so the range can't wrap
    /// onto a live port (SEC-07) and an aborted load can't leak one.
    /// Guarded by a synchronous `Mutex` (never held across `.await`) so
    /// allocate + "is the range full?" is one atomic decision and
    /// `PortGuard::drop` can release without a runtime.
    ports_in_use: Arc<std::sync::Mutex<std::collections::HashSet<u16>>>,
}

fn resident_models_of(inner: &Inner) -> Vec<ResidentModelInfo> {
    let mut resident: Vec<ResidentModelInfo> = inner
        .loaded_models
        .iter()
        .filter_map(|entry| {
            let model = entry.value();
            if model.is_failed() {
                return None;
            }
            let residency = &model.residency;
            Some(ResidentModelInfo {
                model_id: residency.model_id.clone(),
                backend: LLAMA_CPP_BACKEND,
                kind: residency.kind,
                trusted_circle_sharded: residency.rpc_layer_offload,
                loaded_since: residency.healthy_since()?,
            })
        })
        .collect();
    resident.sort_by(|a, b| (&a.model_id, a.kind).cmp(&(&b.model_id, b.kind)));
    resident
}

impl LlamaCppWorker {
    /// Construct a fresh worker. No subprocesses are spawned until the
    /// first inference request for a given model.
    pub fn new(identity: NodeIdentity, config: LlamaCppConfig) -> Self {
        let span = usize::from(
            config
                .server_port_range
                .end
                .saturating_sub(config.server_port_range.start),
        );
        if span <= config.max_loaded_models.max(1) {
            tracing::warn!(
                ports = span,
                max_loaded_models = config.max_loaded_models,
                "llama-server port range cannot hold max_loaded_models + 1 children; cold loads at the resident cap will be refused"
            );
        }
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
                ports_in_use: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            }),
        }
    }

    /// `true` when this worker offloads layers to `ggml-rpc` peers. All of
    /// its models then execute on the Tier-1 trusted circle, not only on this
    /// host — routing and privacy decisions must treat it accordingly.
    pub fn uses_rpc_layer_offload(&self) -> bool {
        !self.inner.config.rpc_servers.is_empty() || self.inner.config.rpc_peer_offload.is_some()
    }

    /// Models resident right now: every loaded instance whose current
    /// `llama-server` child has passed `/health` and every periodic probe
    /// since, and has not been retired. Instances that are loading,
    /// crashed, respawning, failing probes, or shutting down are omitted.
    pub fn resident_models(&self) -> Vec<ResidentModelInfo> {
        resident_models_of(&self.inner)
    }

    /// [`ResidentModels`] source for this worker that does not keep it alive.
    pub fn resident_models_handle(&self) -> ResidentModels {
        let inner = Arc::downgrade(&self.inner);
        ResidentModels::new(move || {
            inner
                .upgrade()
                .map(|inner| resident_models_of(&inner))
                .unwrap_or_default()
        })
    }

    /// Honest execution label for jobs served by this worker:
    /// `"trusted-circle-sharded"` when RPC layer offload is configured,
    /// otherwise `"local"`.
    pub fn execution_trust_label(&self) -> &'static str {
        if self.uses_rpc_layer_offload() {
            crate::shard_plan::ShardTrustTier::TrustedCircle.trust_label()
        } else {
            "local"
        }
    }

    /// Fail closed unless RPC layer offload is currently opted in. When the
    /// operator has turned `cluster_enabled` off, RPC-backed children are
    /// torn down too, so remote layer links do not outlive the opt-in.
    fn require_rpc_offload_allowed(&self) -> Result<(), WorkerError> {
        if self.rpc_offload_permitted() {
            return Ok(());
        }
        let resident: Vec<String> = self
            .inner
            .loaded_models
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for key in resident {
            if let Some((_, model)) = self.inner.loaded_models.remove(&key) {
                model.shutdown();
            }
        }
        Err(rpc_offload_refused())
    }

    /// The configured trusted-circle gate, or a permanently closed one when
    /// none was wired (fail closed).
    fn rpc_gate(&self) -> RpcClusterGate {
        self.inner
            .config
            .rpc_cluster_gate
            .clone()
            .unwrap_or_else(|| RpcClusterGate::new(|| false))
    }

    /// `true` unless RPC layer offload is configured and the trusted-circle
    /// opt-in is currently off (or no gate was wired, which fails closed).
    fn rpc_offload_permitted(&self) -> bool {
        !self.uses_rpc_layer_offload()
            || self
                .inner
                .config
                .rpc_cluster_gate
                .as_ref()
                .is_some_and(RpcClusterGate::is_open)
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

        // Fast path: warm hits avoid serializing behind an unrelated cold
        // model load (which can hold `load_gate` for up to `model_load_timeout`).
        if let Some(existing) = self.inner.loaded_models.get(&load_key) {
            if !existing.is_failed() {
                return Ok(existing.clone());
            }
        }

        // Slow path: serialize every mutation involved in a cold load or dead-
        // model replacement. A waiter re-checks the map after the winner
        // inserts and returns that exact process instead of spawning a
        // duplicate. Loading is serialized intentionally because each cold
        // start can consume gigabytes of RAM.
        let _load_guard = self.inner.load_gate.lock().await;

        let stale = match self.inner.loaded_models.get(&load_key) {
            Some(existing) if !existing.is_failed() => return Ok(existing.clone()),
            Some(existing) => Some(existing.clone()),
            None => None,
        };
        if let Some(stale) = stale {
            // The previous instance was declared dead (crash budget spent,
            // hang detected, or protocol violation). Make sure its child is
            // killed and reaped and its port is back in the pool before the
            // replacement allocates one.
            self.inner.loaded_models.remove(&load_key);
            stale.shutdown();
            if !stale.wait_stopped(SUPERVISOR_STOP_TIMEOUT).await {
                tracing::warn!(
                    model = %model_id,
                    "stale llama-server supervisor did not stop in time; its port stays reserved until it does"
                );
            }
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

        // Everything from here to the map insert owns the child and its port
        // through RAII: an error return, or the caller dropping this future
        // mid-load (client disconnect), kills the child (`kill_on_drop`) and
        // releases the port (`PortGuard`). No warm model has been touched
        // yet — eviction is deferred until the new model is healthy.
        let port_guard = self.allocate_port()?;
        let port = port_guard.port();
        let api_key = LlamaApiKey::generate()?;
        // Enrolled-peer offload: this spawn's own loopback proxies, owned (like
        // the port) by whoever owns the child, so a lost tunnel retires only
        // the model that used it.
        let rpc_peer_proxies = match &self.inner.config.rpc_peer_offload {
            Some(offload) => Some(offload.bind(self.rpc_gate()).await.map_err(|error| {
                WorkerError::Other(format!("bind trusted-circle RPC peer proxies: {error}"))
            })?),
            None => None,
        };
        let mut rpc_endpoints = self.inner.config.rpc_servers.clone();
        if let Some(proxies) = &rpc_peer_proxies {
            rpc_endpoints.extend(proxies.rpc_endpoints());
        }
        let mut child = llama_server_command(
            &self.inner.config.server_binary_path,
            &model_path,
            port,
            &api_key,
            self.inner.config.default_n_gpu_layers,
            self.inner.config.default_context_size,
            &self.inner.config.extra_env,
            embeddings,
            &rpc_endpoints,
            self.inner.config.tensor_split.as_deref(),
        )
        .and_then(|mut cmd| cmd.spawn())
        .map_err(|e| WorkerError::Other(format!("spawn llama-server: {e}")))?;

        // Wait for /health to go 200 before declaring the model loaded.
        wait_for_health(
            &self.inner.client,
            port,
            self.inner.config.model_load_timeout,
            &mut child,
        )
        .await
        .map_err(|e| WorkerError::Other(format!("llama-server health check: {e}")))?;
        let healthy_since = SystemTime::now();

        // An RPC cold load can take minutes; if the operator withdrew the
        // trusted-circle opt-in meanwhile, refuse admission (dropping `child`
        // kills it and `port_guard` releases its port).
        if !self.rpc_offload_permitted() {
            return Err(rpc_offload_refused());
        }
        let unattached = unattached_rpc_peers(rpc_peer_proxies.as_ref());
        if !unattached.is_empty() {
            tracing::warn!(
                model = %model_id,
                peers = ?unattached,
                "llama-server passed /health without attaching every trusted-circle RPC peer; refusing the load"
            );
            return Err(rpc_peers_not_attached());
        }

        // Admission is committed: the replacement is spawned and healthy.
        // Only now enforce the resident-model cap (SEC-07) by evicting the
        // least-recently-used *other* model, so a failed or cancelled cold
        // load can never cost a healthy warm model. The incoming candidate is
        // the composite key so a chat instance of the same model isn't
        // mistaken for "already the incoming one".
        self.evict_lru_if_at_cap(&load_key).await;

        let failed = Arc::new(Notify::new());
        let failed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (kill_tx, kill_rx) = watch::channel(false);
        let (stopped_tx, stopped_rx) = watch::channel(false);
        let residency = Arc::new(Residency::healthy(
            model_id,
            embeddings,
            !rpc_endpoints.is_empty(),
            healthy_since,
        ));

        // The map entry is keyed by the composite `load_key`, but the
        // `model_id` carried into telemetry stays the bare alias so logs
        // read naturally. The supervisor re-spawns with the same
        // `embeddings` flag so a crashed embedding instance comes back as an
        // embedding instance, and with the same API key so in-flight handles
        // stay valid across a restart.
        let supervisor_input = SupervisorInput {
            model_id: model_id.to_string(),
            port_guard,
            api_key: api_key.clone(),
            failed: failed.clone(),
            failed_flag: failed_flag.clone(),
            kill: kill_rx,
            stopped: stopped_tx,
            client: self.inner.client.clone(),
            config: self.inner.config.clone(),
            model_path,
            embeddings,
            rpc_endpoints,
            rpc_peer_proxies,
            residency: residency.clone(),
        };

        // The supervisor task owns the child and its port for the model's
        // lifetime (including respawned children); `LoadedModel` keeps only
        // the kill switch.
        let supervisor = tokio::spawn(run_supervisor(supervisor_input, child));

        let loaded = Arc::new(LoadedModel {
            port,
            api_key,
            last_used: Mutex::new(Instant::now()),
            failed,
            failed_flag,
            kill: kill_tx,
            stopped: stopped_rx,
            supervisor,
            residency,
        });
        // `load_gate` makes this insert a single-flight commit. Keep the
        // replacement guard as defense in depth if the synchronization model
        // changes later.
        if let Some(prev) = self.inner.loaded_models.insert(load_key, loaded.clone()) {
            if prev.port != port {
                prev.shutdown();
            }
        }
        Ok(loaded)
    }

    /// SEC-07: reserve a port not currently bound by a live child. Scans
    /// the configured range for the first free slot and reserves it behind
    /// a [`PortGuard`]. When every port in the range is in use, returns
    /// [`WorkerError::Capacity`] rather than wrapping onto a live port (the
    /// old `fetch_add % span` behaviour, which silently collided and churned
    /// spawn/fail).
    fn allocate_port(&self) -> Result<PortGuard, WorkerError> {
        let range = self.inner.config.server_port_range.clone();
        let mut in_use = self
            .inner
            .ports_in_use
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for port in range.clone() {
            if in_use.contains(&port) {
                continue;
            }
            // Prove that the OS currently considers the loopback address
            // bindable, not merely that this process has not recorded it.
            // llama-server cannot inherit this listener, so a narrow
            // close-to-child-bind race remains; child liveness checks around
            // `/health` below prevent a normally failing child from accepting
            // an unrelated listener as healthy, and the per-spawn API key
            // means an impostor cannot answer authenticated requests.
            if std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
                in_use.insert(port);
                return Ok(PortGuard {
                    port,
                    pool: Arc::clone(&self.inner.ports_in_use),
                });
            }
        }
        tracing::warn!(
            range_start = range.start,
            range_end = range.end,
            "llama-server port range exhausted; refusing load"
        );
        Err(WorkerError::Capacity)
    }

    /// SEC-07: if the worker is already at `max_loaded_models`, evict the
    /// least-recently-used model (by `last_used`) to make room for a new
    /// one. The model currently being (re)loaded — `incoming` — is never a
    /// candidate. The evicted model's subprocess is killed and reaped via
    /// [`LoadedModel::shutdown`], which also releases its port.
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
                // Wait for the reap so the victim's memory and port are
                // actually free before the next cold load can count on them.
                if !model.wait_stopped(SUPERVISOR_STOP_TIMEOUT).await {
                    tracing::warn!(
                        model = %victim_id,
                        "evicted llama-server supervisor did not stop in time"
                    );
                }
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
                let mut request = build_completion_request(&inference)
                    .map_err(|e| WorkerError::BadManifest(format!("invalid sampling: {e}")))?;
                self.require_rpc_offload_allowed()?;

                // Load the model up front so dispatch-time errors are
                // returned through `WorkerError` rather than as a single
                // `Final::Error` event with no chunks. Once we get past
                // this point the only failure mode is in-stream. The chat
                // path always loads the non-embedding flavour.
                let model = self.ensure_loaded(&inference.model_cid, false).await?;
                if !inference.messages.is_empty() {
                    apply_chat_template(
                        &self.inner.client,
                        &model,
                        &inference,
                        &mut request,
                        self.inner.config.per_request_idle_timeout,
                    )
                    .await?;
                }

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
                self.require_rpc_offload_allowed()?;

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

/// Build the fully configured `llama-server` command for one spawn. Kept
/// separate from spawning so the exact argv/env contract is unit-testable.
/// Callers spawn it immediately and wait on `/health` separately.
#[allow(clippy::too_many_arguments)]
fn llama_server_command(
    binary: &Path,
    model: &Path,
    port: u16,
    api_key: &LlamaApiKey,
    n_gpu_layers: i32,
    ctx_size: usize,
    extra_env: &[(String, String)],
    embeddings: bool,
    rpc_servers: &[String],
    tensor_split: Option<&str>,
) -> std::io::Result<Command> {
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
    cmd.env("XDG_CACHE_HOME", std::env::temp_dir());
    cmd.stdin(Stdio::null());

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
    // Set after `extra_env` so no configured variable can replace or clear
    // the per-spawn key. Environment, not argv: argv is world-readable.
    cmd.env(LLAMA_API_KEY_ENV, api_key.as_str());
    cmd.args(LLAMA_SERVER_HARDENING_ARGS);
    // `--jinja` enables modern chat templates + tool calling.
    // Cross-tenant KV-cache isolation: one `llama-server` instance serves all
    // callers for a model. On pinned `b12651`, `--cache-reuse` and the default
    // 8 GiB `--cache-ram` prompt cache create a ~15 ms vs ~800 ms prompt-
    // confirmation timing side channel visible in `total_duration_ms`, and a
    // per-request `cache_prompt: false` alone does not prevent reuse unless
    // every request sets `cache_prompt: false` alongside `--no-cache-prompt`,
    // `--cache-ram 0`, and `--cache-reuse 0`.
    cmd.arg("--jinja");
    cmd.arg("--no-cache-prompt");
    cmd.arg("--cache-ram").arg("0");
    cmd.arg("--cache-reuse").arg("0");
    validate_rpc_offload_flags(rpc_servers, tensor_split)?;
    if !rpc_servers.is_empty() {
        let normalized: Vec<String> = rpc_servers.iter().map(|s| s.trim().to_string()).collect();
        cmd.arg("--rpc").arg(normalized.join(","));
        cmd.arg("--split-mode").arg("layer");
        if let Some(split) = tensor_split {
            cmd.arg("--tensor-split").arg(split.trim());
        }
    }
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
    Ok(cmd)
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
    /// Owned for the supervisor's lifetime: the port returns to the pool
    /// only after the last child bound to it has been reaped.
    port_guard: PortGuard,
    api_key: LlamaApiKey,
    failed: Arc<Notify>,
    failed_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Kill switch from [`LoadedModel`]: `true`, or the sender being
    /// dropped, ends supervision with a SIGKILL + reap.
    kill: watch::Receiver<bool>,
    /// Dropped (closing the channel) or set when supervision has ended.
    stopped: watch::Sender<bool>,
    client: reqwest::Client,
    config: LlamaCppConfig,
    model_path: PathBuf,
    /// Whether this instance was spawned with `--embeddings`. Re-spawns on
    /// crash must reuse the same flavour or the endpoint shape would flip.
    embeddings: bool,
    /// The complete `--rpc` list of this model (explicit servers plus its own
    /// peer proxies); respawns reuse it.
    rpc_endpoints: Vec<String>,
    /// This model's enrolled-peer proxies, if any. Dropped only after the
    /// last child is reaped, which closes every tunnel it carried.
    rpc_peer_proxies: Option<rpc_peers::RpcPeerProxySet>,
    /// Kept current with what supervision observes about the child.
    residency: Arc<Residency>,
}

/// Long-running task: watch the child, restart on crash up to 3 times in
/// 60 s, periodically poll `/health`, and obey the kill switch. Owns the
/// `Child` and its [`PortGuard`] for the lifetime of the loaded model.
///
/// The child lives in `slot` *outside* the supervision future, so when the
/// kill switch wins the race the child is still reachable here and can be
/// SIGKILLed **and reaped** before the port is released — the next owner of
/// the port never races a dying process for it.
async fn run_supervisor(input: SupervisorInput, initial_child: Child) {
    let SupervisorInput {
        model_id,
        port_guard,
        api_key,
        failed,
        failed_flag,
        mut kill,
        stopped,
        client,
        config,
        model_path,
        embeddings,
        rpc_endpoints,
        rpc_peer_proxies,
        residency,
    } = input;
    let port = port_guard.port();
    let mut slot = Some(initial_child);
    drain_child_io(&mut slot);
    let mut tunnel_losses = rpc_peer_proxies
        .as_ref()
        .map(rpc_peers::RpcPeerProxySet::remote_losses);

    let killed = tokio::select! {
        biased;
        // `Err` means the `LoadedModel` handle was dropped: nobody can reach
        // this child any more, so it must not outlive the handle.
        _ = kill.wait_for(|requested| *requested) => true,
        // One of this model's trusted-circle tunnels ended outside
        // llama-server's control: its next use of that ggml-rpc socket would
        // abort it, possibly mid-request. Retire it now; the next request
        // cold-loads over fresh tunnels.
        () = rpc_tunnel_lost(&mut tunnel_losses) => {
            tracing::warn!(model = %model_id, port, "trusted-circle RPC tunnel lost; retiring model");
            true
        }
        () = supervise(
            &mut slot,
            &model_id,
            port,
            &api_key,
            &client,
            &config,
            &model_path,
            embeddings,
            &rpc_endpoints,
            rpc_peer_proxies.as_ref(),
            &residency,
        ) => false,
    };
    residency.mark_down();

    if let Some(mut child) = slot.take() {
        if killed {
            tracing::info!(model = %model_id, port, "killing llama-server on request");
        }
        // `kill()` sends SIGKILL and then waits, so the process is reaped
        // (and its listener closed) before `port_guard` drops below.
        if let Err(error) = child.kill().await {
            tracing::warn!(model = %model_id, %error, "failed to kill llama-server child");
        }
    }
    drop(rpc_peer_proxies);
    failed_flag.store(true, std::sync::atomic::Ordering::Release);
    failed.notify_waiters();
    drop(port_guard);
    stopped.send_replace(true);
}

/// Resolves when one of this model's peer tunnels is lost; never without
/// enrolled-peer offload.
async fn rpc_tunnel_lost(losses: &mut Option<watch::Receiver<u64>>) {
    if let Some(losses) = losses {
        if losses.changed().await.is_ok() {
            return;
        }
    }
    std::future::pending().await
}

/// The supervision loop proper. Returns once the model is terminally dead
/// (clean exit, crash budget exhausted, failed respawn, or the cluster gate
/// closed). On return `slot` holds no live child, or a child the caller
/// must kill.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    slot: &mut Option<Child>,
    model_id: &str,
    port: u16,
    api_key: &LlamaApiKey,
    client: &reqwest::Client,
    config: &LlamaCppConfig,
    model_path: &Path,
    embeddings: bool,
    rpc_endpoints: &[String],
    rpc_peer_proxies: Option<&rpc_peers::RpcPeerProxySet>,
    residency: &Residency,
) {
    // Sliding window of recent crash timestamps. If we accumulate three
    // entries inside a 60 s window, give up.
    let mut crash_times: Vec<Instant> = Vec::new();
    let crash_window = Duration::from_secs(60);
    let health_url = format!("http://127.0.0.1:{port}/health");

    loop {
        let Some(child) = slot.as_mut() else {
            return;
        };

        let mut consecutive_health_fail: u32 = 0;
        let health_interval = Duration::from_secs(30);
        let mut health_timer = tokio::time::interval(health_interval);
        // Tick once immediately to clear the "first tick is now" behaviour.
        health_timer.tick().await;

        // Two concurrent things: wait for the child to exit, and run
        // periodic /health checks. Whichever fires first decides what
        // happens next.
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
                            residency.record_probe(true);
                        }
                        _ => {
                            residency.record_probe(false);
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
        // The child has exited (or was killed and reaped) either way.
        *slot = None;
        residency.mark_down();

        match exit_kind {
            ChildExit::CleanExit => {
                // Nothing in Phase asks llama-server to exit cleanly, so a
                // zero exit is treated as terminal rather than restarted.
                tracing::info!(model = %model_id, "llama-server exited cleanly");
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
                    return;
                }
                // Exponential backoff: 1s, 2s, 4s.
                let backoff_secs = 1u64 << crash_times.len().saturating_sub(1);
                let backoff = Duration::from_secs(backoff_secs).min(Duration::from_secs(8));
                tracing::info!(model = %model_id, backoff_ms = backoff.as_millis() as u64, "restarting llama-server");
                tokio::time::sleep(backoff).await;
                // A crash must not silently re-establish remote layer links
                // after the operator has withdrawn the trusted-circle opt-in.
                if !rpc_endpoints.is_empty()
                    && !config
                        .rpc_cluster_gate
                        .as_ref()
                        .is_some_and(RpcClusterGate::is_open)
                {
                    tracing::warn!(
                        model = %model_id,
                        "cluster_enabled is off; not respawning RPC-backed llama-server"
                    );
                    return;
                }
                // Tunnels of the crashed child may linger while closing; only
                // the respawn's own count as attached.
                if let Some(proxies) = rpc_peer_proxies {
                    proxies.begin_respawn();
                }
                match llama_server_command(
                    &config.server_binary_path,
                    model_path,
                    port,
                    api_key,
                    config.default_n_gpu_layers,
                    config.default_context_size,
                    &config.extra_env,
                    embeddings,
                    rpc_endpoints,
                    config.tensor_split.as_deref(),
                )
                .and_then(|mut cmd| cmd.spawn())
                {
                    Ok(respawned) => {
                        *slot = Some(respawned);
                        drain_child_io(slot);
                        let Some(respawned) = slot.as_mut() else {
                            return;
                        };
                        if let Err(e) =
                            wait_for_health(client, port, config.model_load_timeout, respawned)
                                .await
                        {
                            tracing::warn!(
                                model = %model_id,
                                error = %e,
                                "respawned llama-server failed health check"
                            );
                            // The caller kills and reaps the child still in
                            // `slot`.
                            return;
                        }
                        let unattached = unattached_rpc_peers(rpc_peer_proxies);
                        if !unattached.is_empty() {
                            tracing::warn!(
                                model = %model_id,
                                peers = ?unattached,
                                "respawned llama-server did not attach every trusted-circle RPC peer; retiring it"
                            );
                            return;
                        }
                        residency.mark_healthy_now();
                    }
                    Err(e) => {
                        tracing::error!(model = %model_id, error = %e, "failed to respawn");
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
    let prompt = if inference.messages.is_empty() {
        inference.prompt.clone().unwrap_or_default()
    } else {
        String::new() // Replaced by the loaded model's template before dispatch.
    };
    let prompt_chars = prompt.chars().count() as u64;
    let mut body = validated_sampling_params(&inference.sampling)?;

    body.insert("prompt".to_string(), serde_json::Value::String(prompt));
    body.insert("stream".to_string(), serde_json::Value::Bool(true));
    // Server-owned cross-tenant KV-cache isolation: callers cannot re-enable
    // prompt caching on a shared `llama-server` instance.
    body.insert("cache_prompt".to_string(), serde_json::Value::Bool(false));

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

        let response = client
            .post(&url)
            .bearer_auth(model.api_key.as_str())
            .json(&body)
            .send()
            .await;
        let resp = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                if let Err(error) = read_limited_body(r, MAX_BACKEND_ERROR_BODY_BYTES).await {
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
                // Do not echo raw backend error text into signed receipts or
                // remote error responses: backend messages may quote prompt
                // fragments or internal paths.
                yield emit_final_error(
                    &mut producer,
                    &identity,
                    manifest_hash,
                    prompt_chars,
                    0,
                    started_at,
                    format!("llama-server returned {status}"),
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
                    // A wedged llama-server keeps its VRAM/RAM and port
                    // forever if merely flagged. Kill it now: the supervisor
                    // SIGKILLs and reaps the child and releases the port, and
                    // the next request cold-loads a fresh instance. This must
                    // happen BEFORE the terminal yield — consumers stop
                    // polling at `Final`, so code after it may never run.
                    tracing::warn!(
                        port = model.port,
                        idle_timeout_ms = idle_timeout.as_millis() as u64,
                        "llama-server stream hung; killing child"
                    );
                    model.shutdown();
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
                .bearer_auth(model.api_key.as_str())
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

/// Use the loaded model's own chat template without changing the native
/// completion stream or its receipt commitment. Never fall back to generic
/// role markers when the backend cannot format this conversation.
async fn apply_chat_template(
    client: &reqwest::Client,
    model: &LoadedModel,
    inference: &InferenceJobSpec,
    request: &mut CompletionRequest,
    deadline: Duration,
) -> Result<(), WorkerError> {
    #[derive(Deserialize)]
    struct TemplateResponse {
        prompt: String,
    }
    let port = model.port;
    let formatted = timeout(deadline, async {
        let response = client
            .post(format!("http://127.0.0.1:{port}/apply-template"))
            .bearer_auth(model.api_key.as_str())
            // This backend exports plain text, without a reasoning-channel
            // parser. Ask the model template for its supported non-thinking
            // path rather than leaking raw reasoning control tokens.
            .json(&serde_json::json!({
                "messages": inference.messages,
                "add_generation_prompt": true,
                "chat_template_kwargs": {"enable_thinking": false},
            }))
            .send()
            .await
            .map_err(|error| {
                WorkerError::Other(format!("chat template request failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(WorkerError::Other(format!(
                "chat template request returned {}",
                response.status()
            )));
        }
        let bytes = read_limited_body(response, MAX_CHAT_TEMPLATE_BODY_BYTES)
            .await
            .map_err(|error| {
                WorkerError::Other(format!("chat template response failed: {error}"))
            })?;
        let response: TemplateResponse = serde_json::from_slice(&bytes)
            .map_err(|_| WorkerError::Other("invalid chat template response".into()))?;
        if response.prompt.is_empty() {
            return Err(WorkerError::Other("empty chat template response".into()));
        }
        Ok(response.prompt)
    })
    .await
    .map_err(|_| WorkerError::Other("chat template request timed out".into()))??;
    request.prompt_chars = formatted.chars().count() as u64;
    request.body["prompt"] = serde_json::Value::String(formatted);
    Ok(())
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

pub mod rpc_peers;

#[cfg(test)]
mod tests;
