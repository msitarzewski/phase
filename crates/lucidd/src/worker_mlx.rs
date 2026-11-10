// SPDX-License-Identifier: AGPL-3.0-or-later

//! MLX-backed inference worker for Apple Silicon.
//!
//! This adapter deliberately uses a separate process boundary from
//! `LlamaCppWorker`: `mlx_lm.server` is a Python HTTP/SSE service with a
//! different command line, readiness contract, response schema, and failure
//! profile than `llama-server`.  The shared Phase surface remains unchanged:
//! [`Worker`], [`JobEvent`], commitments, and signed receipts are reused.
//!
//! The first supported boundary is intentionally narrow:
//!
//! - macOS on arm64 only;
//! - one immutable, canonically hashed local model bundle per worker;
//! - one in-flight inference at a time;
//! - compatibility targets `mlx-lm==0.31.3` and `mlx==0.31.2`, explicitly
//!   unverified until the runtime supplies a stable offline attestation;
//! - `/v1/chat/completions` and `/v1/completions` SSE, always with
//!   `model: "default_model"`;
//! - no embeddings, adapters, draft models, remote model identifiers, or
//!   `trust-remote-code`.
//!
//! This code is an adapter spike, not hardware acceptance.  No real Metal
//! model run is implied by constructing or compiling this worker; callers can
//! surface [`MLX_HARDWARE_ACCEPTANCE`] verbatim in status output.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_stream::stream;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use phase_identity::NodeIdentity;
use phase_protocol::{
    CommitmentAccumulator, Completion, InferenceJobSpec, JobEvent, JobHandle, JobHandleProducer,
    JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream, OutputChunk, SamplingParams,
    SignedManifest, Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

/// Compatibility target for the experimental adapter. This is not a runtime
/// attestation: `mlx_lm.server` has no stable offline version-reporting
/// contract that binds both Python packages to the entry point being launched.
pub const TARGET_MLX_LM_VERSION: &str = "0.31.3";
/// Compatibility target for the experimental adapter; see
/// [`MLX_RUNTIME_ATTESTATION`].
pub const TARGET_MLX_VERSION: &str = "0.31.2";
pub const MLX_HARDWARE_ACCEPTANCE: &str =
    "unverified: no real Metal/model acceptance run is recorded";
pub const MLX_RUNTIME_ATTESTATION: &str = "unverified: mlx_lm.server has no stable offline probe that binds the installed mlx-lm and mlx package versions to the launched entry point; only the entry-point bytes are pinned";
pub const MLX_PORT_BINDING_STATUS: &str = "guarded, not race-free: Phase reserves the loopback port immediately before spawn, but mlx_lm.server cannot inherit that listener";
/// Signed alias/provider format for the canonical bundle-root contract below.
pub const MLX_BUNDLE_FORMAT: &str = "mlx-bundle-v1";
/// Human-readable identifier for the exact root calculation. The bytes fed to
/// SHA-256 start with `phase/mlx-bundle-root/v1\0` and are described by
/// [`inspect_mlx_bundle`].
pub const MLX_BUNDLE_ROOT_ALGORITHM: &str = "sha256:phase/mlx-bundle-root/v1";

const SUPPORTED_KINDS: [JobSpecKind; 1] = [JobSpecKind::Inference];
const DEFAULT_MAX_TOKENS: u32 = 512;
const ABSOLUTE_MAX_TOKENS: u32 = 8192;
const MAX_PROMPT_CHARS: usize = 1_000_000;
const MAX_MESSAGES: usize = 4_096;
const MAX_SSE_FRAME_BYTES: usize = 64 * 1024;
const SSE_INGEST_SLICE_BYTES: usize = 4 * 1024;
const MAX_STREAM_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WARMUP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_STOP_SEQUENCES: usize = 16;
const MAX_STOP_SEQUENCE_CHARS: usize = 256;
const MAX_TOP_K: u64 = 1_000;
const MAX_SEED: u64 = i32::MAX as u64;
const MAX_BUNDLE_ENTRIES: usize = 4_096;
const MAX_BUNDLE_DEPTH: usize = 16;
const MAX_BUNDLE_PATH_BYTES: usize = 1_024;
const MAX_RUNTIME_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MODEL_CONFIG_BYTES: u64 = 1024 * 1024;
const FILE_HASH_BUFFER_BYTES: usize = 1024 * 1024;
const CHILD_IO_BUFFER_BYTES: usize = 4 * 1024;
const BUNDLE_ROOT_DOMAIN: &[u8] = b"phase/mlx-bundle-root/v1\0";

use crate::registry::{ModelCid, MAX_MODEL_SIZE_BYTES};

/// Verified, bounded metadata derived from bundle bytes rather than CLI
/// strings. Callers can safely use this for signed alias metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MlxBundleMetadata {
    pub model_cid: ModelCid,
    pub total_bytes: u64,
    pub file_count: u32,
    /// Conservative context limit read from the hashed `config.json` when the
    /// model declares one unambiguously. `None` means it must not be advertised
    /// from this adapter.
    pub context_length: Option<u32>,
}

/// Configuration for one pinned MLX runtime and one verified model bundle.
///
/// `model_cid` must equal the independently recomputed canonical bundle root.
/// The worker verifies it during construction and before every process spawn;
/// it never converts an untrusted request model into a filesystem path.
#[derive(Debug, Clone)]
pub struct MlxConfig {
    pub server_binary_path: PathBuf,
    pub verified_model_bundle_path: PathBuf,
    pub model_cid: ModelCid,
    /// `0` asks Phase to choose a currently free ephemeral loopback port. A
    /// fixed nonzero port is rejected if already occupied. There remains a
    /// small bind race because mlx_lm cannot inherit Phase's listener.
    pub server_port: u16,
    pub max_output_tokens: u32,
    pub model_load_timeout: Duration,
    pub request_start_timeout: Duration,
    pub per_request_idle_timeout: Duration,
    pub request_deadline: Duration,
    pub shutdown_timeout: Duration,
    /// Number of retries after the first failed start.  Capped at three.
    pub max_start_retries: u8,
    pub restart_backoff: Duration,
}

impl MlxConfig {
    pub fn new(
        server_binary_path: PathBuf,
        verified_model_bundle_path: PathBuf,
        model_cid: ModelCid,
        server_port: u16,
    ) -> Self {
        Self {
            server_binary_path,
            verified_model_bundle_path,
            model_cid,
            server_port,
            max_output_tokens: ABSOLUTE_MAX_TOKENS,
            model_load_timeout: Duration::from_secs(120),
            request_start_timeout: Duration::from_secs(30),
            per_request_idle_timeout: Duration::from_secs(30),
            request_deadline: Duration::from_secs(15 * 60),
            shutdown_timeout: Duration::from_secs(3),
            max_start_retries: 2,
            restart_backoff: Duration::from_secs(1),
        }
    }
}

#[derive(Clone)]
pub struct MlxWorker {
    inner: Arc<Inner>,
}

struct Inner {
    identity: NodeIdentity,
    config: MlxConfig,
    bundle_snapshot: Arc<BundleSnapshot>,
    runtime_snapshot: RuntimeSnapshot,
    client: reqwest::Client,
    server: Mutex<Option<Arc<MlxServer>>>,
    capacity: Arc<Semaphore>,
}

impl MlxWorker {
    /// Validate the platform, pinned runtime, and verified bundle before any
    /// subprocess can be spawned.
    pub fn new(identity: NodeIdentity, config: MlxConfig) -> Result<Self, WorkerError> {
        let validated = validate_config(config)?;
        let config = validated.config;
        let client = reqwest::Client::builder()
            // Never let a configured system proxy receive local prompts.
            .no_proxy()
            // A loopback backend must not redirect a prompt elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.request_start_timeout)
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .build()
            .map_err(|_| WorkerError::Other("failed to construct MLX HTTP client".into()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                identity,
                config,
                bundle_snapshot: Arc::new(validated.bundle_snapshot),
                runtime_snapshot: validated.runtime_snapshot,
                client,
                server: Mutex::new(None),
                capacity: Arc::new(Semaphore::new(1)),
            }),
        })
    }

    pub fn hardware_acceptance(&self) -> &'static str {
        MLX_HARDWARE_ACCEPTANCE
    }

    pub fn model_cid(&self) -> ModelCid {
        self.inner.config.model_cid
    }

    pub fn bundle_metadata(&self) -> MlxBundleMetadata {
        self.inner.bundle_snapshot.metadata()
    }

    pub fn bundle_format(&self) -> &'static str {
        MLX_BUNDLE_FORMAT
    }

    pub fn max_output_tokens(&self) -> u32 {
        self.inner.config.max_output_tokens
    }

    pub fn advertised_capacity(&self) -> u32 {
        1
    }

    pub fn runtime_executable_sha256(&self) -> [u8; 32] {
        self.inner.runtime_snapshot.sha256
    }

    pub fn runtime_attestation(&self) -> &'static str {
        MLX_RUNTIME_ATTESTATION
    }

    pub fn port_binding_status(&self) -> &'static str {
        MLX_PORT_BINDING_STATUS
    }

    /// Spawn the pinned backend if necessary and complete its bounded health
    /// check plus one-token model warm-up. Callers must await this before
    /// publishing a "loaded" model capability.
    pub async fn preload(&self) -> Result<(), WorkerError> {
        self.ensure_loaded().await.map(|_| ())
    }

    async fn ensure_loaded(&self) -> Result<Arc<MlxServer>, WorkerError> {
        let mut slot = self.inner.server.lock().await;
        if let Some(server) = slot.as_ref() {
            if !server.is_failed() {
                return Ok(server.clone());
            }
        }
        if let Some(stale) = slot.take() {
            stale.shutdown(self.inner.config.shutdown_timeout).await;
        }

        let attempts = u32::from(self.inner.config.max_start_retries) + 1;
        let mut last_failure = "MLX backend did not become ready";
        for attempt in 0..attempts {
            let config = self.inner.config.clone();
            let expected_bundle = self.inner.bundle_snapshot.clone();
            let expected_runtime = self.inner.runtime_snapshot.clone();
            let reverified = tokio::task::spawn_blocking(move || {
                reverify_spawn_inputs(&config, &expected_bundle, &expected_runtime)
            })
            .await
            .map_err(|_| WorkerError::Other("MLX verification task failed".into()))?;
            reverified?;

            let server = match spawn_mlx_server(&self.inner.config) {
                Ok(server) => Arc::new(server),
                Err(error) => {
                    tracing::warn!(attempt, error = %error, "MLX backend spawn failed");
                    last_failure = "failed to start MLX backend";
                    if attempt + 1 < attempts {
                        tokio::time::sleep(
                            self.inner
                                .config
                                .restart_backoff
                                .saturating_mul(attempt + 1),
                        )
                        .await;
                    }
                    continue;
                }
            };

            match wait_for_ready(&self.inner.client, &server, &self.inner.config).await {
                Ok(()) => {
                    *slot = Some(server.clone());
                    return Ok(server);
                }
                Err(error) => {
                    tracing::warn!(attempt, error = %error, "MLX backend readiness failed");
                    last_failure = "MLX backend readiness failed";
                    server.shutdown(self.inner.config.shutdown_timeout).await;
                    if attempt + 1 < attempts {
                        tokio::time::sleep(
                            self.inner
                                .config
                                .restart_backoff
                                .saturating_mul(attempt + 1),
                        )
                        .await;
                    }
                }
            }
        }
        Err(WorkerError::Other(last_failure.into()))
    }
}

