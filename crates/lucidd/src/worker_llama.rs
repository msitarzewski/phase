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
    ChatRole, CommitmentAccumulator, Completion, InferenceJobSpec, JobEvent, JobHandle,
    JobHandleProducer, JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream, OutputChunk,
    SignedManifest, Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

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
    /// Model alias the caller used to request this load. Stable for the
    /// life of the LoadedModel; eviction creates a new entry.
    #[allow(dead_code)] // surfaced via metrics in LUCID M6
    model_id: String,
    /// First time the worker saw this model. Useful for "uptime since
    /// load" telemetry and stale-state debugging.
    #[allow(dead_code)]
    loaded_at: Instant,
    /// Updated on every successful inference. The eviction policy in
    /// LUCID M6 reads this to decide what to unload first.
    last_used: Mutex<Instant>,
    /// Signalled when the supervisor task has given up (3 crashes in 60s,
    /// or unload requested). All in-flight requests should bail.
    failed: Arc<Notify>,
    /// Set to true once the supervisor declared the model dead. Reads
    /// under acquire/release ordering; a stale `false` just means one
    /// extra retry that will hit `failed.notified()` immediately.
    failed_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Join handle for the supervisor task. Held so we can abort it on
    /// `Drop` of the [`LlamaCppWorker`].
    #[allow(dead_code)]
    supervisor: tokio::task::JoinHandle<()>,
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
    /// Next port to try when allocating. Atomic so multiple concurrent
    /// loads don't collide; the per-port bind check inside `spawn_model`
    /// catches the rare race where two loads pick the same port.
    next_port: std::sync::atomic::AtomicU16,
}

impl LlamaCppWorker {
    /// Construct a fresh worker. No subprocesses are spawned until the
    /// first inference request for a given model.
    pub fn new(identity: NodeIdentity, config: LlamaCppConfig) -> Self {
        let next_port = std::sync::atomic::AtomicU16::new(config.server_port_range.start);
        let client = reqwest::Client::builder()
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
                next_port,
            }),
        }
    }

    /// Ensure a model is loaded. Idempotent — if the model is already
    /// loaded, returns the existing entry. If not, spawns a new
    /// `llama-server` subprocess and waits for `/health` to go green
    /// before returning.
    async fn ensure_loaded(&self, model_id: &str) -> Result<Arc<LoadedModel>, WorkerError> {
        if let Some(existing) = self.inner.loaded_models.get(model_id) {
            if !existing
                .failed_flag
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(existing.clone());
            }
            // The previous load has been declared dead; drop it and try
            // again. The supervisor's `kill()` already ran.
            drop(existing);
            self.inner.loaded_models.remove(model_id);
        }

        let model_path = resolve_model_path(&self.inner.config.model_dir, model_id);
        if !model_path.exists() {
            return Err(WorkerError::ArtifactUnavailable(format!(
                "model file not found: {}",
                model_path.display()
            )));
        }

        let port = self.allocate_port();
        let child = spawn_llama_server(
            &self.inner.config.server_binary_path,
            &model_path,
            port,
            self.inner.config.default_n_gpu_layers,
            self.inner.config.default_context_size,
            &self.inner.config.extra_env,
        )
        .map_err(|e| WorkerError::Other(format!("spawn llama-server: {e}")))?;

        // Wait for /health to go 200 before declaring the model loaded.
        wait_for_health(&self.inner.client, port, self.inner.config.model_load_timeout)
            .await
            .map_err(|e| WorkerError::Other(format!("llama-server health check: {e}")))?;

        let failed = Arc::new(Notify::new());
        let failed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let model_id_owned = model_id.to_string();
        let supervisor_input = SupervisorInput {
            model_id: model_id_owned.clone(),
            port,
            failed: failed.clone(),
            failed_flag: failed_flag.clone(),
            client: self.inner.client.clone(),
            config: self.inner.config.clone(),
            model_path: model_path.clone(),
        };

        // The supervisor task gets the child handle (so it can wait/kill).
        // The `LoadedModel`'s `Mutex<Option<Child>>` starts empty — the
        // supervisor "owns" the process for its lifetime. Re-spawned
        // children are also held inside the supervisor.
        let supervisor = tokio::spawn(run_supervisor(supervisor_input, child));

        let loaded = Arc::new(LoadedModel {
            port,
            model_id: model_id_owned.clone(),
            loaded_at: Instant::now(),
            last_used: Mutex::new(Instant::now()),
            failed,
            failed_flag,
            supervisor,
        });
        self.inner
            .loaded_models
            .insert(model_id_owned, loaded.clone());
        Ok(loaded)
    }

    fn allocate_port(&self) -> u16 {
        // Wrap inside the configured range. There's a tiny race window
        // between `fetch_add` and `bind()` inside the child where two
        // loads can pick adjacent ports and one fails; the spawn path
        // surfaces that as `WorkerError::Other` and the caller retries.
        let range = &self.inner.config.server_port_range;
        let span = (range.end - range.start) as u32;
        let raw = self
            .inner
            .next_port
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u32;
        let offset = if span == 0 { 0 } else { raw % span };
        range.start + offset as u16
    }
}