impl Worker for MlxWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        &SUPPORTED_KINDS
    }

    fn capacity_hint(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        let manifest_hash = job
            .manifest_hash()
            .map_err(|error| WorkerError::BadManifest(error.to_string()))?;
        let JobSpec::Inference(inference) = &job.payload else {
            return Err(WorkerError::Unsupported {
                kind: job.payload.kind(),
            });
        };

        if inference.model_cid != self.inner.config.model_cid.to_hex() {
            return Err(WorkerError::ArtifactUnavailable("model unavailable".into()));
        }
        let request = build_completion_request(inference, self.inner.config.max_output_tokens)
            .map_err(|error| WorkerError::BadManifest(format!("invalid MLX request: {error}")))?;

        let permit = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| WorkerError::Capacity)?;
        let server = match self.ensure_loaded().await {
            Ok(server) => server,
            Err(error) => {
                drop(permit);
                return Err(error);
            }
        };

        let (handle, producer) = JobHandle::new(JobId(manifest_hash));
        let stream: JobStream = Box::pin(run_inference(
            self.inner.client.clone(),
            server,
            request,
            manifest_hash,
            producer,
            self.inner.identity.clone(),
            self.inner.config.clone(),
            permit,
        ));
        Ok((handle, stream))
    }
}

struct ValidatedConfig {
    config: MlxConfig,
    bundle_snapshot: BundleSnapshot,
    runtime_snapshot: RuntimeSnapshot,
}

fn validate_config(mut config: MlxConfig) -> Result<ValidatedConfig, WorkerError> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(WorkerError::Other(
            "MLX backend requires macOS on Apple Silicon".into(),
        ));
    }
    if config.max_output_tokens == 0 || config.max_output_tokens > ABSOLUTE_MAX_TOKENS {
        return Err(WorkerError::Other(format!(
            "MLX max_output_tokens must be in 1..={ABSOLUTE_MAX_TOKENS}"
        )));
    }
    if config.max_start_retries > 3 {
        return Err(WorkerError::Other(
            "MLX max_start_retries must be at most 3".into(),
        ));
    }
    for (name, value) in [
        ("model_load_timeout", config.model_load_timeout),
        ("request_start_timeout", config.request_start_timeout),
        ("per_request_idle_timeout", config.per_request_idle_timeout),
        ("request_deadline", config.request_deadline),
        ("shutdown_timeout", config.shutdown_timeout),
    ] {
        if value.is_zero() {
            return Err(WorkerError::Other(format!(
                "MLX {name} must be greater than zero"
            )));
        }
    }
    let (runtime_path, runtime_snapshot) = inspect_runtime(&config.server_binary_path)?;
    let (bundle_path, bundle_snapshot) = scan_bundle(&config.verified_model_bundle_path)?;
    if bundle_snapshot.cid != config.model_cid {
        return Err(WorkerError::ArtifactUnavailable(
            "MLX bundle root does not match configured model CID".into(),
        ));
    }
    config.server_binary_path = runtime_path;
    config.verified_model_bundle_path = bundle_path;
    Ok(ValidatedConfig {
        config,
        bundle_snapshot,
        runtime_snapshot,
    })
}

/// Independently inspect an immutable MLX bundle and compute its canonical
/// content root. The root is SHA-256 over:
///
/// - `phase/mlx-bundle-root/v1\0`;
/// - the big-endian file count and total file bytes;
/// - for every file sorted by canonical relative path bytes: the big-endian
///   path length, path bytes, file size, and SHA-256 of the file bytes.
///
/// Relative path components must be portable ASCII (and therefore canonical
/// UTF-8), which intentionally excludes Unicode-normalization and
/// case-folding ambiguity across filesystems.
pub fn inspect_mlx_bundle(path: &Path) -> Result<MlxBundleMetadata, WorkerError> {
    scan_bundle(path).map(|(_, snapshot)| snapshot.metadata())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSnapshot {
    identity: FileIdentity,
    sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BundleSnapshot {
    cid: ModelCid,
    total_bytes: u64,
    file_count: u32,
    context_length: Option<u32>,
    root_identity: FileIdentity,
    entries: Vec<SnapshotEntry>,
}

impl BundleSnapshot {
    fn metadata(&self) -> MlxBundleMetadata {
        MlxBundleMetadata {
            model_cid: self.cid,
            total_bytes: self.total_bytes,
            file_count: self.file_count,
            context_length: self.context_length,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotEntry {
    relative_path: String,
    kind: SnapshotEntryKind,
    identity: FileIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotEntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    readonly: bool,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    nlink: u64,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    mtime: i64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

fn file_identity(metadata: &Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            nlink: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
    #[cfg(not(unix))]
    {
        FileIdentity {
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
        }
    }
}

fn require_immutable_permissions(metadata: &Metadata, subject: &str) -> Result<(), WorkerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o222 != 0 {
            return Err(WorkerError::ArtifactUnavailable(format!(
                "{subject} must have no write permission bits"
            )));
        }
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(WorkerError::ArtifactUnavailable(format!(
                "{subject} must not be hard-linked"
            )));
        }
    }
    #[cfg(not(unix))]
    if !metadata.permissions().readonly() {
        return Err(WorkerError::ArtifactUnavailable(format!(
            "{subject} must be read-only"
        )));
    }
    Ok(())
}

fn inspect_runtime(path: &Path) -> Result<(PathBuf, RuntimeSnapshot), WorkerError> {
    if !path.is_absolute() {
        return Err(WorkerError::Other(
            "MLX runtime path must be absolute".into(),
        ));
    }
    let supplied_metadata = path
        .symlink_metadata()
        .map_err(|_| WorkerError::Other("MLX runtime is unavailable".into()))?;
    if supplied_metadata.file_type().is_symlink() {
        return Err(WorkerError::Other(
            "MLX runtime path must not be a symbolic link".into(),
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| WorkerError::Other("MLX runtime is unavailable".into()))?;
    let before = canonical
        .symlink_metadata()
        .map_err(|_| WorkerError::Other("MLX runtime is unavailable".into()))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(WorkerError::Other(
            "MLX runtime must be a regular file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 {
            return Err(WorkerError::Other("MLX runtime is not executable".into()));
        }
    }
    require_immutable_permissions(&before, "MLX runtime")
        .map_err(|error| WorkerError::Other(error.to_string()))?;
    if before.len() == 0 || before.len() > MAX_RUNTIME_BYTES {
        return Err(WorkerError::Other(
            "MLX runtime size is outside the supported range".into(),
        ));
    }
    let (sha256, opened_identity) = hash_regular_file(&canonical, &before, MAX_RUNTIME_BYTES)
        .map_err(|error| WorkerError::Other(format!("MLX runtime verification failed: {error}")))?;
    Ok((
        canonical,
        RuntimeSnapshot {
            identity: opened_identity,
            sha256,
        },
    ))
}

fn canonical_bundle_root(path: &Path) -> Result<PathBuf, WorkerError> {
    if !path.is_absolute() {
        return Err(WorkerError::Other(
            "MLX model bundle path must be absolute".into(),
        ));
    }
    let supplied_metadata = path
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    if supplied_metadata.file_type().is_symlink() {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle root must not be a symbolic link".into(),
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    let metadata = canonical
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(WorkerError::ArtifactUnavailable("model unavailable".into()));
    }
    require_immutable_permissions(&metadata, "MLX bundle root")?;
    Ok(canonical)
}

#[derive(Debug)]
struct BundleFileRecord {
    relative_path: String,
    size: u64,
    sha256: [u8; 32],
}

#[derive(Default)]
struct BundleScanState {
    entry_count: usize,
    total_bytes: u64,
    files: Vec<BundleFileRecord>,
    entries: Vec<SnapshotEntry>,
    portable_paths: BTreeSet<String>,
    ambiguity_keys: BTreeSet<String>,
    has_weights: bool,
    has_tokenizer: bool,
    context_length: Option<u32>,
}

fn scan_bundle(path: &Path) -> Result<(PathBuf, BundleSnapshot), WorkerError> {
    let root = canonical_bundle_root(path)?;
    let root_before = root
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    let mut state = BundleScanState::default();
    scan_bundle_directory(&root, &root, "", 0, &mut state)?;
    let root_after = root
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model bundle changed during scan".into()))?;
    if file_identity(&root_before) != file_identity(&root_after) {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle changed during scan".into(),
        ));
    }
    if !state.portable_paths.contains("config.json")
        || !state.portable_paths.contains("tokenizer_config.json")
        || !state.has_weights
        || !state.has_tokenizer
    {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle is incomplete".into(),
        ));
    }
    if state.files.is_empty() || state.total_bytes == 0 {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle contains no content".into(),
        ));
    }

    state.files.sort_by(|left, right| {
        left.relative_path
            .as_bytes()
            .cmp(right.relative_path.as_bytes())
    });
    state.entries.sort_by(|left, right| {
        left.relative_path
            .as_bytes()
            .cmp(right.relative_path.as_bytes())
    });
    let file_count = u32::try_from(state.files.len())
        .map_err(|_| WorkerError::ArtifactUnavailable("model bundle file count overflow".into()))?;
    let mut root_hasher = Sha256::new();
    root_hasher.update(BUNDLE_ROOT_DOMAIN);
    root_hasher.update(u64::from(file_count).to_be_bytes());
    root_hasher.update(state.total_bytes.to_be_bytes());
    for file in &state.files {
        let path = file.relative_path.as_bytes();
        let path_len = u32::try_from(path.len()).map_err(|_| {
            WorkerError::ArtifactUnavailable("model bundle path is too long".into())
        })?;
        root_hasher.update(path_len.to_be_bytes());
        root_hasher.update(path);
        root_hasher.update(file.size.to_be_bytes());
        root_hasher.update(file.sha256);
    }
    let cid = ModelCid(root_hasher.finalize().into());
    if cid.0.iter().all(|byte| *byte == 0) {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle produced an invalid zero CID".into(),
        ));
    }
    Ok((
        root,
        BundleSnapshot {
            cid,
            total_bytes: state.total_bytes,
            file_count,
            context_length: state.context_length,
            root_identity: file_identity(&root_after),
            entries: state.entries,
        },
    ))
}

fn scan_bundle_directory(
    root: &Path,
    directory: &Path,
    relative_directory: &str,
    depth: usize,
    state: &mut BundleScanState,
) -> Result<(), WorkerError> {
    if depth > MAX_BUNDLE_DEPTH {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle exceeds directory depth limit".into(),
        ));
    }
    let before = directory
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    if !before.is_dir() || before.file_type().is_symlink() {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle contains a non-directory traversal entry".into(),
        ));
    }
    require_immutable_permissions(&before, "MLX bundle directory")?;

    let mut children = std::fs::read_dir(directory)
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
    children.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });

    for child in children {
        state.entry_count += 1;
        if state.entry_count > MAX_BUNDLE_ENTRIES {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle exceeds entry limit".into(),
            ));
        }
        let segment = canonical_path_segment(&child.file_name())?;
        let relative_path = if relative_directory.is_empty() {
            segment
        } else {
            format!("{relative_directory}/{segment}")
        };
        if relative_path.len() > MAX_BUNDLE_PATH_BYTES {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle path exceeds length limit".into(),
            ));
        }
        let ambiguity_key = relative_path.to_ascii_lowercase();
        if !state.portable_paths.insert(relative_path.clone())
            || !state.ambiguity_keys.insert(ambiguity_key)
        {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle contains ambiguous paths".into(),
            ));
        }

        let child_path = child.path();
        if !child_path.starts_with(root) {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle path escaped its root".into(),
            ));
        }
        let metadata = child_path
            .symlink_metadata()
            .map_err(|_| WorkerError::ArtifactUnavailable("model unavailable".into()))?;
        if metadata.file_type().is_symlink() {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle contains unsupported symbolic links".into(),
            ));
        }
        require_immutable_permissions(&metadata, "MLX bundle entry")?;

        if metadata.is_dir() {
            state.entries.push(SnapshotEntry {
                relative_path: relative_path.clone(),
                kind: SnapshotEntryKind::Directory,
                identity: file_identity(&metadata),
            });
            scan_bundle_directory(root, &child_path, &relative_path, depth + 1, state)?;
        } else if metadata.is_file() {
            reject_executable_bundle_entry(&relative_path, &metadata)?;
            state.total_bytes = state
                .total_bytes
                .checked_add(metadata.len())
                .filter(|total| *total <= MAX_MODEL_SIZE_BYTES)
                .ok_or_else(|| {
                    WorkerError::ArtifactUnavailable("model bundle exceeds byte limit".into())
                })?;
            let (sha256, identity) =
                hash_regular_file(&child_path, &metadata, MAX_MODEL_SIZE_BYTES).map_err(
                    |error| {
                        WorkerError::ArtifactUnavailable(format!(
                            "model bundle file verification failed: {error}"
                        ))
                    },
                )?;
            if relative_path == "config.json" {
                state.context_length =
                    read_verified_context_length(&child_path, &metadata, sha256, &identity)?;
            }
            state.has_weights |= relative_path.ends_with(".safetensors");
            state.has_tokenizer |= matches!(
                relative_path.as_str(),
                "tokenizer.json" | "tokenizer.model" | "sentencepiece.bpe.model"
            );
            state.entries.push(SnapshotEntry {
                relative_path: relative_path.clone(),
                kind: SnapshotEntryKind::File,
                identity,
            });
            state.files.push(BundleFileRecord {
                relative_path,
                size: metadata.len(),
                sha256,
            });
        } else {
            return Err(WorkerError::ArtifactUnavailable(
                "model bundle contains a special file".into(),
            ));
        }
    }

    let after = directory
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model bundle changed during scan".into()))?;
    if file_identity(&before) != file_identity(&after) {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle changed during scan".into(),
        ));
    }
    Ok(())
}

fn canonical_path_segment(segment: &std::ffi::OsStr) -> Result<String, WorkerError> {
    let Some(segment) = segment.to_str() else {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle path is not valid UTF-8".into(),
        ));
    };
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.len() > 255
        || !segment.is_ascii()
        || segment
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'/' || byte == b'\\' || byte == b':')
    {
        return Err(WorkerError::ArtifactUnavailable(
            "model bundle path is not portable canonical UTF-8".into(),
        ));
    }
    Ok(segment.to_string())
}

fn reject_executable_bundle_entry(
    relative_path: &str,
    metadata: &Metadata,
) -> Result<(), WorkerError> {
    let lowercase = relative_path.to_ascii_lowercase();
    if [".py", ".pyc", ".pyo", ".so", ".dylib", ".dll", ".sh"]
        .iter()
        .any(|suffix| lowercase.ends_with(suffix))
    {
        return Err(WorkerError::ArtifactUnavailable(
            "MLX model bundle must not contain executable code".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err(WorkerError::ArtifactUnavailable(
                "MLX model bundle files must not be executable".into(),
            ));
        }
    }
    Ok(())
}

fn hash_regular_file(
    path: &Path,
    expected: &Metadata,
    max_bytes: u64,
) -> Result<([u8; 32], FileIdentity), String> {
    if !expected.is_file() || expected.file_type().is_symlink() || expected.len() > max_bytes {
        return Err("file type or size changed".into());
    }
    let expected_identity = file_identity(expected);
    let mut file = File::open(path).map_err(|_| "file became unavailable".to_string())?;
    let opened = file
        .metadata()
        .map_err(|_| "opened file metadata is unavailable".to_string())?;
    let opened_identity = file_identity(&opened);
    if expected_identity != opened_identity {
        return Err("file changed before it was opened".into());
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; FILE_HASH_BUFFER_BYTES];
    let mut read_bytes = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "file read failed".to_string())?;
        if count == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(count as u64)
            .filter(|total| *total <= max_bytes)
            .ok_or_else(|| "file exceeds byte limit".to_string())?;
        hasher.update(&buffer[..count]);
    }
    if read_bytes != expected.len() {
        return Err("file size changed while hashing".into());
    }
    let after_open = file
        .metadata()
        .map_err(|_| "post-read metadata is unavailable".to_string())?;
    let after_path = path
        .symlink_metadata()
        .map_err(|_| "file disappeared while hashing".to_string())?;
    if opened_identity != file_identity(&after_open)
        || opened_identity != file_identity(&after_path)
    {
        return Err("file changed while hashing".into());
    }
    Ok((hasher.finalize().into(), opened_identity))
}