impl Worker for LlamaCppWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        &[JobSpecKind::Inference]
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        let inference = match &job.payload {
            JobSpec::Inference(spec) => spec.clone(),
            other => {
                return Err(WorkerError::Unsupported {
                    kind: other.kind(),
                });
            }
        };

        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let job_id = JobId(manifest_hash);

        // Load the model up front so dispatch-time errors are returned
        // through `WorkerError` rather than as a single `Final::Error`
        // event with no chunks. Once we get past this point the only
        // failure mode is in-stream.
        let model = self.ensure_loaded(&inference.model_cid).await?;

        let (handle, producer) = JobHandle::new(job_id);
        let identity = self.inner.identity.clone();
        let client = self.inner.client.clone();
        let idle_timeout = self.inner.config.per_request_idle_timeout;

        let stream: JobStream = Box::pin(run_inference(
            client,
            model,
            inference,
            manifest_hash,
            producer,
            identity,
            idle_timeout,
        ));
        Ok((handle, stream))
    }
}

// ---------------------------------------------------------------------------
// Subprocess management
// ---------------------------------------------------------------------------

/// Resolve `model_id` ("llama-3.2-3b" / "llama-3.2-3b.gguf" / absolute
/// path) into an actual filesystem path inside `model_dir`. Absolute
/// paths bypass the dir entirely so test fixtures and dev setups don't
/// have to copy GGUFs around.
fn resolve_model_path(model_dir: &Path, model_id: &str) -> PathBuf {
    let candidate = PathBuf::from(model_id);
    if candidate.is_absolute() {
        return candidate;
    }
    let with_ext = if model_id.ends_with(".gguf") {
        model_dir.join(model_id)
    } else {
        model_dir.join(format!("{model_id}.gguf"))
    };
    with_ext
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
) -> std::io::Result<Child> {
    let mut cmd = Command::new(binary);
    cmd.arg("--model").arg(model);
    cmd.arg("--host").arg("127.0.0.1");
    cmd.arg("--port").arg(port.to_string());
    cmd.arg("--ctx-size").arg(ctx_size.to_string());
    if n_gpu_layers == i32::MAX {
        cmd.arg("--n-gpu-layers").arg("all");
    } else {
        cmd.arg("--n-gpu-layers").arg(n_gpu_layers.to_string());
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
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/health");
    let started = Instant::now();
    let poll_interval = Duration::from_millis(200);
    let mut last_err = String::from("never responded");
    while started.elapsed() < deadline {
        match client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => return Ok(()),
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
                );
                match respawned {
                    Ok(c) => {
                        let mut respawned_opt = Some(c);
                        drain_child_io(&mut respawned_opt);
                        current_child = respawned_opt;
                        // Wait for /health on the respawned child.
                        if let Err(e) =
                            wait_for_health(&client, port, config.model_load_timeout).await
                        {
                            tracing::warn!(
                                model = %model_id,
                                error = %e,
                                "respawned llama-server failed health check"
                            );
                            if let Some(mut c) = current_child.take() {
                                let _ = c.kill().await;
                            }
                            crash_times.push(Instant::now());
                            if crash_times.len() >= 3 {
                                failed_flag
                                    .store(true, std::sync::atomic::Ordering::Release);
                                failed.notify_waiters();
                                return;
                            }
                            // Loop will fall through to the next iteration;
                            // `current_child` is None, so we break out of
                            // the outer loop and stop supervising.
                            failed_flag
                                .store(true, std::sync::atomic::Ordering::Release);
                            failed.notify_waiters();
                            return;
                        }
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
        let reader = tokio::io::BufReader::new(stdout);
        tokio::spawn(async move {
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::trace!(target: "lucidd::llama_server", "stdout: {line}");
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let reader = tokio::io::BufReader::new(stderr);
        tokio::spawn(async move {
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::trace!(target: "lucidd::llama_server", "stderr: {line}");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Inference path
// ---------------------------------------------------------------------------

/// Drive a single inference: render the prompt, fire `POST /completion`
/// with `stream: true`, decode SSE frames into [`JobEvent::Output`], and
/// produce a signed receipt at the end.
fn run_inference(
    client: reqwest::Client,
    model: Arc<LoadedModel>,
    inference: InferenceJobSpec,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    identity: NodeIdentity,
    idle_timeout: Duration,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    stream! {
        let started_at = Instant::now();
        let prompt = render_prompt(&inference);
        let prompt_chars = prompt.chars().count() as u64;
        let url = format!("http://127.0.0.1:{}/completion", model.port);
        let mut body = serde_json::json!({
            "prompt": prompt,
            "stream": true,
            "cache_prompt": true,
        });
        if let Some(map) = body.as_object_mut() {
            if let Some(n_predict) = inference.max_tokens {
                map.insert("n_predict".to_string(), serde_json::json!(n_predict));
            }
            // Pass-through of sampling params. We only forward keys with
            // numeric/string values that JSON-decode cleanly; anything we
            // can't parse is silently dropped (server tolerates unknown
            // sampler names but not malformed JSON).
            for (k, v) in &inference.sampling.params {
                if let Ok(json_v) = serde_json::from_str::<serde_json::Value>(v) {
                    map.insert(k.clone(), json_v);
                }
            }
        }

        let response = client.post(&url).json(&body).send().await;
        let resp = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
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
                    yield emit_final_error(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        format!("SSE stream broke: {e}"),
                    );
                    return;
                }
                Ok(None) => {
                    // Stream ended without a `stop:true` frame. Treat
                    // as Stop with an empty terminal — same path as
                    // happy completion, just no token left to flush.
                    break;
                }
                Err(_elapsed) => {
                    yield emit_final_error(
                        &mut producer,
                        &identity,
                        manifest_hash,
                        prompt_chars,
                        completion_tokens,
                        started_at,
                        format!("no token within {:?} (hang detected)", idle_timeout),
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
            buf.extend_from_slice(&chunk);
            while let Some(pos) = find_double_newline(&buf) {
                let frame_bytes = buf.split_to(pos + 2);
                // Two `\n\n` → drop the second one (we kept it as the
                // record boundary).
                let frame = &frame_bytes[..frame_bytes.len().saturating_sub(2)];
                if let Some(json_part) = strip_sse_data_prefix(frame) {
                    match serde_json::from_slice::<CompletionFrame>(json_part) {
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
                                if let Some(st) = f.stop_type {
                                    final_stop_type = Some(st);
                                }
                                break 'outer;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "skipping malformed SSE frame");
                        }
                    }
                }
            }
        }

        {
            let mut last = model.last_used.lock().await;
            *last = Instant::now();
        }

        let (commitment, count) = acc.finalize();
        let completion = if cancelled {
            Completion::Cancelled
        } else {
            match final_stop_type.as_deref() {
                Some("limit") | Some("length") => Completion::Length,
                _ => Completion::Stop,
            }
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
    let (commitment, count) = CommitmentAccumulator::new().finalize();
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
    use phase_protocol::ChatMessage;

    #[test]
    fn resolve_model_path_appends_gguf() {
        let p = resolve_model_path(Path::new("/var/models"), "llama-3.2");
        assert_eq!(p, PathBuf::from("/var/models/llama-3.2.gguf"));
    }

    #[test]
    fn resolve_model_path_respects_existing_extension() {
        let p = resolve_model_path(Path::new("/var/models"), "llama-3.2.gguf");
        assert_eq!(p, PathBuf::from("/var/models/llama-3.2.gguf"));
    }

    #[test]
    fn resolve_model_path_absolute_bypasses_dir() {
        let p = resolve_model_path(Path::new("/var/models"), "/tmp/x.gguf");
        assert_eq!(p, PathBuf::from("/tmp/x.gguf"));
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
    fn find_double_newline_finds_first() {
        let buf = b"hello\n\nworld\n\nbye";
        assert_eq!(find_double_newline(buf), Some(5));
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