fn read_verified_context_length(
    path: &Path,
    expected_metadata: &Metadata,
    expected_sha256: [u8; 32],
    expected_identity: &FileIdentity,
) -> Result<Option<u32>, WorkerError> {
    if expected_metadata.len() > MAX_MODEL_CONFIG_BYTES {
        return Err(WorkerError::ArtifactUnavailable(
            "model config exceeds size limit".into(),
        ));
    }
    let mut file = File::open(path)
        .map_err(|_| WorkerError::ArtifactUnavailable("model config became unavailable".into()))?;
    if &file_identity(&file.metadata().map_err(|_| {
        WorkerError::ArtifactUnavailable("model config metadata is unavailable".into())
    })?) != expected_identity
    {
        return Err(WorkerError::ArtifactUnavailable(
            "model config changed during scan".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(expected_metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|_| WorkerError::ArtifactUnavailable("model config read failed".into()))?;
    let post_metadata = file.metadata().map_err(|_| {
        WorkerError::ArtifactUnavailable("model config metadata is unavailable".into())
    })?;
    let path_metadata = path
        .symlink_metadata()
        .map_err(|_| WorkerError::ArtifactUnavailable("model config became unavailable".into()))?;
    let observed_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    if &file_identity(&post_metadata) != expected_identity
        || &file_identity(&path_metadata) != expected_identity
        || observed_sha256 != expected_sha256
    {
        return Err(WorkerError::ArtifactUnavailable(
            "model config changed during scan".into(),
        ));
    }
    let config: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| WorkerError::ArtifactUnavailable("model config is not valid JSON".into()))?;
    if json_contains_forbidden_code_key(&config) {
        return Err(WorkerError::ArtifactUnavailable(
            "model config requests unsupported custom code".into(),
        ));
    }
    let direct = config.get("max_position_embeddings");
    let nested = config
        .get("text_config")
        .and_then(|value| value.get("max_position_embeddings"));
    let mut values = [direct, nested]
        .into_iter()
        .flatten()
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    WorkerError::ArtifactUnavailable(
                        "model context length is not a positive u32".into(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    values.sort_unstable();
    values.dedup();
    if values.len() > 1 {
        return Err(WorkerError::ArtifactUnavailable(
            "model config declares conflicting context lengths".into(),
        ));
    }
    Ok(values.into_iter().next())
}

fn json_contains_forbidden_code_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "auto_map" | "custom_pipelines" | "trust_remote_code"
            ) || json_contains_forbidden_code_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(json_contains_forbidden_code_key),
        _ => false,
    }
}

fn reverify_spawn_inputs(
    config: &MlxConfig,
    expected_bundle: &BundleSnapshot,
    expected_runtime: &RuntimeSnapshot,
) -> Result<(), WorkerError> {
    let (runtime_path, runtime) = inspect_runtime(&config.server_binary_path)?;
    if runtime_path != config.server_binary_path || &runtime != expected_runtime {
        return Err(WorkerError::Other(
            "MLX runtime changed after worker construction".into(),
        ));
    }
    let (bundle_path, bundle) = scan_bundle(&config.verified_model_bundle_path)?;
    if bundle_path != config.verified_model_bundle_path
        || bundle.cid != config.model_cid
        || &bundle != expected_bundle
    {
        return Err(WorkerError::ArtifactUnavailable(
            "MLX model bundle changed after worker construction".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Subprocess lifecycle
// ---------------------------------------------------------------------------

struct MlxServer {
    port: u16,
    failed: Arc<AtomicBool>,
    shutdown_tx: mpsc::Sender<ShutdownRequest>,
    supervisor: tokio::task::JoinHandle<()>,
}

struct ShutdownRequest {
    acknowledgement: oneshot::Sender<()>,
}

impl MlxServer {
    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    async fn shutdown(&self, deadline: Duration) {
        self.failed.store(true, Ordering::Release);
        let (acknowledgement, received) = oneshot::channel();
        let request = ShutdownRequest { acknowledgement };
        if matches!(
            timeout(deadline, self.shutdown_tx.send(request)).await,
            Ok(Ok(()))
        ) {
            let _ = timeout(deadline.saturating_mul(3), received).await;
        }
        self.supervisor.abort();
    }
}

impl Drop for MlxServer {
    fn drop(&mut self) {
        self.failed.store(true, Ordering::Release);
        self.supervisor.abort();
    }
}

/// Last-resort cleanup for the `JobStream` drop contract.  Explicit handle
/// cancellation takes the graceful bounded shutdown path; if the caller drops
/// the stream future outright, `Drop` cannot await, so aborting the supervisor
/// relies on the child's `kill_on_drop(true)` guarantee.
struct StreamServerGuard {
    server: Option<Arc<MlxServer>>,
}

impl StreamServerGuard {
    fn new(server: Arc<MlxServer>) -> Self {
        Self {
            server: Some(server),
        }
    }

    fn disarm(&mut self) {
        self.server = None;
    }
}

impl Drop for StreamServerGuard {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.failed.store(true, Ordering::Release);
            server.supervisor.abort();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(slot) = self.server.try_lock() {
            if let Some(server) = slot.as_ref() {
                server.failed.store(true, Ordering::Release);
                server.supervisor.abort();
            }
        }
    }
}

fn server_arguments(config: &MlxConfig, port: u16) -> Vec<OsString> {
    [
        "--model".into(),
        config.verified_model_bundle_path.as_os_str().to_owned(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string().into(),
        "--allowed-origins".into(),
        "lucid.invalid".into(),
        "--log-level".into(),
        "INFO".into(),
        "--max-tokens".into(),
        config.max_output_tokens.to_string().into(),
        "--decode-concurrency".into(),
        "1".into(),
        "--prompt-concurrency".into(),
        "1".into(),
        "--prefill-step-size".into(),
        "512".into(),
        "--prompt-cache-size".into(),
        "0".into(),
    ]
    .into_iter()
    .collect()
}

fn spawn_mlx_server(config: &MlxConfig) -> std::io::Result<MlxServer> {
    // Holding this listener until immediately before spawn rejects a stale or
    // attacker-controlled service already on the configured port. MLX does
    // not support inheriting a listening FD, so a narrow close-to-bind race
    // remains and is disclosed through `MLX_PORT_BINDING_STATUS`.
    let port_guard = TcpListener::bind((Ipv4Addr::LOCALHOST, config.server_port))?;
    let port = port_guard.local_addr()?.port();
    let mut command = Command::new(&config.server_binary_path);
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("HF_HUB_OFFLINE", "1");
    command.env("TRANSFORMERS_OFFLINE", "1");
    command.env("PYTHONNOUSERSITE", "1");
    command.env("TOKENIZERS_PARALLELISM", "false");
    command.args(server_arguments(config, port));
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    drop(port_guard);
    let child = command.spawn()?;
    Ok(supervise_child(child, port, config.shutdown_timeout))
}

fn supervise_child(mut child: Child, port: u16, shutdown_timeout: Duration) -> MlxServer {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let failed = Arc::new(AtomicBool::new(false));
    let failed_task = failed.clone();
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
    let supervisor = tokio::spawn(async move {
        let stdout_task = stdout.map(|stream| tokio::spawn(discard_child_output(stream)));
        let stderr_task = stderr.map(|stream| tokio::spawn(discard_child_output(stream)));

        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(status) => tracing::warn!(%status, "MLX backend exited"),
                    Err(error) => tracing::warn!(%error, "failed waiting for MLX backend"),
                }
            }
            request = shutdown_rx.recv() => {
                if let Some(request) = request {
                    terminate_child(&mut child, shutdown_timeout).await;
                    let _ = request.acknowledgement.send(());
                } else {
                    terminate_child(&mut child, shutdown_timeout).await;
                }
            }
        }
        failed_task.store(true, Ordering::Release);
        if let Some(task) = stdout_task {
            task.abort();
        }
        if let Some(task) = stderr_task {
            task.abort();
        }
    });
    MlxServer {
        port,
        failed,
        shutdown_tx,
        supervisor,
    }
}

async fn discard_child_output<R>(mut reader: R)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; CHILD_IO_BUFFER_BYTES];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn terminate_child(child: &mut Child, deadline: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }

    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let mut signal = Command::new("/bin/kill");
        signal.env_clear();
        signal.arg("-INT").arg(pid.to_string());
        signal.stdin(Stdio::null());
        signal.stdout(Stdio::null());
        signal.stderr(Stdio::null());
        let _ = timeout(Duration::from_secs(1), signal.status()).await;
    }

    if matches!(timeout(deadline, child.wait()).await, Ok(Ok(_))) {
        return;
    }
    let _ = child.start_kill();
    let _ = timeout(deadline, child.wait()).await;
}

async fn wait_for_ready(
    client: &reqwest::Client,
    server: &MlxServer,
    config: &MlxConfig,
) -> Result<(), String> {
    let health_url = format!("http://127.0.0.1:{}/health", server.port);
    let started = Instant::now();
    let mut last_error = "not reachable".to_string();
    while started.elapsed() < config.model_load_timeout {
        if server.is_failed() {
            return Err("process exited during startup".into());
        }
        match client
            .get(&health_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                return warm_up_model(client, server, config).await;
            }
            Ok(response) => last_error = format!("health status {}", response.status()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!("health deadline exceeded ({last_error})"))
}

async fn warm_up_model(
    client: &reqwest::Client,
    server: &MlxServer,
    config: &MlxConfig,
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/v1/completions", server.port);
    let response = client
        .post(url)
        .timeout(config.model_load_timeout)
        .json(&serde_json::json!({
            "model": "default_model",
            "prompt": " ",
            "stream": false,
            "max_tokens": 1,
            "temperature": 0.0
        }))
        .send()
        .await
        .map_err(|error| format!("warm-up request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("warm-up status {}", response.status()));
    }
    if server.is_failed() {
        return Err("process exited during warm-up".into());
    }
    let body = read_limited_response(response, MAX_WARMUP_RESPONSE_BYTES).await?;
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| "warm-up returned invalid JSON".to_string())?;
    if value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err("warm-up response omitted choices".into());
    }
    Ok(())
}

async fn read_limited_response(response: reqwest::Response, limit: usize) -> Result<Bytes, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("backend response exceeded its size limit".into());
    }
    let mut output = BytesMut::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(limit),
    );
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| "backend response transport failed".to_string())?;
        if chunk.len() > limit.saturating_sub(output.len()) {
            return Err("backend response exceeded its size limit".into());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output.freeze())
}

// ---------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Chat,
    Text,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
            Self::Text => "/v1/completions",
        }
    }
}

#[derive(Debug)]
struct CompletionRequest {
    endpoint: Endpoint,
    body: serde_json::Value,
    prompt_chars: u64,
    max_tokens: u32,
}

fn build_completion_request(
    inference: &InferenceJobSpec,
    configured_max_tokens: u32,
) -> Result<CompletionRequest, String> {
    if inference.messages.len() > MAX_MESSAGES {
        return Err(format!("message count exceeds {MAX_MESSAGES}"));
    }
    if inference
        .messages
        .iter()
        .any(|message| !message.images.is_empty())
    {
        return Err("multimodal messages are not supported by the MLX adapter".into());
    }
    if !inference.messages.is_empty() && inference.prompt.is_some() {
        return Err("prompt and messages are mutually exclusive".into());
    }
    if inference.messages.is_empty()
        && inference
            .prompt
            .as_deref()
            .is_none_or(|prompt| prompt.is_empty())
    {
        return Err("an inference prompt or messages are required".into());
    }

    let prompt_chars = if inference.messages.is_empty() {
        inference
            .prompt
            .as_deref()
            .unwrap_or_default()
            .chars()
            .count()
    } else {
        inference
            .messages
            .iter()
            .map(|message| message.content.chars().count())
            .sum()
    };
    if prompt_chars > MAX_PROMPT_CHARS {
        return Err(format!("prompt exceeds {MAX_PROMPT_CHARS} characters"));
    }

    let max_tokens = inference
        .max_tokens
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .clamp(1, configured_max_tokens.min(ABSOLUTE_MAX_TOKENS));
    let mut body = validated_sampling_params(&inference.sampling)?;
    let endpoint = if inference.messages.is_empty() {
        body.insert(
            "prompt".into(),
            serde_json::Value::String(inference.prompt.clone().unwrap_or_default()),
        );
        Endpoint::Text
    } else {
        let messages = inference
            .messages
            .iter()
            .map(|message| {
                serde_json::json!({
                    "role": match message.role {
                        phase_protocol::ChatRole::System => "system",
                        phase_protocol::ChatRole::User => "user",
                        phase_protocol::ChatRole::Assistant => "assistant",
                        phase_protocol::ChatRole::Tool => "tool",
                    },
                    "content": message.content,
                })
            })
            .collect::<Vec<_>>();
        body.insert("messages".into(), serde_json::Value::Array(messages));
        Endpoint::Chat
    };

    // Insert server-owned controls last.  Untrusted sampling cannot replace
    // the local bundle with a remote identifier or disable streaming/caps.
    body.insert("model".into(), serde_json::json!("default_model"));
    body.insert("stream".into(), serde_json::json!(true));
    body.insert("max_tokens".into(), serde_json::json!(max_tokens));
    Ok(CompletionRequest {
        endpoint,
        body: serde_json::Value::Object(body),
        prompt_chars: prompt_chars as u64,
        max_tokens,
    })
}

fn validated_sampling_params(
    sampling: &SamplingParams,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut output = serde_json::Map::new();
    for (key, encoded) in &sampling.params {
        if matches!(
            key.as_str(),
            "model" | "prompt" | "messages" | "stream" | "max_tokens" | "adapters"
        ) {
            return Err(format!("sampling parameter '{key}' is server-owned"));
        }
        if !matches!(
            key.as_str(),
            "temperature"
                | "top_p"
                | "top_k"
                | "min_p"
                | "repetition_penalty"
                | "presence_penalty"
                | "frequency_penalty"
                | "seed"
                | "stop"
        ) {
            return Err("unsupported sampling parameter".into());
        }
        let value: serde_json::Value = serde_json::from_str(encoded)
            .map_err(|_| format!("sampling parameter '{key}' is not valid JSON"))?;
        match key.as_str() {
            "temperature" => validate_number(key, &value, 0.0, 2.0, true)?,
            "top_p" | "min_p" => validate_number(key, &value, 0.0, 1.0, true)?,
            "repetition_penalty" => validate_number(key, &value, 0.0, 2.0, false)?,
            "presence_penalty" | "frequency_penalty" => {
                validate_number(key, &value, -2.0, 2.0, true)?
            }
            "top_k" => validate_unsigned_integer(key, &value, MAX_TOP_K)?,
            "seed" => validate_unsigned_integer(key, &value, MAX_SEED)?,
            "stop" => validate_stop_sequences(&value)?,
            _ => return Err("unsupported sampling parameter".into()),
        }
        output.insert(key.clone(), value);
    }
    Ok(output)
}

fn validate_number(
    key: &str,
    value: &serde_json::Value,
    min: f64,
    max: f64,
    include_min: bool,
) -> Result<(), String> {
    let Some(number) = value.as_f64() else {
        return Err(format!("sampling parameter '{key}' must be numeric"));
    };
    if !number.is_finite()
        || if include_min {
            number < min || number > max
        } else {
            number <= min || number > max
        }
    {
        return Err(format!("sampling parameter '{key}' is out of range"));
    }
    Ok(())
}

fn validate_unsigned_integer(key: &str, value: &serde_json::Value, max: u64) -> Result<(), String> {
    let Some(number) = value.as_u64() else {
        return Err(format!(
            "sampling parameter '{key}' must be an unsigned integer"
        ));
    };
    if number > max {
        return Err(format!("sampling parameter '{key}' is out of range"));
    }
    Ok(())
}

fn validate_stop_sequences(value: &serde_json::Value) -> Result<(), String> {
    let Some(sequences) = value.as_array() else {
        return Err("sampling parameter 'stop' must be an array".into());
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

// ---------------------------------------------------------------------------
// SSE inference
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalReason {
    Stop,
    Length,
}

#[derive(Debug, PartialEq, Eq)]
enum FrameAction {
    Ignore,
    Done,
    Delta {
        text: String,
        terminal: Option<TerminalReason>,
    },
}

#[derive(Debug, Deserialize)]
struct OpenAiFrame {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

fn parse_sse_frame(frame: &[u8], endpoint: Endpoint) -> Result<FrameAction, String> {
    let mut data: Option<&[u8]> = None;
    for raw_line in frame.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(b":") {
            continue;
        }
        if let Some(payload) = line.strip_prefix(b"data:") {
            if data.is_some() {
                return Err("multiple data fields in one SSE event".into());
            }
            data = Some(payload.strip_prefix(b" ").unwrap_or(payload));
        }
    }
    let Some(data) = data else {
        return Ok(FrameAction::Ignore);
    };
    if data == b"[DONE]" {
        return Ok(FrameAction::Done);
    }

    let parsed: OpenAiFrame =
        serde_json::from_slice(data).map_err(|_| "invalid SSE JSON".to_string())?;
    if parsed.error.is_some() {
        return Err("MLX backend returned an SSE error object".into());
    }
    if parsed.choices.is_empty() {
        return if parsed.usage.is_some() {
            Ok(FrameAction::Ignore)
        } else {
            Err("SSE frame omitted choices".into())
        };
    }
    if parsed.choices.len() != 1 {
        return Err("SSE frame must contain exactly one choice".into());
    }
    let choice = parsed.choices.into_iter().next().expect("length checked");
    let terminal = match choice.finish_reason.as_deref() {
        None => None,
        Some("stop") => Some(TerminalReason::Stop),
        Some("length") => Some(TerminalReason::Length),
        Some(_) => return Err("unsupported MLX finish reason".into()),
    };
    let text = match endpoint {
        Endpoint::Chat => {
            let mut text = choice.delta.reasoning_content.unwrap_or_default();
            text.push_str(&choice.delta.content.unwrap_or_default());
            text
        }
        Endpoint::Text => choice.text.unwrap_or_default(),
    };
    Ok(FrameAction::Delta { text, terminal })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SseFrameTooLarge;

impl std::fmt::Display for SseFrameTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MLX SSE frame exceeded {MAX_SSE_FRAME_BYTES} byte limit"
        )
    }
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn append_sse_chunk(buffer: &mut BytesMut, chunk: &[u8]) -> Result<(), SseFrameTooLarge> {
    let mut combined = BytesMut::with_capacity(buffer.len().saturating_add(chunk.len()));
    combined.extend_from_slice(buffer);
    combined.extend_from_slice(chunk);
    let first_size = find_sse_boundary(&combined)
        .map(|(position, _)| position)
        .unwrap_or(combined.len());
    if first_size > MAX_SSE_FRAME_BYTES {
        return Err(SseFrameTooLarge);
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

fn take_next_sse_frame(buffer: &mut BytesMut) -> Result<Option<Bytes>, SseFrameTooLarge> {
    match find_sse_boundary(buffer) {
        Some((position, _)) if position > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        Some((position, delimiter_len)) => {
            let mut framed = buffer.split_to(position + delimiter_len);
            framed.truncate(position);
            Ok(Some(framed.freeze()))
        }
        None if buffer.len() > MAX_SSE_FRAME_BYTES => Err(SseFrameTooLarge),
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_inference(
    client: reqwest::Client,
    server: Arc<MlxServer>,
    request: CompletionRequest,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    identity: NodeIdentity,
    config: MlxConfig,
    permit: OwnedSemaphorePermit,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    let mut stream_server_guard = StreamServerGuard::new(server.clone());
    stream! {
        let _permit = permit;
        let started_at = Instant::now();
        let mut accumulator = CommitmentAccumulator::new();
        let mut completion_chunks = 0u64;
        let mut output_bytes = 0usize;
        let mut sequence = 0u64;
        let mut terminal_reason = None;
        let mut saw_finish = false;
        let url = format!("http://127.0.0.1:{}{}", server.port, request.endpoint.path());

        if server.is_failed() {
            yield finish_event(
                &mut producer, &identity, manifest_hash, accumulator,
                Completion::Error, Some("MLX backend is unavailable".into()),
                request.prompt_chars, completion_chunks, started_at, &config,
            );
            return;
        }

        let response = tokio::select! {
            _ = producer.cancelled() => {
                server.shutdown(config.shutdown_timeout).await;
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Cancelled, None, request.prompt_chars,
                    completion_chunks, started_at, &config,
                );
                return;
            }
            result = timeout(
                config.request_start_timeout,
                client
                    .post(url)
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .json(&request.body)
                    .send(),
            ) => result,
        };

        let response = match response {
            Ok(Ok(response)) if response.status().is_success() => response,
            Ok(Ok(response)) => {
                let status = response.status();
                drop(response);
                server.shutdown(config.shutdown_timeout).await;
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Error, Some(format!("MLX backend returned {status}")),
                    request.prompt_chars, completion_chunks, started_at, &config,
                );
                return;
            }
            Ok(Err(_)) => {
                server.shutdown(config.shutdown_timeout).await;
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Error, Some("MLX backend request failed".into()),
                    request.prompt_chars, completion_chunks, started_at, &config,
                );
                return;
            }
            Err(_) => {
                server.shutdown(config.shutdown_timeout).await;
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Error, Some("MLX backend response deadline exceeded".into()),
                    request.prompt_chars, completion_chunks, started_at, &config,
                );
                return;
            }
        };

        let content_type_ok = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if !content_type_ok {
            drop(response);
            server.shutdown(config.shutdown_timeout).await;
            yield finish_event(
                &mut producer, &identity, manifest_hash, accumulator,
                Completion::Error, Some("MLX backend returned a non-SSE response".into()),
                request.prompt_chars, completion_chunks, started_at, &config,
            );
            return;
        }

        let mut bytes = response.bytes_stream();
        let mut buffer = BytesMut::with_capacity(4096);
        let max_output_frames = u64::from(request.max_tokens).saturating_mul(8).saturating_add(32);

        'streaming: loop {
            if server.is_failed() {
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Error, Some("MLX backend exited during inference".into()),
                    request.prompt_chars, completion_chunks, started_at, &config,
                );
                return;
            }
            let Some(remaining) = config.request_deadline.checked_sub(started_at.elapsed()) else {
                server.shutdown(config.shutdown_timeout).await;
                yield finish_event(
                    &mut producer, &identity, manifest_hash, accumulator,
                    Completion::Error, Some("MLX inference deadline exceeded".into()),
                    request.prompt_chars, completion_chunks, started_at, &config,
                );
                return;
            };
            let wait_for = config.per_request_idle_timeout.min(remaining);
            let next = tokio::select! {
                _ = producer.cancelled() => {
                    server.shutdown(config.shutdown_timeout).await;
                    yield finish_event(
                        &mut producer, &identity, manifest_hash, accumulator,
                        Completion::Cancelled, None, request.prompt_chars,
                        completion_chunks, started_at, &config,
                    );
                    return;
                }
                result = timeout(wait_for, bytes.next()) => result,
            };
            let chunk = match next {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(_))) => {
                    server.shutdown(config.shutdown_timeout).await;
                    yield finish_event(
                        &mut producer, &identity, manifest_hash, accumulator,
                        Completion::Error, Some("MLX SSE transport failed".into()),
                        request.prompt_chars, completion_chunks, started_at, &config,
                    );
                    return;
                }
                Ok(None) => {
                    server.shutdown(config.shutdown_timeout).await;
                    yield finish_event(
                        &mut producer, &identity, manifest_hash, accumulator,
                        Completion::Error, Some("MLX SSE ended before [DONE]".into()),
                        request.prompt_chars, completion_chunks, started_at, &config,
                    );
                    return;
                }
                Err(_) => {
                    server.shutdown(config.shutdown_timeout).await;
                    yield finish_event(
                        &mut producer, &identity, manifest_hash, accumulator,
                        Completion::Error, Some("MLX SSE idle deadline exceeded".into()),
                        request.prompt_chars, completion_chunks, started_at, &config,
                    );
                    return;
                }
            };

            for segment in chunk.chunks(SSE_INGEST_SLICE_BYTES) {
                if append_sse_chunk(&mut buffer, segment).is_err() {
                    server.shutdown(config.shutdown_timeout).await;
                    yield finish_event(
                        &mut producer, &identity, manifest_hash, accumulator,
                        Completion::Error, Some("MLX SSE frame exceeded its size limit".into()),
                        request.prompt_chars, completion_chunks, started_at, &config,
                    );
                    return;
                }
                loop {
                    let frame = match take_next_sse_frame(&mut buffer) {
                        Ok(Some(frame)) => frame,
                        Ok(None) => break,
                        Err(_) => {
                            server.shutdown(config.shutdown_timeout).await;
                            yield finish_event(
                                &mut producer, &identity, manifest_hash, accumulator,
                                Completion::Error, Some("MLX SSE frame exceeded its size limit".into()),
                                request.prompt_chars, completion_chunks, started_at, &config,
                            );
                            return;
                        }
                    };
                    let action = match parse_sse_frame(&frame, request.endpoint) {
                        Ok(action) => action,
                        Err(error) => {
                            tracing::warn!(error = %error, "rejected malformed MLX SSE frame");
                            server.shutdown(config.shutdown_timeout).await;
                            yield finish_event(
                                &mut producer, &identity, manifest_hash, accumulator,
                                Completion::Error, Some("MLX backend returned an invalid SSE frame".into()),
                                request.prompt_chars, completion_chunks, started_at, &config,
                            );
                            return;
                        }
                    };
                    match action {
                        FrameAction::Ignore => {}
                        FrameAction::Done => {
                            break 'streaming;
                        }
                        FrameAction::Delta { text, terminal } => {
                            if saw_finish && (!text.is_empty() || terminal.is_some()) {
                                server.shutdown(config.shutdown_timeout).await;
                                yield finish_event(
                                    &mut producer, &identity, manifest_hash, accumulator,
                                    Completion::Error, Some("MLX backend emitted data after finish".into()),
                                    request.prompt_chars, completion_chunks, started_at, &config,
                                );
                                return;
                            }
                            if !text.is_empty() {
                                output_bytes = match output_bytes.checked_add(text.len()) {
                                    Some(total) if total <= MAX_STREAM_OUTPUT_BYTES => total,
                                    _ => {
                                        server.shutdown(config.shutdown_timeout).await;
                                        yield finish_event(
                                            &mut producer, &identity, manifest_hash, accumulator,
                                            Completion::Error, Some("MLX output exceeded its byte limit".into()),
                                            request.prompt_chars, completion_chunks, started_at, &config,
                                        );
                                        return;
                                    }
                                };
                                if completion_chunks >= max_output_frames {
                                    server.shutdown(config.shutdown_timeout).await;
                                    yield finish_event(
                                        &mut producer, &identity, manifest_hash, accumulator,
                                        Completion::Error, Some("MLX output exceeded its frame limit".into()),
                                        request.prompt_chars, completion_chunks, started_at, &config,
                                    );
                                    return;
                                }
                                let output = OutputChunk {
                                    kind: "token".into(),
                                    data: Bytes::from(text),
                                    seq: sequence,
                                };
                                accumulator.update(&output);
                                sequence += 1;
                                completion_chunks += 1;
                                yield JobEvent::Output(output);
                            }
                            if let Some(reason) = terminal {
                                terminal_reason = Some(reason);
                                saw_finish = true;
                            }
                        }
                    }
                }
            }
        }

        let completion = match terminal_reason.unwrap_or(TerminalReason::Stop) {
            TerminalReason::Stop => Completion::Stop,
            TerminalReason::Length => Completion::Length,
        };
        stream_server_guard.disarm();
        yield finish_event(
            &mut producer, &identity, manifest_hash, accumulator, completion, None,
            request.prompt_chars, completion_chunks, started_at, &config,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_event(
    producer: &mut JobHandleProducer,
    identity: &NodeIdentity,
    manifest_hash: [u8; 32],
    accumulator: CommitmentAccumulator,
    completion: Completion,
    error: Option<String>,
    prompt_tokens: u64,
    completion_tokens: u64,
    started_at: Instant,
    config: &MlxConfig,
) -> JobEvent {
    let (output_commitment, output_chunk_count) = accumulator.finalize();
    let mut extra = BTreeMap::new();
    extra.insert("backend".into(), "mlx".into());
    extra.insert(
        "runtime_compatibility_target".into(),
        format!(
            "mlx-lm={};mlx={}",
            TARGET_MLX_LM_VERSION, TARGET_MLX_VERSION
        ),
    );
    extra.insert("runtime_attestation".into(), MLX_RUNTIME_ATTESTATION.into());
    extra.insert(
        "bundle_root_algorithm".into(),
        MLX_BUNDLE_ROOT_ALGORITHM.into(),
    );
    extra.insert("bundle_cid".into(), config.model_cid.to_hex());
    extra.insert("hardware_acceptance".into(), "unverified".into());
    let result = JobResult {
        job_spec_hash: manifest_hash,
        output_commitment,
        output_chunk_count,
        completion,
        resumption: None,
        metrics: JobMetrics {
            total_duration_ms: started_at.elapsed().as_millis() as u64,
            // `mlx_lm.server` does not expose prompt-token counts before the
            // terminal usage frame.  Character count is a stable lower-trust
            // workload measure, matching the existing llama worker's spike.
            prompt_tokens,
            completion_tokens,
            extra,
        },
    };
    let receipt = ReceiptBuilder::new(result.clone(), manifest_hash)
        .sign_with(identity)
        .expect("signing an in-memory JobResult is infallible");
    producer.deliver_receipt(receipt);
    JobEvent::Final { result, error }
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::convert::Infallible;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::sync::Arc;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use axum::body::Body;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use axum::extract::State;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use axum::http::{header, Response, StatusCode};
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use axum::routing::{get, post};
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use axum::{Json, Router};
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use futures::StreamExt;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use phase_manifest::ManifestBuilder;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use phase_protocol::{EmbeddingJobSpec, JobSpec};
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use tokio::net::TcpListener;

    use super::*;

    fn inference(model_cid: &str) -> InferenceJobSpec {
        InferenceJobSpec {
            model_cid: model_cid.to_string(),
            messages: Vec::new(),
            prompt: Some("hello".into()),
            resume_from: None,
            sampling: SamplingParams::default(),
            max_tokens: Some(8),
            stream: true,
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn signed_job(spec: JobSpec) -> SignedManifest<JobSpec> {
        ManifestBuilder::new(spec)
            .sign_with(&NodeIdentity::generate())
            .expect("sign test manifest")
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        let mut permissions = std::fs::symlink_metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn create_immutable_bundle(root: &Path) -> PathBuf {
        let bundle = root.join("bundle");
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(
            bundle.join("config.json"),
            br#"{"max_position_embeddings":4096}"#,
        )
        .unwrap();
        std::fs::write(bundle.join("tokenizer_config.json"), b"{}").unwrap();
        std::fs::write(bundle.join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(bundle.join("model.safetensors"), b"fixture").unwrap();
        let nested = bundle.join("weights");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("model-00002.safetensors"), b"second").unwrap();
        for file in [
            bundle.join("config.json"),
            bundle.join("tokenizer_config.json"),
            bundle.join("tokenizer.json"),
            bundle.join("model.safetensors"),
            nested.join("model-00002.safetensors"),
        ] {
            set_mode(&file, 0o444);
        }
        set_mode(&nested, 0o555);
        set_mode(&bundle, 0o555);
        bundle
    }

    #[cfg(unix)]
    fn thaw_for_cleanup(path: &Path) {
        let Ok(metadata) = path.symlink_metadata() else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.is_dir() {
            set_mode(path, 0o755);
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    thaw_for_cleanup(&entry.path());
                }
            }
        } else if metadata.is_file() {
            set_mode(path, 0o644);
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn create_runtime_and_bundle(root: &Path, port: u16) -> (PathBuf, PathBuf, MlxConfig) {
        let runtime = root.join("pinned-mlx-lm-server");
        std::fs::write(
            &runtime,
            b"#!/bin/sh\ntrap 'exit 0' INT TERM\nwhile :; do sleep 1; done\n",
        )
        .expect("write fixture runtime");
        set_mode(&runtime, 0o555);

        let bundle = create_immutable_bundle(root);
        let bundle_metadata = inspect_mlx_bundle(&bundle).unwrap();
        let mut config = MlxConfig::new(
            runtime.clone(),
            bundle.clone(),
            bundle_metadata.model_cid,
            port,
        );
        config.model_load_timeout = Duration::from_secs(2);
        config.request_start_timeout = Duration::from_secs(1);
        config.per_request_idle_timeout = Duration::from_millis(75);
        config.request_deadline = Duration::from_secs(1);
        config.shutdown_timeout = Duration::from_millis(100);
        config.max_start_retries = 0;
        config.restart_backoff = Duration::from_millis(1);
        (runtime, bundle, config)
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn attach_fixture_server(worker: &MlxWorker, port: u16) {
        let failed = Arc::new(AtomicBool::new(false));
        let failed_task = failed.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let supervisor = tokio::spawn(async move {
            if let Some(request) = shutdown_rx.recv().await {
                failed_task.store(true, Ordering::Release);
                let _ = request.acknowledgement.send(());
            }
        });
        *worker.inner.server.lock().await = Some(Arc::new(MlxServer {
            port,
            failed,
            shutdown_tx,
            supervisor,
        }));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[derive(Clone)]
    enum FixtureMode {
        Success,
        Hang,
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[derive(Clone)]
    struct FixtureState {
        mode: FixtureMode,
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn fixture_health() -> StatusCode {
        StatusCode::OK
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn fixture_completion(
        State(state): State<FixtureState>,
        Json(body): Json<serde_json::Value>,
    ) -> Response<Body> {
        state.bodies.lock().await.push(body.clone());
        if body.get("stream") == Some(&serde_json::Value::Bool(false)) {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"choices":[{"text":"x"}]}"#))
                .unwrap();
        }
        match state.mode {
            FixtureMode::Success => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(concat!(
                    "data: {\"choices\":[{\"text\":\"hello\",\"finish_reason\":null}]}\n\n",
                    "data: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                )))
                .unwrap(),
            FixtureMode::Hang => {
                let pending = futures::stream::pending::<Result<Bytes, Infallible>>();
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(pending))
                    .unwrap()
            }
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    async fn start_fixture(
        mode: FixtureMode,
    ) -> (
        u16,
        Arc<Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let state = FixtureState {
            mode,
            bodies: bodies.clone(),
        };
        let app = Router::new()
            .route("/health", get(fixture_health))
            .route("/v1/completions", post(fixture_completion))
            .route("/v1/chat/completions", post(fixture_completion))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (port, bodies, task)
    }

    #[cfg(unix)]
    #[test]
    fn canonical_bundle_root_is_deterministic_and_exposes_hashed_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let first_root = directory.path().join("first");
        let second_root = directory.path().join("second");
        std::fs::create_dir(&first_root).unwrap();
        std::fs::create_dir(&second_root).unwrap();
        let first = create_immutable_bundle(&first_root);
        let second = create_immutable_bundle(&second_root);

        let first_metadata = inspect_mlx_bundle(&first).unwrap();
        let second_metadata = inspect_mlx_bundle(&second).unwrap();
        assert_eq!(first_metadata, second_metadata);
        assert_eq!(first_metadata.file_count, 5);
        assert_eq!(first_metadata.context_length, Some(4096));
        assert!(first_metadata.total_bytes > 0);
        assert_eq!(MLX_BUNDLE_FORMAT, "mlx-bundle-v1");
        assert_eq!(MLX_BUNDLE_ROOT_ALGORITHM, "sha256:phase/mlx-bundle-root/v1");

        thaw_for_cleanup(&first);
        thaw_for_cleanup(&second);
    }

    #[cfg(unix)]
    #[test]
    fn bundle_root_changes_with_file_bytes_and_relative_path() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = create_immutable_bundle(directory.path());
        let original = inspect_mlx_bundle(&bundle).unwrap();

        let weights = bundle.join("model.safetensors");
        set_mode(&weights, 0o644);
        std::fs::write(&weights, b"changed").unwrap();
        set_mode(&weights, 0o444);
        let content_changed = inspect_mlx_bundle(&bundle).unwrap();
        assert_ne!(content_changed.model_cid, original.model_cid);

        set_mode(&bundle, 0o755);
        std::fs::rename(&weights, bundle.join("renamed.safetensors")).unwrap();
        set_mode(&bundle, 0o555);
        let path_changed = inspect_mlx_bundle(&bundle).unwrap();
        assert_ne!(path_changed.model_cid, content_changed.model_cid);

        thaw_for_cleanup(&bundle);
    }

    #[cfg(unix)]
    #[test]
    fn bundle_scan_rejects_writable_links_hardlinks_and_ambiguous_paths() {
        use std::os::unix::fs::symlink;

        let writable_root = tempfile::tempdir().unwrap();
        let writable = create_immutable_bundle(writable_root.path());
        set_mode(&writable.join("tokenizer.json"), 0o644);
        assert!(matches!(
            inspect_mlx_bundle(&writable),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("write permission")
        ));
        thaw_for_cleanup(&writable);

        let symlink_root = tempfile::tempdir().unwrap();
        let linked = create_immutable_bundle(symlink_root.path());
        set_mode(&linked, 0o755);
        symlink("config.json", linked.join("config-link.json")).unwrap();
        set_mode(&linked, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&linked),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("symbolic")
        ));
        let root_link = symlink_root.path().join("bundle-root-link");
        symlink(&linked, &root_link).unwrap();
        assert!(matches!(
            inspect_mlx_bundle(&root_link),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("root must not be a symbolic link")
        ));
        thaw_for_cleanup(&linked);

        let hardlink_root = tempfile::tempdir().unwrap();
        let hardlinked = create_immutable_bundle(hardlink_root.path());
        set_mode(&hardlinked, 0o755);
        std::fs::hard_link(
            hardlinked.join("tokenizer.json"),
            hardlinked.join("duplicate-tokenizer.json"),
        )
        .unwrap();
        set_mode(&hardlinked, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&hardlinked),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("hard-linked")
        ));
        thaw_for_cleanup(&hardlinked);

        let ambiguous_root = tempfile::tempdir().unwrap();
        let ambiguous = create_immutable_bundle(ambiguous_root.path());
        set_mode(&ambiguous, 0o755);
        let ambiguous_file = ambiguous.join("bad\\name.json");
        std::fs::write(&ambiguous_file, b"{}").unwrap();
        set_mode(&ambiguous_file, 0o444);
        set_mode(&ambiguous, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&ambiguous),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("portable canonical UTF-8")
        ));
        thaw_for_cleanup(&ambiguous);

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;

            let non_utf8_root = tempfile::tempdir().unwrap();
            let non_utf8 = create_immutable_bundle(non_utf8_root.path());
            set_mode(&non_utf8, 0o755);
            let non_utf8_file = non_utf8.join(std::ffi::OsString::from_vec(vec![b'b', 0xff]));
            std::fs::write(&non_utf8_file, b"x").unwrap();
            set_mode(&non_utf8_file, 0o444);
            set_mode(&non_utf8, 0o555);
            assert!(matches!(
                inspect_mlx_bundle(&non_utf8),
                Err(WorkerError::ArtifactUnavailable(message))
                    if message.contains("not valid UTF-8")
            ));
            thaw_for_cleanup(&non_utf8);
        }
    }

    #[cfg(unix)]
    #[test]
    fn bundle_scan_rejects_conflicting_or_invalid_context_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = create_immutable_bundle(directory.path());
        let config_path = bundle.join("config.json");
        set_mode(&config_path, 0o644);
        std::fs::write(
            &config_path,
            br#"{"max_position_embeddings":4096,"text_config":{"max_position_embeddings":8192}}"#,
        )
        .unwrap();
        set_mode(&config_path, 0o444);
        assert!(matches!(
            inspect_mlx_bundle(&bundle),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("conflicting context")
        ));
        thaw_for_cleanup(&bundle);

        let custom_code_root = tempfile::tempdir().unwrap();
        let custom_code = create_immutable_bundle(custom_code_root.path());
        let config_path = custom_code.join("config.json");
        set_mode(&config_path, 0o644);
        std::fs::write(
            &config_path,
            br#"{"max_position_embeddings":4096,"nested":{"auto_map":{"AutoModel":"model.Custom"}}}"#,
        )
        .unwrap();
        set_mode(&config_path, 0o444);
        assert!(matches!(
            inspect_mlx_bundle(&custom_code),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("custom code")
        ));
        thaw_for_cleanup(&custom_code);

        let executable_root = tempfile::tempdir().unwrap();
        let executable = create_immutable_bundle(executable_root.path());
        set_mode(&executable, 0o755);
        let executable_file = executable.join("hook.json");
        std::fs::write(&executable_file, b"{}").unwrap();
        set_mode(&executable_file, 0o555);
        set_mode(&executable, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&executable),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("must not be executable")
        ));
        thaw_for_cleanup(&executable);

        let source_root = tempfile::tempdir().unwrap();
        let source = create_immutable_bundle(source_root.path());
        set_mode(&source, 0o755);
        let source_file = source.join("modeling_custom.py");
        std::fs::write(&source_file, b"raise RuntimeError('must not load')").unwrap();
        set_mode(&source_file, 0o444);
        set_mode(&source, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&source),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("executable code")
        ));
        thaw_for_cleanup(&source);
    }

    #[cfg(unix)]
    #[test]
    fn bundle_scan_enforces_recursive_count_depth_and_byte_limits() {
        let depth_root = tempfile::tempdir().unwrap();
        let deep = create_immutable_bundle(depth_root.path());
        set_mode(&deep, 0o755);
        let mut cursor = deep.clone();
        let mut created_directories = Vec::new();
        for index in 0..=MAX_BUNDLE_DEPTH {
            cursor = cursor.join(format!("d{index:02}"));
            std::fs::create_dir(&cursor).unwrap();
            created_directories.push(cursor.clone());
        }
        for directory in created_directories.iter().rev() {
            set_mode(directory, 0o555);
        }
        set_mode(&deep, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&deep),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("depth limit")
        ));
        thaw_for_cleanup(&deep);

        let bytes_root = tempfile::tempdir().unwrap();
        let oversized = create_immutable_bundle(bytes_root.path());
        set_mode(&oversized, 0o755);
        let sparse = oversized.join("oversized.safetensors");
        let sparse_file = File::create(&sparse).unwrap();
        sparse_file.set_len(MAX_MODEL_SIZE_BYTES + 1).unwrap();
        drop(sparse_file);
        set_mode(&sparse, 0o444);
        set_mode(&oversized, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&oversized),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("byte limit")
        ));
        thaw_for_cleanup(&oversized);

        let count_root = tempfile::tempdir().unwrap();
        let crowded = create_immutable_bundle(count_root.path());
        set_mode(&crowded, 0o755);
        for index in 0..MAX_BUNDLE_ENTRIES {
            let path = crowded.join(format!("extra-{index:04}.json"));
            File::create(&path).unwrap();
            set_mode(&path, 0o444);
        }
        set_mode(&crowded, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&crowded),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("entry limit")
        ));
        thaw_for_cleanup(&crowded);
    }

    #[test]
    fn request_forces_default_model_and_rejects_model_override() {
        let request = build_completion_request(&inference("cid:test"), 64).unwrap();
        assert_eq!(request.body["model"], "default_model");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["max_tokens"], 8);

        let mut hostile = inference("cid:test");
        hostile
            .sampling
            .params
            .insert("model".into(), r#""remote/repo""#.into());
        assert!(build_completion_request(&hostile, 64).is_err());
        hostile.sampling.params.clear();
        hostile
            .sampling
            .params
            .insert("adapters".into(), r#""../../adapter""#.into());
        assert!(build_completion_request(&hostile, 64).is_err());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn command_is_loopback_single_capacity_and_contains_no_remote_code_flags() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 18123);
        let validated = validate_config(config).unwrap();
        let arguments = server_arguments(&validated.config, 18123)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--host", "127.0.0.1"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--decode-concurrency", "1"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--prompt-concurrency", "1"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--prompt-cache-size", "0"]));
        assert!(!arguments.iter().any(|value| value == "--trust-remote-code"));
        assert!(!arguments.iter().any(|value| value == "--adapter-path"));
        assert!(!arguments
            .iter()
            .any(|value| value.contains("http://") || value.contains("https://")));
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[test]
    fn malicious_sse_is_rejected_fail_closed() {
        assert!(parse_sse_frame(b"data: not-json", Endpoint::Text).is_err());
        assert!(parse_sse_frame(
            br#"data: {"choices":[{"text":"a"},{"text":"b"}]}"#,
            Endpoint::Text,
        )
        .is_err());
        assert!(parse_sse_frame(
            br#"data: {"choices":[{"text":"","finish_reason":"remote_error"}]}"#,
            Endpoint::Text,
        )
        .is_err());
        assert!(parse_sse_frame(b"data: {\"choices\":[]}\ndata: [DONE]", Endpoint::Text,).is_err());
    }

    #[test]
    fn chat_sse_preserves_reasoning_and_content_text() {
        let action = parse_sse_frame(
            br#"data: {"choices":[{"delta":{"reasoning_content":"think ","content":"answer"},"finish_reason":null}]}"#,
            Endpoint::Chat,
        )
        .unwrap();
        assert_eq!(
            action,
            FrameAction::Delta {
                text: "think answer".into(),
                terminal: None,
            }
        );
    }

    #[test]
    fn oversized_and_crlf_sse_frames_are_bounded() {
        let mut buffer = BytesMut::from(vec![b'x'; MAX_SSE_FRAME_BYTES].as_slice());
        assert_eq!(append_sse_chunk(&mut buffer, b"x"), Err(SseFrameTooLarge));
        assert_eq!(buffer.len(), MAX_SSE_FRAME_BYTES);

        let mut crlf = BytesMut::from(&b"data: [DONE]\r\n\r\nrest"[..]);
        let frame = take_next_sse_frame(&mut crlf).unwrap().unwrap();
        assert_eq!(&frame[..], b"data: [DONE]");
        assert_eq!(&crlf[..], b"rest");
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn wrong_model_and_embeddings_fail_before_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 18124);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        let model_cid = worker.model_cid().to_hex();
        assert_eq!(worker.supported_kinds(), &[JobSpecKind::Inference]);
        assert_eq!(worker.capacity_hint(), 1);
        assert_eq!(worker.advertised_capacity(), 1);
        assert_eq!(worker.bundle_format(), MLX_BUNDLE_FORMAT);
        assert_eq!(worker.bundle_metadata().context_length, Some(4096));
        assert_eq!(worker.runtime_attestation(), MLX_RUNTIME_ATTESTATION);
        assert_eq!(worker.hardware_acceptance(), MLX_HARDWARE_ACCEPTANCE);
        assert_eq!(worker.port_binding_status(), MLX_PORT_BINDING_STATUS);
        assert_ne!(worker.runtime_executable_sha256(), [0; 32]);

        let wrong = signed_job(JobSpec::Inference(inference(
            &ModelCid([0x99; 32]).to_hex(),
        )));
        assert!(matches!(
            worker.execute(wrong).await,
            Err(WorkerError::ArtifactUnavailable(_))
        ));

        let embedding = signed_job(JobSpec::Embedding(EmbeddingJobSpec {
            model_cid,
            input: vec!["hello".into()],
        }));
        assert!(matches!(
            worker.execute(embedding).await,
            Err(WorkerError::Unsupported {
                kind: JobSpecKind::Embedding
            })
        ));
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn configured_cid_mismatch_and_post_construction_bundle_mutation_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        let mut wrong = config.clone();
        wrong.model_cid = ModelCid([0x55; 32]);
        assert!(matches!(
            MlxWorker::new(NodeIdentity::generate(), wrong),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("does not match")
        ));

        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        let model_cid = worker.model_cid().to_hex();
        let weights = bundle.join("model.safetensors");
        set_mode(&weights, 0o644);
        std::fs::write(&weights, b"mutated-after-construction").unwrap();
        set_mode(&weights, 0o444);
        let result = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await;
        assert!(matches!(
            result,
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("changed after worker construction")
        ));
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn post_construction_runtime_mutation_fails_before_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        let model_cid = worker.model_cid().to_hex();
        set_mode(&runtime, 0o755);
        std::fs::write(
            &runtime,
            b"#!/bin/sh\n# mutated\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        set_mode(&runtime, 0o555);
        let result = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await;
        assert!(matches!(
            result,
            Err(WorkerError::Other(message))
                if message.contains("changed after worker construction")
        ));
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn mutable_and_symlinked_runtime_entry_points_are_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        set_mode(&runtime, 0o755);
        assert!(matches!(
            MlxWorker::new(NodeIdentity::generate(), config.clone()),
            Err(WorkerError::Other(message)) if message.contains("write permission")
        ));

        set_mode(&runtime, 0o555);
        let runtime_link = directory.path().join("runtime-link");
        symlink(&runtime, &runtime_link).unwrap();
        let mut linked = config;
        linked.server_binary_path = runtime_link;
        assert!(matches!(
            MlxWorker::new(NodeIdentity::generate(), linked),
            Err(WorkerError::Other(message)) if message.contains("symbolic link")
        ));
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn preload_rejects_occupied_fixed_port_before_loaded_state() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), port);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        let result = worker.preload().await;
        assert!(matches!(
            result,
            Err(WorkerError::Other(message)) if message.contains("failed to start")
        ));
        drop(listener);
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn success_uses_default_model_and_signs_streamed_output() {
        let (port, bodies, fixture) = start_fixture(FixtureMode::Success).await;
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        attach_fixture_server(&worker, port).await;
        worker
            .preload()
            .await
            .expect("an already ready backend must satisfy preload");
        let model_cid = worker.model_cid().to_hex();
        let (handle, mut stream) = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await
            .unwrap();

        let output = stream.next().await.unwrap();
        assert!(matches!(
            output,
            JobEvent::Output(OutputChunk { ref data, seq: 0, .. }) if data == "hello"
        ));
        let final_event = stream.next().await.unwrap();
        let JobEvent::Final { result, error } = final_event else {
            panic!("expected final event");
        };
        assert_eq!(result.completion, Completion::Stop);
        assert_eq!(result.output_chunk_count, 1);
        assert_eq!(result.metrics.extra["hardware_acceptance"], "unverified");
        assert_eq!(
            result.metrics.extra["bundle_root_algorithm"],
            MLX_BUNDLE_ROOT_ALGORITHM
        );
        assert_eq!(result.metrics.extra["bundle_cid"], model_cid);
        assert_eq!(
            result.metrics.extra["runtime_attestation"],
            MLX_RUNTIME_ATTESTATION
        );
        assert!(error.is_none());
        assert!(stream.next().await.is_none());
        let receipt = handle.finish().await.unwrap();
        assert_eq!(receipt.result.output_commitment, result.output_commitment);

        let captured = bodies.lock().await;
        assert_eq!(captured.len(), 1);
        assert!(captured.iter().all(|body| body["model"] == "default_model"));
        fixture.abort();
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn capacity_is_one_and_cancellation_kills_the_server() {
        let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, mut config) = create_runtime_and_bundle(directory.path(), 0);
        config.per_request_idle_timeout = Duration::from_secs(2);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        attach_fixture_server(&worker, port).await;
        let model_cid = worker.model_cid().to_hex();
        let (handle, mut first_stream) = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await
            .unwrap();

        let second = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await;
        assert!(matches!(second, Err(WorkerError::Capacity)));

        handle.cancel();
        let event = timeout(Duration::from_secs(1), first_stream.next())
            .await
            .expect("cancellation must be bounded")
            .expect("final event");
        assert!(matches!(
            event,
            JobEvent::Final {
                result: JobResult {
                    completion: Completion::Cancelled,
                    ..
                },
                error: None,
            }
        ));
        let slot = worker.inner.server.lock().await;
        assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
        drop(slot);
        fixture.abort();
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn dropping_stream_invalidates_and_kills_the_server() {
        let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        attach_fixture_server(&worker, port).await;
        let model_cid = worker.model_cid().to_hex();
        let (_handle, stream) = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await
            .unwrap();

        drop(stream);
        let slot = worker.inner.server.lock().await;
        assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
        drop(slot);
        fixture.abort();
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    async fn hung_sse_hits_idle_deadline_and_invalidates_server() {
        let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
        let directory = tempfile::tempdir().unwrap();
        let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
        let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
        attach_fixture_server(&worker, port).await;
        let model_cid = worker.model_cid().to_hex();
        let (_handle, mut stream) = worker
            .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
            .await
            .unwrap();

        let event = timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("idle watchdog must be bounded")
            .expect("final event");
        assert!(matches!(
            event,
            JobEvent::Final {
                result: JobResult {
                    completion: Completion::Error,
                    ..
                },
                error: Some(_),
            }
        ));
        let slot = worker.inner.server.lock().await;
        assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
        drop(slot);
        fixture.abort();
        set_mode(&runtime, 0o755);
        thaw_for_cleanup(&bundle);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn non_apple_platform_fails_configuration_before_spawn() {
        let config = MlxConfig::new(
            PathBuf::from("/does/not/matter"),
            PathBuf::from("/does/not/matter"),
            ModelCid([1; 32]),
            18124,
        );
        assert!(matches!(
            MlxWorker::new(NodeIdentity::generate(), config),
            Err(WorkerError::Other(message)) if message.contains("macOS on Apple Silicon")
        ));
    }
}
