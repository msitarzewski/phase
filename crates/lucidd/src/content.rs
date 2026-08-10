// SPDX-License-Identifier: AGPL-3.0-or-later

//! Verified, resumable model-content coordination for LUCID.
//!
//! Network framing stays workload-neutral in `phase-net`; this module joins
//! those byte streams to LUCID's signed alias registry and Phase's existing
//! content-addressed [`ArtifactStore`]. No peer supplies a URL or filesystem
//! path. Only an exact signed CID/size mapping can become worker-visible.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _};
use phase_artifact_server::{ArtifactStore, BlobId};
use phase_net::{
    BlobStreamFrame, BlobStreamFrameKind, BlobStreamHandler, BlobStreamRequest, Discovery, PeerId,
    BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS, BLOB_STREAM_MAX_CHUNK_BYTES, BLOB_STREAM_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};

use crate::registry::{
    normalize_model_alias, ContentProviderCandidate, InstalledModel, ModelCapabilities, ModelCid,
    ModelRegistry, ResolvedAlias,
};

const DEFAULT_MAX_CONCURRENT_PULLS: usize = 2;
const DEFAULT_MAX_CONCURRENT_SERVES: usize = 4;
const DEFAULT_MAX_CONCURRENT_SERVES_PER_PEER: usize = 1;
const DEFAULT_MAX_PROVIDER_ATTEMPTS: usize = 8;
const DEFAULT_MAX_MODEL_BYTES: u64 = 128 * 1024 * 1024 * 1024;
const DEFAULT_MAX_STAGING_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const DEFAULT_PULL_DEADLINE: Duration = Duration::from_secs(60 * 60);
const PER_PROVIDER_DEADLINE: Duration = Duration::from_secs(15 * 60);
const MAX_DIAGNOSTIC_CHARS: usize = 512;
const CID_LOCK_STRIPES: usize = 64;
const MAX_PROGRESS_UPDATES_PER_PROVIDER: u64 = 1_024;
const CONTENT_CATALOG_SCHEMA_VERSION: u32 = 1;
const CONTENT_CATALOG_FILE: &str = ".lucidd-content-catalog-v1.json";
const CONTENT_CATALOG_TEMP_FILE: &str = ".lucidd-content-catalog-v1.json.tmp";
const MAX_CONTENT_CATALOG_ENTRIES: usize = 1_024;
const MAX_CONTENT_CATALOG_BYTES: u64 = 512 * 1_024;
const BLOB_SERVE_PEER_STRIPES: usize = 256;
const MAX_CONCURRENT_SERVES: usize = 1_024;
const MAX_STAGING_FILES: usize = 4_096;

#[derive(Debug, Clone)]
pub struct ContentPlaneConfig {
    pub max_concurrent_pulls: usize,
    /// Maximum content streams admitted concurrently after phase-net's
    /// protocol-level stream admission succeeds.
    pub max_concurrent_serves: usize,
    /// Maximum concurrent content streams in one bounded peer-id stripe. A
    /// fixed stripe table avoids an attacker-controlled peer map.
    pub max_concurrent_serves_per_peer: usize,
    pub max_provider_attempts: usize,
    pub max_model_bytes: u64,
    /// Total conservative allowance for resumable staging files. Existing
    /// on-disk partials are included at startup and an active pull reserves
    /// its complete advertised size until commit succeeds.
    pub max_staging_bytes: u64,
    pub pull_deadline: Duration,
    /// Explicit opt-in to advertise successfully installed content as
    /// transferable. Cache-only consumers leave this false.
    pub publish_provider: bool,
    /// Optional local llama.cpp activation profile. When present, a verified
    /// GGUF restored or pulled into the immutable worker directory is also
    /// advertised as executable by this node. Consume-only/content-cache
    /// nodes leave this `None`, preserving the installed-vs-loaded boundary.
    pub local_gguf_activation: Option<LocalGgufActivation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalGgufActivation {
    pub context_length: u32,
    pub max_concurrent: u32,
    pub backend: String,
}

impl Default for ContentPlaneConfig {
    fn default() -> Self {
        Self {
            max_concurrent_pulls: DEFAULT_MAX_CONCURRENT_PULLS,
            max_concurrent_serves: DEFAULT_MAX_CONCURRENT_SERVES,
            max_concurrent_serves_per_peer: DEFAULT_MAX_CONCURRENT_SERVES_PER_PEER,
            max_provider_attempts: DEFAULT_MAX_PROVIDER_ATTEMPTS,
            max_model_bytes: DEFAULT_MAX_MODEL_BYTES,
            max_staging_bytes: DEFAULT_MAX_STAGING_BYTES,
            pull_deadline: DEFAULT_PULL_DEADLINE,
            publish_provider: false,
            local_gguf_activation: None,
        }
    }
}

impl ContentPlaneConfig {
    fn validate(&self) -> Result<(), ContentError> {
        if self.max_concurrent_pulls == 0 || self.max_concurrent_pulls > 32 {
            return Err(ContentError::Configuration(
                "max_concurrent_pulls must be within 1..=32".to_string(),
            ));
        }
        if self.max_concurrent_serves == 0 || self.max_concurrent_serves > MAX_CONCURRENT_SERVES {
            return Err(ContentError::Configuration(format!(
                "max_concurrent_serves must be within 1..={MAX_CONCURRENT_SERVES}"
            )));
        }
        if self.max_concurrent_serves_per_peer == 0
            || self.max_concurrent_serves_per_peer > self.max_concurrent_serves
        {
            return Err(ContentError::Configuration(
                "max_concurrent_serves_per_peer must be within 1..=max_concurrent_serves"
                    .to_string(),
            ));
        }
        if self.max_provider_attempts == 0 || self.max_provider_attempts > 64 {
            return Err(ContentError::Configuration(
                "max_provider_attempts must be within 1..=64".to_string(),
            ));
        }
        if self.max_model_bytes == 0 || self.max_model_bytes > crate::registry::MAX_MODEL_SIZE_BYTES
        {
            return Err(ContentError::Configuration(
                "max_model_bytes is outside the registry allocation bound".to_string(),
            ));
        }
        if self.max_staging_bytes == 0
            || self.max_staging_bytes > crate::registry::MAX_MODEL_SIZE_BYTES.saturating_mul(32)
        {
            return Err(ContentError::Configuration(
                "max_staging_bytes is outside the bounded staging allocation range".to_string(),
            ));
        }
        if self.pull_deadline < Duration::from_secs(1) || self.pull_deadline > DEFAULT_PULL_DEADLINE
        {
            return Err(ContentError::Configuration(
                "pull_deadline must be within 1s..=1h".to_string(),
            ));
        }
        if let Some(activation) = &self.local_gguf_activation {
            if activation.context_length == 0 {
                return Err(ContentError::Configuration(
                    "local GGUF context_length must be nonzero".to_string(),
                ));
            }
            if activation.max_concurrent == 0 || activation.max_concurrent > 1_024 {
                return Err(ContentError::Configuration(
                    "local GGUF max_concurrent must be within 1..=1024".to_string(),
                ));
            }
            if activation.backend != "llama.cpp" {
                return Err(ContentError::Configuration(
                    "local GGUF activation currently supports only the llama.cpp backend"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullProgress {
    Resolving {
        alias: String,
    },
    SelectingProvider {
        cid: String,
        providers: usize,
    },
    Downloading {
        cid: String,
        completed: u64,
        total: u64,
        provider: String,
    },
    Verifying {
        cid: String,
        total: u64,
    },
    Installing {
        cid: String,
    },
    Registering {
        alias: String,
        cid: String,
    },
    Success {
        alias: String,
        cid: String,
        size: u64,
    },
}

/// Optional trust constraints for resolving a mutable human alias. An exact
/// CID pin and/or alias-publisher PeerId narrows the signed candidate set
/// before conflict reconciliation; neither constraint is treated as a content
/// provider preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullSelection {
    pub exact_cid: Option<ModelCid>,
    pub publisher: Option<PeerId>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("invalid content-plane configuration: {0}")]
    Configuration(String),
    #[error("model alias is unknown or has no signed mapping")]
    UnknownAlias,
    #[error("signed alias records conflict on immutable content metadata")]
    AliasConflict,
    #[error("no signed alias record matches the requested CID/publisher pin")]
    PinMismatch,
    #[error("signed alias records advertise unsupported format '{0}'")]
    UnsupportedFormat(String),
    #[error("advertised model size {actual} exceeds the local pull limit {maximum}")]
    ModelTooLarge { actual: u64, maximum: u64 },
    #[error("no authenticated peer advertises the requested content")]
    NoProviders,
    #[error("content transfer failed: {0}")]
    Transfer(String),
    #[error("content pull exceeded its configured total deadline")]
    DeadlineExceeded,
    #[error("all {attempts} bounded provider attempts failed: {last_error}")]
    ProvidersExhausted { attempts: usize, last_error: String },
    #[error("content verification/install failed: {0}")]
    Verification(String),
    #[error("content registration failed: {0}")]
    Registration(String),
    #[error("pull cancelled because the consumer disconnected")]
    Cancelled,
}

/// Bounded, non-waiting admission for already-open content streams. A fixed
/// peer stripe table deliberately trades occasional conservative collisions
/// for memory use that cannot grow with attacker-selected PeerIds.
#[derive(Debug)]
struct BlobServeGate {
    global: Arc<Semaphore>,
    peers: Vec<Arc<Semaphore>>,
}

#[derive(Debug)]
struct BlobServePermit {
    _global: OwnedSemaphorePermit,
    _peer: OwnedSemaphorePermit,
}

impl BlobServeGate {
    fn new(global_limit: usize, per_peer_limit: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            peers: (0..BLOB_SERVE_PEER_STRIPES)
                .map(|_| Arc::new(Semaphore::new(per_peer_limit)))
                .collect(),
        }
    }

    fn peer_index(&self, peer: &PeerId) -> usize {
        let mut hasher = DefaultHasher::new();
        peer.hash(&mut hasher);
        (hasher.finish() as usize) % self.peers.len()
    }

    fn try_acquire(&self, peer: &PeerId) -> Option<BlobServePermit> {
        let global = self.global.clone().try_acquire_owned().ok()?;
        let peer = self.peers[self.peer_index(peer)]
            .clone()
            .try_acquire_owned()
            .ok()?;
        Some(BlobServePermit {
            _global: global,
            _peer: peer,
        })
    }
}

#[derive(Debug, Default)]
struct StagingQuotaState {
    total_accounted: u64,
    tracked_files: usize,
    /// Stable `<cid>.part` files can be associated with a pull and released
    /// after successful commit. Unique ArtifactStore staging files remain in
    /// `unattributed_bytes` and are never deleted by this coordinator.
    stable: HashMap<ModelCid, u64>,
    unattributed_bytes: u64,
}

/// Conservative process-local coordinator layered over an on-disk startup
/// inventory. It never deletes partials: without a cross-process ownership
/// lock, age-based deletion could race another daemon using the same store.
#[derive(Debug)]
struct StagingQuota {
    maximum: u64,
    state: StdMutex<StagingQuotaState>,
}

#[derive(Debug)]
struct StagingQuotaPermit {
    quota: Arc<StagingQuota>,
    cid: ModelCid,
    prior: Option<u64>,
    rollback_on_drop: bool,
}

impl StagingQuota {
    fn from_store(store: &ArtifactStore, maximum: u64) -> Result<Self, ContentError> {
        let state = scan_staging_usage(store)?;
        Ok(Self {
            maximum,
            state: StdMutex::new(state),
        })
    }

    fn reserve(
        self: &Arc<Self>,
        cid: ModelCid,
        expected_size: u64,
    ) -> Result<StagingQuotaPermit, ContentError> {
        let mut state = self.state.lock().map_err(|_| {
            ContentError::Verification("staging quota state is unavailable".to_string())
        })?;
        let prior = state.stable.get(&cid).copied();
        if prior.is_none() && state.tracked_files >= MAX_STAGING_FILES {
            return Err(ContentError::Verification(format!(
                "staging file count reached the {MAX_STAGING_FILES}-file limit"
            )));
        }
        let desired = prior.unwrap_or(0).max(expected_size);
        let additional = desired.saturating_sub(prior.unwrap_or(0));
        let next = state
            .total_accounted
            .checked_add(additional)
            .ok_or_else(|| {
                ContentError::Verification("staging byte accounting overflow".to_string())
            })?;
        // If startup finds the store already over quota, a no-growth pull of
        // an existing stable partial may still finish and remove its staging
        // allocation. New growth remains fail-closed until usage is below the
        // configured bound.
        if additional > 0 && next > self.maximum {
            return Err(ContentError::Verification(format!(
                "staging quota exceeded: {next} bytes requested/accounted, {} bytes allowed",
                self.maximum
            )));
        }
        state.total_accounted = next;
        if prior.is_none() {
            state.tracked_files += 1;
        }
        state.stable.insert(cid, desired);
        drop(state);
        Ok(StagingQuotaPermit {
            quota: self.clone(),
            cid,
            prior,
            rollback_on_drop: true,
        })
    }

    fn restore(&self, cid: ModelCid, prior: Option<u64>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(current) = state.stable.get(&cid).copied() else {
            return;
        };
        state.total_accounted = state.total_accounted.saturating_sub(current);
        match prior {
            Some(bytes) => {
                state.stable.insert(cid, bytes);
                state.total_accounted = state.total_accounted.saturating_add(bytes);
            }
            None => {
                state.stable.remove(&cid);
                state.tracked_files = state.tracked_files.saturating_sub(1);
            }
        }
    }

    fn complete(&self, cid: ModelCid) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(bytes) = state.stable.remove(&cid) {
            state.total_accounted = state.total_accounted.saturating_sub(bytes);
            state.tracked_files = state.tracked_files.saturating_sub(1);
        }
    }
}

impl StagingQuotaPermit {
    /// Once ArtifactStore has returned a stable staging path, cancellation or
    /// transfer failure may leave resumable bytes behind. Keep the complete
    /// expected-size reservation rather than undercounting future growth.
    fn retain_partial(&mut self) {
        self.rollback_on_drop = false;
    }

    fn complete(mut self) {
        self.quota.complete(self.cid);
        self.rollback_on_drop = false;
    }
}

impl Drop for StagingQuotaPermit {
    fn drop(&mut self) {
        if self.rollback_on_drop {
            self.quota.restore(self.cid, self.prior);
        }
    }
}

fn scan_staging_usage(store: &ArtifactStore) -> Result<StagingQuotaState, ContentError> {
    let root = store.base_dir().join("blobs").join(".staging");
    let root_metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StagingQuotaState::default());
        }
        Err(error) => return Err(ContentError::Verification(bounded_diagnostic(error))),
    };
    if !root_metadata.file_type().is_dir() {
        return Err(ContentError::Verification(
            "artifact staging root is not a directory".to_string(),
        ));
    }

    let mut state = StagingQuotaState::default();
    let mut file_count = 0usize;
    let prefix_entries = std::fs::read_dir(&root)
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
    for prefix_entry in prefix_entries {
        let prefix_entry =
            prefix_entry.map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
        let prefix_metadata = std::fs::symlink_metadata(prefix_entry.path())
            .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
        if !prefix_metadata.file_type().is_dir() {
            return Err(ContentError::Verification(
                "artifact staging root contains a non-directory entry".to_string(),
            ));
        }
        let entries = std::fs::read_dir(prefix_entry.path())
            .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
            if !metadata.file_type().is_file() {
                return Err(ContentError::Verification(
                    "artifact staging directory contains a non-regular entry".to_string(),
                ));
            }
            file_count += 1;
            if file_count > MAX_STAGING_FILES {
                return Err(ContentError::Verification(format!(
                    "artifact staging inventory exceeds the {MAX_STAGING_FILES}-file limit"
                )));
            }
            state.tracked_files = file_count;
            state.total_accounted = state
                .total_accounted
                .checked_add(metadata.len())
                .ok_or_else(|| {
                    ContentError::Verification("staging byte accounting overflow".to_string())
                })?;

            let stable_cid = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".part"))
                .filter(|stem| stem.len() == 64)
                .and_then(|stem| ModelCid::from_hex(stem).ok());
            let stable_cid = stable_cid.filter(|cid| {
                BlobId::from_hex(&cid.to_hex())
                    .map(|blob_id| store.staging_path(&blob_id) == path)
                    .unwrap_or(false)
            });
            if let Some(cid) = stable_cid {
                if state.stable.insert(cid, metadata.len()).is_some() {
                    return Err(ContentError::Verification(
                        "duplicate stable staging CID".to_string(),
                    ));
                }
            } else {
                state.unattributed_bytes = state
                    .unattributed_bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| {
                        ContentError::Verification("staging byte accounting overflow".to_string())
                    })?;
            }
        }
    }
    Ok(state)
}

/// Coordinates content serving and pulling while reusing the existing swarm,
/// signed registry, and artifact store.
pub struct ContentPlane {
    network: Arc<Discovery>,
    registry: Arc<ModelRegistry>,
    store: Arc<ArtifactStore>,
    verified_model_dir: PathBuf,
    config: ContentPlaneConfig,
    permits: Arc<Semaphore>,
    serve_gate: Arc<BlobServeGate>,
    staging_quota: Arc<StagingQuota>,
    pull_locks: Vec<Mutex<()>>,
    catalog_lock: Mutex<()>,
}

impl std::fmt::Debug for ContentPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContentPlane")
            .field("verified_model_dir", &self.verified_model_dir)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ContentPlane {
    pub fn new(
        network: Arc<Discovery>,
        registry: Arc<ModelRegistry>,
        store: Arc<ArtifactStore>,
        verified_model_dir: PathBuf,
        config: ContentPlaneConfig,
    ) -> Result<Self, ContentError> {
        config.validate()?;
        let staging_quota = Arc::new(StagingQuota::from_store(&store, config.max_staging_bytes)?);
        Ok(Self {
            network,
            registry,
            store,
            verified_model_dir,
            permits: Arc::new(Semaphore::new(config.max_concurrent_pulls)),
            serve_gate: Arc::new(BlobServeGate::new(
                config.max_concurrent_serves,
                config.max_concurrent_serves_per_peer,
            )),
            staging_quota,
            pull_locks: (0..CID_LOCK_STRIPES).map(|_| Mutex::new(())).collect(),
            catalog_lock: Mutex::new(()),
            config,
        })
    }

    /// Inbound content handler for [`Discovery::set_blob_stream_handler`].
    /// Only exact CIDs in the verified installed-content catalog are served.
    /// Caller policy additionally decides whether this handler is installed.
    pub fn blob_stream_handler(&self) -> BlobStreamHandler {
        let store = self.store.clone();
        let registry = self.registry.clone();
        let serve_gate = self.serve_gate.clone();
        Arc::new(move |peer, request, frames| {
            let store = store.clone();
            let registry = registry.clone();
            let serve_gate = serve_gate.clone();
            Box::pin(async move {
                let Some(_admission) = serve_gate.try_acquire(&peer) else {
                    send_rejection(&frames, request.content_id, "content server busy").await;
                    return;
                };
                let cid = ModelCid(request.content_id);
                let Some(expected_size) = installed_content_size(&registry, cid).await else {
                    send_rejection(&frames, request.content_id, "content unavailable").await;
                    return;
                };
                serve_blob(store, expected_size, request, frames).await;
            })
        })
    }

    pub async fn pull(
        &self,
        alias: &str,
        progress: Option<mpsc::Sender<PullProgress>>,
    ) -> Result<InstalledModel, ContentError> {
        self.pull_selected(alias, PullSelection::default(), progress)
            .await
    }

    pub async fn pull_selected(
        &self,
        alias: &str,
        selection: PullSelection,
        progress: Option<mpsc::Sender<PullProgress>>,
    ) -> Result<InstalledModel, ContentError> {
        let deadline = tokio::time::Instant::now() + self.config.pull_deadline;
        tokio::time::timeout_at(
            deadline,
            self.pull_before_deadline(alias, selection, progress, deadline),
        )
        .await
        .map_err(|_| ContentError::DeadlineExceeded)?
    }

    async fn pull_before_deadline(
        &self,
        alias: &str,
        selection: PullSelection,
        progress: Option<mpsc::Sender<PullProgress>>,
        pull_deadline: tokio::time::Instant,
    ) -> Result<InstalledModel, ContentError> {
        emit_progress(
            &progress,
            PullProgress::Resolving {
                alias: alias.to_string(),
            },
        )
        .await?;

        // Bound the complete operation, including DHT resolution. Otherwise a
        // caller could bypass the transfer cap by flooding alias lookups.
        let _global_permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ContentError::Cancelled)?;
        let aliases = resolve_alias_before_deadline(&self.registry, alias, pull_deadline).await?;
        let selected = select_alias_records(aliases, selection)?;
        let expected = reconcile_alias_records(&selected)?;
        if expected.size > self.config.max_model_bytes {
            return Err(ContentError::ModelTooLarge {
                actual: expected.size,
                maximum: self.config.max_model_bytes,
            });
        }

        // Fixed stripes bound memory while serializing every same-CID pull.
        let _same_cid = self.pull_locks[cid_lock_index(&expected.cid)].lock().await;

        if installed_blob_matches(self.store.clone(), expected.cid, expected.size).await? {
            return self
                .register_installed(&expected.alias, expected.cid, expected.size, &progress)
                .await;
        }

        let advertised =
            find_providers_before_deadline(&self.registry, &expected.cid, pull_deadline).await?;
        let providers = select_content_providers(advertised, &expected);
        if providers.is_empty() {
            return Err(ContentError::NoProviders);
        }
        let attempt_plan = provider_attempt_plan(&providers, self.config.max_provider_attempts);
        let cid_hex = expected.cid.to_hex();
        emit_progress(
            &progress,
            PullProgress::SelectingProvider {
                cid: cid_hex.clone(),
                providers: providers.len().min(self.config.max_provider_attempts),
            },
        )
        .await?;

        let blob_id = BlobId::from_hex(&cid_hex)
            .ok_or_else(|| ContentError::Verification("invalid canonical blob ID".to_string()))?;
        let mut staging_reservation = self.staging_quota.reserve(expected.cid, expected.size)?;
        let (staging_path, mut completed) =
            prepare_staging(self.store.clone(), blob_id.clone(), expected.size).await?;
        staging_reservation.retain_partial();

        // A previous provider may already have completed the stable partial.
        // Verify it before opening a new network stream. Invalid full partials
        // are removed by commit_staged_blob, after which retries start at zero.
        if completed == expected.size {
            emit_progress(
                &progress,
                PullProgress::Verifying {
                    cid: cid_hex.clone(),
                    total: expected.size,
                },
            )
            .await?;
            match commit_staging(
                self.store.clone(),
                staging_path.clone(),
                blob_id.clone(),
                expected.size,
            )
            .await
            {
                Ok(()) => {
                    staging_reservation.complete();
                    return self
                        .register_installed(&expected.alias, expected.cid, expected.size, &progress)
                        .await;
                }
                Err(_) => {
                    (_, completed) =
                        prepare_staging(self.store.clone(), blob_id.clone(), expected.size).await?;
                }
            }
        }

        let mut last_failure = "providers did not complete the transfer".to_string();
        let mut attempts = 0usize;

        for provider in attempt_plan {
            attempts += 1;
            if tokio::time::Instant::now() >= pull_deadline {
                last_failure = "pull deadline reached".to_string();
                break;
            }
            match download_from_provider(
                &self.network,
                provider,
                expected.cid,
                expected.size,
                &staging_path,
                completed,
                pull_deadline,
                &progress,
            )
            .await
            {
                Ok(new_completed) if new_completed == expected.size => {}
                Ok(_) => {
                    return Err(ContentError::Transfer(
                        "provider completed at an unexpected cursor".to_string(),
                    ));
                }
                Err(ContentError::Cancelled) => return Err(ContentError::Cancelled),
                Err(error) => {
                    last_failure = bounded_diagnostic(error);
                    completed = staging_len(&staging_path, expected.size).await?;
                    continue;
                }
            }

            emit_progress(
                &progress,
                PullProgress::Verifying {
                    cid: cid_hex.clone(),
                    total: expected.size,
                },
            )
            .await?;
            match commit_staging(
                self.store.clone(),
                staging_path.clone(),
                blob_id.clone(),
                expected.size,
            )
            .await
            {
                Ok(()) => {
                    staging_reservation.complete();
                    return self
                        .register_installed(&expected.alias, expected.cid, expected.size, &progress)
                        .await;
                }
                Err(error) => {
                    // Hash failure removes the unsafe full partial. A later
                    // authenticated provider gets a clean attempt.
                    last_failure = bounded_diagnostic(error);
                    (_, completed) =
                        prepare_staging(self.store.clone(), blob_id.clone(), expected.size).await?;
                }
            }
        }

        Err(ContentError::ProvidersExhausted {
            attempts,
            last_error: last_failure,
        })
    }

    /// Restore the consume-only installed catalog after independently
    /// re-hashing every referenced artifact-store blob. The complete catalog
    /// is validated before any entry is registered, so malformed, truncated,
    /// conflicting, or unverifiable input exposes no worker-visible model.
    ///
    /// Provider claims are intentionally not published here: startup must
    /// install the inbound blob handler before opting restored content into
    /// serving.
    pub async fn restore_installed_catalog(&self) -> Result<Vec<InstalledModel>, ContentError> {
        let _catalog = self.catalog_lock.lock().await;
        let verified_dir = self.verified_model_dir.clone();
        let store = self.store.clone();
        let max_model_bytes = self.config.max_model_bytes;
        let entries = tokio::task::spawn_blocking(move || {
            load_and_verify_catalog(&verified_dir, &store, max_model_bytes)
        })
        .await
        .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?
        .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;

        let existing = self.registry.local_installed_async().await;
        let loaded = self.registry.local_models_async().await;
        preflight_catalog_registration(&entries, &existing, &loaded)
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;

        let mut restored = Vec::with_capacity(entries.len());
        for entry in entries {
            let installed = self
                .registry
                .register_verified_gguf_blob(
                    self.store.clone(),
                    self.verified_model_dir.clone(),
                    &entry.alias,
                    entry.cid,
                    entry.size,
                )
                .await
                .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;
            self.activate_installed_gguf(&installed).await?;
            restored.push(installed);
        }
        Ok(restored)
    }

    async fn register_installed(
        &self,
        alias: &str,
        cid: ModelCid,
        size: u64,
        progress: &Option<mpsc::Sender<PullProgress>>,
    ) -> Result<InstalledModel, ContentError> {
        let cid_hex = cid.to_hex();
        emit_progress(
            progress,
            PullProgress::Installing {
                cid: cid_hex.clone(),
            },
        )
        .await?;
        emit_progress(
            progress,
            PullProgress::Registering {
                alias: alias.to_string(),
                cid: cid_hex.clone(),
            },
        )
        .await?;
        let was_installed = self
            .registry
            .local_installed_async()
            .await
            .iter()
            .any(|existing| existing.model_id == alias && existing.model_cid == cid);
        let was_loaded = self
            .registry
            .local_models_async()
            .await
            .iter()
            .any(|existing| existing.model_cid == cid);
        let installed = self
            .registry
            .register_verified_gguf_blob(
                self.store.clone(),
                self.verified_model_dir.clone(),
                alias,
                cid,
                size,
            )
            .await
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;
        if let Err(error) = self.activate_installed_gguf(&installed).await {
            self.rollback_registration(&installed, !was_installed, !was_loaded)
                .await;
            return Err(error);
        }
        if let Err(error) = self.persist_installed_catalog().await {
            self.rollback_registration(&installed, !was_installed, !was_loaded)
                .await;
            return Err(error);
        }
        if self.config.publish_provider {
            if let Err(error) = self.registry.publish_installed_content_provider(&cid).await {
                // Installation is already durably committed. A transient DHT
                // publication failure must not turn a successful install into
                // an ambiguous API error; the provider refresh can be retried
                // without changing the verified content state.
                tracing::warn!(
                    cid = %cid.to_hex(),
                    error = %bounded_diagnostic(error),
                    "verified content installed but initial provider advertisement failed"
                );
            }
        }
        emit_progress(
            progress,
            PullProgress::Success {
                alias: alias.to_string(),
                cid: cid_hex,
                size,
            },
        )
        .await?;
        Ok(installed)
    }

    async fn rollback_registration(
        &self,
        installed: &InstalledModel,
        remove_installed: bool,
        remove_loaded: bool,
    ) {
        if remove_loaded {
            if let Err(error) = self.registry.withdraw(&installed.model_cid).await {
                tracing::warn!(
                    cid = %installed.model_cid.to_hex(),
                    error = %bounded_diagnostic(error),
                    "failed to roll back loaded model state after registration failure"
                );
            }
        }
        if remove_installed {
            if let Err(error) = self
                .registry
                .withdraw_installed_content(&installed.model_id)
                .await
            {
                tracing::warn!(
                    alias = %installed.model_id,
                    error = %bounded_diagnostic(error),
                    "failed to roll back installed model state after registration failure"
                );
            }
            if let Err(error) = self.persist_installed_catalog().await {
                tracing::warn!(
                    alias = %installed.model_id,
                    error = %bounded_diagnostic(error),
                    "failed to checkpoint registration rollback"
                );
            }
        }
    }

    async fn activate_installed_gguf(
        &self,
        installed: &InstalledModel,
    ) -> Result<(), ContentError> {
        let Some(activation) = &self.config.local_gguf_activation else {
            return Ok(());
        };
        let capabilities = ModelCapabilities::now(
            installed.model_id.clone(),
            installed.model_cid,
            installed.format.clone(),
            activation.context_length,
            activation.max_concurrent,
            activation.backend.clone(),
        );
        self.registry
            .advertise_loaded(capabilities)
            .await
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))
    }

    /// Atomically checkpoint the registry's complete installed-content
    /// snapshot. Startup uses this after verified local imports and catalog
    /// restore so both sources converge into one bounded durable catalog.
    /// This never publishes a content-provider claim.
    pub async fn persist_installed_catalog(&self) -> Result<(), ContentError> {
        let _catalog = self.catalog_lock.lock().await;
        let installed = self.registry.local_installed_async().await;
        let verified_dir = self.verified_model_dir.clone();
        tokio::task::spawn_blocking(move || write_catalog(&verified_dir, &installed))
            .await
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedContent {
    alias: String,
    cid: ModelCid,
    size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentCatalog {
    schema_version: u32,
    entries: Vec<ContentCatalogEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentCatalogEntry {
    alias: String,
    cid: String,
    size: u64,
    format: String,
}

#[derive(Debug)]
struct VerifiedCatalogEntry {
    alias: String,
    cid: ModelCid,
    size: u64,
}

fn write_catalog(directory: &std::path::Path, installed: &[InstalledModel]) -> anyhow::Result<()> {
    if installed.len() > MAX_CONTENT_CATALOG_ENTRIES {
        bail!("installed content catalog exceeds the {MAX_CONTENT_CATALOG_ENTRIES}-entry limit");
    }
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create verified content directory {directory:?}"))?;
    let catalog_path = directory.join(CONTENT_CATALOG_FILE);
    reject_non_regular_path(&catalog_path, true)?;

    let entries = installed
        .iter()
        .map(|record| ContentCatalogEntry {
            alias: record.model_id.clone(),
            cid: record.model_cid.to_hex(),
            size: record.size_bytes,
            format: record.format.clone(),
        })
        .collect();
    let bytes = serde_json::to_vec(&ContentCatalog {
        schema_version: CONTENT_CATALOG_SCHEMA_VERSION,
        entries,
    })
    .context("serialize installed content catalog")?;
    if bytes.len() as u64 > MAX_CONTENT_CATALOG_BYTES {
        bail!("installed content catalog exceeds the {MAX_CONTENT_CATALOG_BYTES}-byte limit");
    }

    let temporary_path = directory.join(CONTENT_CATALOG_TEMP_FILE);
    match std::fs::symlink_metadata(&temporary_path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::remove_file(&temporary_path)
                .with_context(|| format!("remove stale content catalog {temporary_path:?}"))?;
        }
        Ok(_) => bail!("content catalog temporary path is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect content catalog temporary path"),
    }

    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .with_context(|| format!("create content catalog {temporary_path:?}"))?;
        file.write_all(&bytes).context("write content catalog")?;
        file.sync_all().context("sync content catalog")?;
        drop(file);
        std::fs::rename(&temporary_path, &catalog_path)
            .with_context(|| format!("publish content catalog {catalog_path:?}"))?;
        sync_directory(directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    result
}

fn load_and_verify_catalog(
    directory: &std::path::Path,
    store: &ArtifactStore,
    max_model_bytes: u64,
) -> anyhow::Result<Vec<VerifiedCatalogEntry>> {
    let path = directory.join(CONTENT_CATALOG_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => bail!("installed content catalog is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("inspect installed content catalog"),
    };
    if metadata.len() > MAX_CONTENT_CATALOG_BYTES {
        bail!("installed content catalog exceeds its byte limit");
    }

    let mut file = std::fs::File::open(&path).context("open installed content catalog")?;
    if !file.metadata()?.file_type().is_file() {
        bail!("installed content catalog changed while opening");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_CONTENT_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read installed content catalog")?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_CONTENT_CATALOG_BYTES {
        bail!("installed content catalog changed or exceeded its byte limit while reading");
    }
    let catalog: ContentCatalog =
        serde_json::from_slice(&bytes).context("decode installed content catalog")?;
    if catalog.schema_version != CONTENT_CATALOG_SCHEMA_VERSION {
        bail!(
            "unsupported installed content catalog schema {}",
            catalog.schema_version
        );
    }
    if catalog.entries.len() > MAX_CONTENT_CATALOG_ENTRIES {
        bail!("installed content catalog exceeds its entry limit");
    }

    let mut aliases = HashSet::with_capacity(catalog.entries.len());
    let mut cid_metadata: HashMap<ModelCid, (u64, String)> = HashMap::new();
    let mut verified = Vec::with_capacity(catalog.entries.len());
    for entry in catalog.entries {
        let normalized = normalize_model_alias(&entry.alias).context("validate catalog alias")?;
        if normalized != entry.alias || !aliases.insert(normalized.clone()) {
            bail!("installed content catalog has a non-canonical or duplicate alias");
        }
        if entry.format != "gguf" {
            bail!("installed content catalog contains an unsupported format");
        }
        if entry.size == 0 || entry.size > max_model_bytes {
            bail!("installed content catalog size is outside the configured bound");
        }
        let cid = ModelCid::from_hex(&entry.cid).context("parse catalog CID")?;
        if cid.to_hex() != entry.cid || cid.0.iter().all(|byte| *byte == 0) {
            bail!("installed content catalog CID is not canonical");
        }
        match cid_metadata.entry(cid) {
            std::collections::hash_map::Entry::Occupied(existing)
                if existing.get() != &(entry.size, entry.format.clone()) =>
            {
                bail!("installed content catalog has conflicting CID metadata");
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((entry.size, entry.format.clone()));
            }
        }

        let blob_id = BlobId::from_hex(&entry.cid).context("convert catalog CID to blob ID")?;
        let blob = store
            .get_blob(&blob_id)?
            .context("catalog blob is absent from the artifact store")?;
        let blob_metadata = std::fs::symlink_metadata(&blob.path)
            .context("inspect catalog blob in artifact store")?;
        if !blob_metadata.file_type().is_file() || blob_metadata.len() != entry.size {
            bail!("catalog blob is not an exact regular artifact-store file");
        }
        let (actual_id, actual_size) = ArtifactStore::compute_blob_id(&blob.path)?;
        if actual_id != blob_id || actual_size != entry.size {
            bail!("catalog blob failed independent CID/size verification");
        }
        verified.push(VerifiedCatalogEntry {
            alias: normalized,
            cid,
            size: entry.size,
        });
    }
    Ok(verified)
}

fn preflight_catalog_registration(
    entries: &[VerifiedCatalogEntry],
    installed: &[InstalledModel],
    loaded: &[crate::registry::ModelCapabilities],
) -> anyhow::Result<()> {
    let by_alias = installed
        .iter()
        .map(|record| (record.model_id.as_str(), record))
        .collect::<HashMap<_, _>>();
    let by_cid = installed
        .iter()
        .map(|record| (record.model_cid, record))
        .collect::<HashMap<_, _>>();
    let loaded_by_alias = loaded
        .iter()
        .map(|record| (record.model_id.as_str(), record.model_cid))
        .collect::<HashMap<_, _>>();
    for entry in entries {
        if by_alias.get(entry.alias.as_str()).is_some_and(|record| {
            record.model_cid != entry.cid
                || record.size_bytes != entry.size
                || record.format != "gguf"
        }) {
            bail!("catalog alias conflicts with already installed content");
        }
        if by_cid
            .get(&entry.cid)
            .is_some_and(|record| record.size_bytes != entry.size || record.format != "gguf")
        {
            bail!("catalog CID metadata conflicts with already installed content");
        }
        if loaded_by_alias
            .get(entry.alias.as_str())
            .is_some_and(|cid| *cid != entry.cid)
        {
            bail!("catalog alias conflicts with an already loaded model");
        }
    }
    Ok(())
}

fn reject_non_regular_path(path: &std::path::Path, absence_allowed: bool) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => bail!("content catalog path is not a regular file"),
        Err(error) if absence_allowed && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect content catalog path"),
    }
}

#[cfg(unix)]
fn sync_directory(directory: &std::path::Path) -> anyhow::Result<()> {
    std::fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .context("sync verified content directory")
}

#[cfg(not(unix))]
fn sync_directory(_directory: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

fn reconcile_alias_records(aliases: &[ResolvedAlias]) -> Result<ExpectedContent, ContentError> {
    let first = aliases.first().ok_or(ContentError::UnknownAlias)?;
    if aliases.iter().any(|candidate| {
        candidate.record.model_cid != first.record.model_cid
            || candidate.record.size_bytes != first.record.size_bytes
            || candidate.record.format != first.record.format
    }) {
        return Err(ContentError::AliasConflict);
    }
    if first.record.format != "gguf" {
        return Err(ContentError::UnsupportedFormat(first.record.format.clone()));
    }
    Ok(ExpectedContent {
        alias: first.record.alias.clone(),
        cid: first.record.model_cid,
        size: first.record.size_bytes,
    })
}

fn select_alias_records(
    aliases: Vec<ResolvedAlias>,
    selection: PullSelection,
) -> Result<Vec<ResolvedAlias>, ContentError> {
    if selection.exact_cid.is_none() && selection.publisher.is_none() {
        return Ok(aliases);
    }
    let selected = aliases
        .into_iter()
        .filter(|candidate| {
            selection
                .exact_cid
                .is_none_or(|cid| candidate.record.model_cid == cid)
                && selection
                    .publisher
                    .is_none_or(|publisher| candidate.publisher == publisher)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err(ContentError::PinMismatch);
    }
    Ok(selected)
}

async fn resolve_alias_before_deadline(
    registry: &ModelRegistry,
    alias: &str,
    deadline: tokio::time::Instant,
) -> Result<Vec<ResolvedAlias>, ContentError> {
    tokio::time::timeout_at(deadline, registry.resolve_alias(alias))
        .await
        .map_err(|_| ContentError::DeadlineExceeded)?
        .map_err(|error| ContentError::Transfer(bounded_diagnostic(error)))
}

async fn find_providers_before_deadline(
    registry: &ModelRegistry,
    cid: &ModelCid,
    deadline: tokio::time::Instant,
) -> Result<Vec<ContentProviderCandidate>, ContentError> {
    tokio::time::timeout_at(deadline, registry.find_content_providers(cid))
        .await
        .map_err(|_| ContentError::DeadlineExceeded)?
        .map_err(|error| ContentError::Transfer(bounded_diagnostic(error)))
}

fn select_content_providers(
    candidates: Vec<ContentProviderCandidate>,
    expected: &ExpectedContent,
) -> Vec<PeerId> {
    let mut seen = HashSet::new();
    let mut providers = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.record.model_cid == expected.cid
                && candidate.record.size_bytes == expected.size
                && candidate.record.format == "gguf"
        })
        .map(|candidate| candidate.provider)
        .filter(|peer| seen.insert(*peer))
        .collect::<Vec<_>>();
    providers.sort_by_key(|peer| peer.to_bytes());
    providers
}

fn provider_attempt_plan(providers: &[PeerId], max_attempts: usize) -> Vec<PeerId> {
    if providers.is_empty() {
        return Vec::new();
    }
    (0..max_attempts)
        .map(|attempt| providers[attempt % providers.len()])
        .collect()
}

fn cid_lock_index(cid: &ModelCid) -> usize {
    usize::from(cid.0[0]) % CID_LOCK_STRIPES
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TransferAction {
    Accepted,
    Chunk(Vec<u8>),
    Complete,
    Rejected(String),
}

#[derive(Debug)]
struct TransferState {
    total: u64,
    cursor: u64,
    accepted: bool,
    complete: bool,
}

impl TransferState {
    fn new(total: u64, cursor: u64) -> Self {
        Self {
            total,
            cursor,
            accepted: false,
            complete: false,
        }
    }

    fn apply(&mut self, frame: BlobStreamFrameKind) -> Result<TransferAction, ContentError> {
        if self.complete {
            return Err(ContentError::Transfer(
                "provider sent a frame after terminal completion".to_string(),
            ));
        }
        match frame {
            BlobStreamFrameKind::Accepted { total_size, offset } if !self.accepted => {
                if total_size != self.total {
                    return Err(ContentError::Transfer(
                        "provider declared a size conflicting with the signed alias".to_string(),
                    ));
                }
                if offset != self.cursor {
                    return Err(ContentError::Transfer(
                        "provider accepted a different resume offset".to_string(),
                    ));
                }
                self.accepted = true;
                Ok(TransferAction::Accepted)
            }
            BlobStreamFrameKind::Rejected { reason } if !self.accepted => {
                self.complete = true;
                Ok(TransferAction::Rejected(bounded_diagnostic(reason)))
            }
            BlobStreamFrameKind::Chunk { offset, bytes } if self.accepted => {
                if offset != self.cursor {
                    return Err(ContentError::Transfer(
                        "provider chunk offset diverged from the staging cursor".to_string(),
                    ));
                }
                if bytes.is_empty() || bytes.len() > BLOB_STREAM_MAX_CHUNK_BYTES {
                    return Err(ContentError::Transfer(
                        "provider chunk exceeded the bounded chunk contract".to_string(),
                    ));
                }
                let end = offset.checked_add(bytes.len() as u64).ok_or_else(|| {
                    ContentError::Transfer("download offset overflow".to_string())
                })?;
                if end > self.total {
                    return Err(ContentError::Transfer(
                        "provider chunk crossed the signed content size".to_string(),
                    ));
                }
                self.cursor = end;
                Ok(TransferAction::Chunk(bytes))
            }
            BlobStreamFrameKind::Eof { offset } if self.accepted => {
                if offset != self.cursor || offset != self.total {
                    return Err(ContentError::Transfer(
                        "provider ended before the signed content size".to_string(),
                    ));
                }
                self.complete = true;
                Ok(TransferAction::Complete)
            }
            _ => Err(ContentError::Transfer(
                "provider violated the blob transfer state machine".to_string(),
            )),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_from_provider(
    network: &Discovery,
    provider: PeerId,
    cid: ModelCid,
    expected_size: u64,
    staging_path: &std::path::Path,
    start: u64,
    pull_deadline: tokio::time::Instant,
    progress: &Option<mpsc::Sender<PullProgress>>,
) -> Result<u64, ContentError> {
    let remaining = pull_deadline.saturating_duration_since(tokio::time::Instant::now());
    let provider_window = remaining.min(PER_PROVIDER_DEADLINE);
    let request = BlobStreamRequest {
        schema_version: BLOB_STREAM_SCHEMA_VERSION,
        content_id: cid.0,
        offset: start,
        deadline_unix_ms: unix_ms().saturating_add(provider_window.as_millis() as u64),
        idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
        metadata: Vec::new(),
    };
    let mut stream = network
        .open_blob_stream(provider, request)
        .await
        .map_err(|error| ContentError::Transfer(bounded_diagnostic(error)))?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(staging_path)
        .await
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
    let actual_start = file
        .metadata()
        .await
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?
        .len();
    if actual_start != start {
        let _ = stream.cancel().await;
        return Err(ContentError::Verification(
            "staging length changed while the same-CID lock was held".to_string(),
        ));
    }

    let mut state = TransferState::new(expected_size, start);
    let progress_step = expected_size
        .div_ceil(MAX_PROGRESS_UPDATES_PER_PROVIDER)
        .max(1);
    let mut next_progress = start.saturating_add(progress_step);
    let provider_label = short_peer(&provider);
    let cid_hex = cid.to_hex();

    loop {
        let frame = match stream.next_frame().await {
            Ok(frame) => frame,
            Err(error) => {
                file.sync_data().await.map_err(|sync_error| {
                    ContentError::Verification(bounded_diagnostic(sync_error))
                })?;
                return Err(ContentError::Transfer(bounded_diagnostic(error)));
            }
        };
        let action = match state.apply(frame.kind) {
            Ok(action) => action,
            Err(error) => {
                let _ = file.sync_data().await;
                let _ = stream.cancel().await;
                return Err(error);
            }
        };
        match action {
            TransferAction::Accepted => {
                if let Err(error) = emit_progress(
                    progress,
                    PullProgress::Downloading {
                        cid: cid_hex.clone(),
                        completed: state.cursor,
                        total: expected_size,
                        provider: provider_label.clone(),
                    },
                )
                .await
                {
                    let _ = file.sync_data().await;
                    let _ = stream.cancel().await;
                    return Err(error);
                }
            }
            TransferAction::Rejected(reason) => {
                file.sync_data().await.map_err(|sync_error| {
                    ContentError::Verification(bounded_diagnostic(sync_error))
                })?;
                return Err(ContentError::Transfer(reason));
            }
            TransferAction::Chunk(bytes) => {
                file.write_all(&bytes)
                    .await
                    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
                if state.cursor >= next_progress || state.cursor == expected_size {
                    if let Err(error) = emit_progress(
                        progress,
                        PullProgress::Downloading {
                            cid: cid_hex.clone(),
                            completed: state.cursor,
                            total: expected_size,
                            provider: provider_label.clone(),
                        },
                    )
                    .await
                    {
                        let _ = file.sync_data().await;
                        let _ = stream.cancel().await;
                        return Err(error);
                    }
                    next_progress = state.cursor.saturating_add(progress_step);
                }
            }
            TransferAction::Complete => {
                file.sync_all()
                    .await
                    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
                return Ok(state.cursor);
            }
        }
    }
}

async fn staging_len(path: &std::path::Path, expected_size: u64) -> Result<u64, ContentError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?;
    if !metadata.file_type().is_file() || metadata.len() > expected_size {
        return Err(ContentError::Verification(
            "staging file is not a bounded regular partial".to_string(),
        ));
    }
    Ok(metadata.len())
}

async fn installed_content_size(registry: &ModelRegistry, cid: ModelCid) -> Option<u64> {
    let mut matches = registry
        .local_installed_async()
        .await
        .into_iter()
        .filter(|installed| installed.model_cid == cid);
    let first = matches.next()?;
    if first.format != "gguf"
        || first.size_bytes == 0
        || matches.any(|installed| {
            installed.format != first.format || installed.size_bytes != first.size_bytes
        })
    {
        return None;
    }
    Some(first.size_bytes)
}

async fn serve_blob(
    store: Arc<ArtifactStore>,
    expected_size: u64,
    request: BlobStreamRequest,
    frames: mpsc::Sender<BlobStreamFrame>,
) {
    // LUCID never interprets peer-supplied metadata as a URL or path. Reject
    // it outright so future callers cannot accidentally introduce an SSRF or
    // filesystem-selection side channel through phase-net's opaque field.
    if !request.metadata.is_empty() {
        send_rejection(
            &frames,
            request.content_id,
            "request metadata is not supported",
        )
        .await;
        return;
    }
    let cid = ModelCid(request.content_id);
    let Some(blob_id) = BlobId::from_hex(&cid.to_hex()) else {
        send_rejection(&frames, request.content_id, "invalid content ID").await;
        return;
    };
    let meta = match tokio::task::spawn_blocking(move || store.get_blob(&blob_id)).await {
        Ok(Ok(Some(meta))) => meta,
        Ok(Ok(None)) => {
            send_rejection(&frames, request.content_id, "content unavailable").await;
            return;
        }
        _ => {
            send_rejection(&frames, request.content_id, "content lookup failed").await;
            return;
        }
    };
    if meta.size_bytes != expected_size {
        send_rejection(
            &frames,
            request.content_id,
            "stored content differs from installed metadata",
        )
        .await;
        return;
    }
    if request.offset > meta.size_bytes {
        send_rejection(
            &frames,
            request.content_id,
            "resume offset exceeds content size",
        )
        .await;
        return;
    }
    let mut file = match tokio::fs::File::open(&meta.path).await {
        Ok(file) => file,
        Err(_) => {
            send_rejection(&frames, request.content_id, "stored content is unavailable").await;
            return;
        }
    };
    let file_metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() == meta.size_bytes => metadata,
        _ => {
            send_rejection(
                &frames,
                request.content_id,
                "stored content metadata changed",
            )
            .await;
            return;
        }
    };
    if file_metadata.len() < request.offset
        || file
            .seek(std::io::SeekFrom::Start(request.offset))
            .await
            .is_err()
    {
        send_rejection(&frames, request.content_id, "resume offset is unavailable").await;
        return;
    }
    if frames
        .send(blob_frame(
            request.content_id,
            BlobStreamFrameKind::Accepted {
                total_size: meta.size_bytes,
                offset: request.offset,
            },
        ))
        .await
        .is_err()
    {
        return;
    }
    let mut offset = request.offset;
    let mut buffer = vec![0_u8; BLOB_STREAM_MAX_CHUNK_BYTES];
    while offset < meta.size_bytes {
        let remaining = (meta.size_bytes - offset).min(BLOB_STREAM_MAX_CHUNK_BYTES as u64) as usize;
        let read = match file.read(&mut buffer[..remaining]).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        if frames
            .send(blob_frame(
                request.content_id,
                BlobStreamFrameKind::Chunk {
                    offset,
                    bytes: buffer[..read].to_vec(),
                },
            ))
            .await
            .is_err()
        {
            return;
        }
        offset += read as u64;
    }
    let _ = frames
        .send(blob_frame(
            request.content_id,
            BlobStreamFrameKind::Eof { offset },
        ))
        .await;
}

fn blob_frame(content_id: [u8; 32], kind: BlobStreamFrameKind) -> BlobStreamFrame {
    BlobStreamFrame {
        schema_version: BLOB_STREAM_SCHEMA_VERSION,
        content_id,
        kind,
    }
}

async fn send_rejection(
    frames: &mpsc::Sender<BlobStreamFrame>,
    content_id: [u8; 32],
    reason: &str,
) {
    let _ = frames
        .send(blob_frame(
            content_id,
            BlobStreamFrameKind::Rejected {
                reason: reason.to_string(),
            },
        ))
        .await;
}

async fn emit_progress(
    progress: &Option<mpsc::Sender<PullProgress>>,
    event: PullProgress,
) -> Result<(), ContentError> {
    if let Some(progress) = progress {
        progress
            .send(event)
            .await
            .map_err(|_| ContentError::Cancelled)?;
    }
    Ok(())
}

async fn installed_blob_matches(
    store: Arc<ArtifactStore>,
    cid: ModelCid,
    expected_size: u64,
) -> Result<bool, ContentError> {
    let id = BlobId::from_hex(&cid.to_hex())
        .ok_or_else(|| ContentError::Verification("invalid canonical blob ID".to_string()))?;
    tokio::task::spawn_blocking(move || store.get_blob(&id))
        .await
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?
        .map(|meta| match meta {
            Some(meta) if meta.size_bytes == expected_size => Ok(true),
            Some(_) => Err(ContentError::Verification(
                "installed blob size conflicts with signed alias".to_string(),
            )),
            None => Ok(false),
        })
        .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?
}

async fn prepare_staging(
    store: Arc<ArtifactStore>,
    blob_id: BlobId,
    expected_size: u64,
) -> Result<(PathBuf, u64), ContentError> {
    tokio::task::spawn_blocking(move || {
        let path = store.prepare_staging_path(&blob_id)?;
        let size = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata.len(),
            Ok(_) => anyhow::bail!("blob staging path is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        if size > expected_size {
            let file = std::fs::OpenOptions::new().write(true).open(&path)?;
            file.set_len(0)?;
            file.sync_all()?;
            Ok((path, 0))
        } else {
            Ok((path, size))
        }
    })
    .await
    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?
    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))
}

async fn commit_staging(
    store: Arc<ArtifactStore>,
    staging_path: PathBuf,
    blob_id: BlobId,
    expected_size: u64,
) -> Result<(), ContentError> {
    tokio::task::spawn_blocking(move || {
        store.commit_staged_blob(&staging_path, &blob_id, expected_size)
    })
    .await
    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))?
    .map(|_| ())
    .map_err(|error| ContentError::Verification(bounded_diagnostic(error)))
}

fn short_peer(peer: &PeerId) -> String {
    peer.to_string().chars().take(12).collect()
}

fn bounded_diagnostic(error: impl std::fmt::Display) -> String {
    error
        .to_string()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect()
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use phase_identity::NodeIdentity;

    use crate::registry::{
        alias_dht_key, content_provider_dht_key, AliasRecord, ContentProviderRecord, DhtTransport,
    };

    #[derive(Debug, Default)]
    struct TestDht {
        records: StdMutex<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
        hanging_keys: StdMutex<HashSet<Vec<u8>>>,
    }

    impl TestDht {
        fn hang(&self, key: Vec<u8>) {
            self.hanging_keys.lock().unwrap().insert(key);
        }
    }

    #[async_trait]
    impl DhtTransport for TestDht {
        async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> AnyResult<()> {
            self.records
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push(value);
            Ok(())
        }

        async fn get_record(&self, key: Vec<u8>) -> AnyResult<Vec<Vec<u8>>> {
            let should_hang = { self.hanging_keys.lock().unwrap().contains(&key) };
            if should_hang {
                futures::future::pending::<()>().await;
            }
            Ok(self
                .records
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or_default())
        }
    }

    fn resolved(alias: &str, cid: ModelCid, size: u64, format: &str) -> ResolvedAlias {
        let mut record = AliasRecord::new(alias, cid, format, size, 1).unwrap();
        // Ensure separately constructed candidates compare only on the
        // immutable metadata under test, not on wall-clock jitter.
        record.issued_at = 1;
        record.valid_until = 2;
        ResolvedAlias {
            record,
            publisher: PeerId::random(),
        }
    }

    fn make_test_file_owner_writable(path: &std::path::Path) {
        let metadata = std::fs::metadata(path).unwrap();
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(permissions.mode() | 0o200);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    async fn connect_discoveries(client: &Discovery, server: &Discovery) {
        server
            .listen("/ip4/127.0.0.1/tcp/0")
            .await
            .expect("listen on loopback");
        let address = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(address) = server.listen_addrs().await.unwrap().into_iter().next() {
                    break address;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loopback listener did not become ready");
        let server_peer = server.local_peer_id();
        client
            .dial_peer(&format!("{address}/p2p/{server_peer}"))
            .await
            .expect("dial loopback content provider");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client
                    .reachability_snapshot()
                    .await
                    .unwrap()
                    .connections
                    .iter()
                    .any(|connection| connection.peer_id == *server_peer)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loopback content connection did not become ready");
    }

    #[test]
    fn config_rejects_unbounded_allocations() {
        assert!(!ContentPlaneConfig::default().publish_provider);
        for config in [
            ContentPlaneConfig {
                max_concurrent_pulls: 0,
                ..Default::default()
            },
            ContentPlaneConfig {
                max_provider_attempts: 65,
                ..Default::default()
            },
            ContentPlaneConfig {
                max_concurrent_serves: 0,
                ..Default::default()
            },
            ContentPlaneConfig {
                max_concurrent_serves: 1,
                max_concurrent_serves_per_peer: 2,
                ..Default::default()
            },
            ContentPlaneConfig {
                max_model_bytes: crate::registry::MAX_MODEL_SIZE_BYTES + 1,
                ..Default::default()
            },
            ContentPlaneConfig {
                max_staging_bytes: 0,
                ..Default::default()
            },
            ContentPlaneConfig {
                pull_deadline: Duration::from_secs(60 * 60 + 1),
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn blob_serve_gate_bounds_global_and_peer_admission_without_peer_maps() {
        let gate = BlobServeGate::new(2, 1);
        let first_peer = PeerId::random();
        let first_stripe = gate.peer_index(&first_peer);
        let second_peer = loop {
            let candidate = PeerId::random();
            if gate.peer_index(&candidate) != first_stripe {
                break candidate;
            }
        };

        let first = gate
            .try_acquire(&first_peer)
            .expect("first stream admitted");
        assert!(gate.try_acquire(&first_peer).is_none());
        let second = gate
            .try_acquire(&second_peer)
            .expect("different peer admitted up to global cap");
        assert!(gate.try_acquire(&PeerId::random()).is_none());
        drop(first);
        assert!(gate.try_acquire(&first_peer).is_some());
        drop(second);
    }

    #[test]
    fn staging_quota_inventories_partials_and_reserves_complete_growth() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(temp.path().join("store")).unwrap();
        let first_blob = BlobId::from_content(b"first partial identity");
        let first_cid = ModelCid::from_hex(first_blob.as_str()).unwrap();
        let first_path = store.prepare_staging_path(&first_blob).unwrap();
        std::fs::write(&first_path, b"1234").unwrap();

        let quota = Arc::new(StagingQuota::from_store(&store, 10).unwrap());
        assert_eq!(quota.state.lock().unwrap().total_accounted, 4);
        let mut first = quota.reserve(first_cid, 8).unwrap();
        first.retain_partial();
        assert_eq!(quota.state.lock().unwrap().total_accounted, 8);

        let second_cid = ModelCid([0xA5; 32]);
        assert!(matches!(
            quota.reserve(second_cid, 3),
            Err(ContentError::Verification(message)) if message.contains("staging quota exceeded")
        ));

        std::fs::remove_file(&first_path).unwrap();
        first.complete();
        assert_eq!(quota.state.lock().unwrap().total_accounted, 0);

        // A reservation made before ArtifactStore returns a staging path is
        // rolled back on early failure/cancellation.
        drop(quota.reserve(second_cid, 10).unwrap());
        assert_eq!(quota.state.lock().unwrap().total_accounted, 0);

        // A daemon that starts over quota may consume an existing full
        // partial without permitting any additional staging growth.
        std::fs::write(&first_path, b"1234").unwrap();
        let over_quota = Arc::new(StagingQuota::from_store(&store, 3).unwrap());
        let existing = over_quota.reserve(first_cid, 4).unwrap();
        assert!(over_quota.reserve(second_cid, 1).is_err());
        std::fs::remove_file(first_path).unwrap();
        existing.complete();
        assert_eq!(over_quota.state.lock().unwrap().total_accounted, 0);
    }

    #[tokio::test]
    async fn total_deadline_covers_alias_and_provider_dht_queries() {
        let alias_dht = Arc::new(TestDht::default());
        alias_dht.hang(alias_dht_key("hanging-alias").unwrap());
        let alias_registry = ModelRegistry::new(NodeIdentity::generate(), alias_dht);
        let alias_error = resolve_alias_before_deadline(
            &alias_registry,
            "hanging-alias",
            tokio::time::Instant::now() + Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert!(matches!(alias_error, ContentError::DeadlineExceeded));

        let provider_dht = Arc::new(TestDht::default());
        let cid = ModelCid([0x51; 32]);
        provider_dht.hang(content_provider_dht_key(&cid));
        let provider_registry = ModelRegistry::new(NodeIdentity::generate(), provider_dht);
        let provider_error = find_providers_before_deadline(
            &provider_registry,
            &cid,
            tokio::time::Instant::now() + Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert!(matches!(provider_error, ContentError::DeadlineExceeded));
    }

    #[test]
    fn diagnostics_are_control_safe_and_bounded() {
        let value = bounded_diagnostic(format!("bad\n\u{1b}[31m{}", "x".repeat(1000)));
        assert!(!value.contains('\n'));
        assert!(!value.contains('\u{1b}'));
        assert!(value.chars().count() <= MAX_DIAGNOSTIC_CHARS);
    }

    #[test]
    fn catalog_preflight_rejects_startup_import_conflicts_before_restore() {
        let catalog = vec![VerifiedCatalogEntry {
            alias: "startup-alias".to_string(),
            cid: ModelCid([1; 32]),
            size: 10,
        }];
        let installed = vec![InstalledModel {
            model_id: "startup-alias".to_string(),
            model_cid: ModelCid([2; 32]),
            format: "gguf".to_string(),
            size_bytes: 10,
            installed_at: 1,
        }];
        assert!(preflight_catalog_registration(&catalog, &installed, &[]).is_err());
        assert!(preflight_catalog_registration(&catalog, &[], &[]).is_ok());
    }

    #[test]
    fn signed_alias_metadata_must_agree_exactly() {
        let cid = ModelCid([1; 32]);
        let agreed = vec![
            resolved("model", cid, 42, "gguf"),
            resolved("model", cid, 42, "gguf"),
        ];
        assert_eq!(
            reconcile_alias_records(&agreed).unwrap(),
            ExpectedContent {
                alias: "model".to_string(),
                cid,
                size: 42,
            }
        );

        for conflicting in [
            resolved("model", ModelCid([2; 32]), 42, "gguf"),
            resolved("model", cid, 43, "gguf"),
            resolved("model", cid, 42, "safetensors"),
        ] {
            assert!(matches!(
                reconcile_alias_records(&[agreed[0].clone(), conflicting]),
                Err(ContentError::AliasConflict)
            ));
        }
        assert!(matches!(
            reconcile_alias_records(&[resolved("model", cid, 42, "safetensors")]),
            Err(ContentError::UnsupportedFormat(format)) if format == "safetensors"
        ));
    }

    #[test]
    fn exact_cid_and_publisher_pins_narrow_conflicts_without_choosing_arbitrarily() {
        let first_cid = ModelCid([0x31; 32]);
        let second_cid = ModelCid([0x32; 32]);
        let first = resolved("pinned", first_cid, 42, "gguf");
        let second = resolved("pinned", second_cid, 84, "gguf");

        let by_cid = select_alias_records(
            vec![first.clone(), second.clone()],
            PullSelection {
                exact_cid: Some(second_cid),
                publisher: None,
            },
        )
        .unwrap();
        assert_eq!(by_cid, vec![second.clone()]);
        assert_eq!(reconcile_alias_records(&by_cid).unwrap().cid, second_cid);

        let by_publisher = select_alias_records(
            vec![first.clone(), second.clone()],
            PullSelection {
                exact_cid: None,
                publisher: Some(first.publisher),
            },
        )
        .unwrap();
        assert_eq!(by_publisher, vec![first.clone()]);

        assert!(matches!(
            select_alias_records(
                vec![first, second],
                PullSelection {
                    exact_cid: Some(ModelCid([0xff; 32])),
                    publisher: None,
                },
            ),
            Err(ContentError::PinMismatch)
        ));
    }

    #[test]
    fn only_exact_signed_content_providers_are_selected_and_deduplicated() {
        let first = PeerId::random();
        let second = PeerId::random();
        let cid = ModelCid([3; 32]);
        let expected = ExpectedContent {
            alias: "model".to_string(),
            cid,
            size: 42,
        };
        let candidate = |provider, size, format: &str| ContentProviderCandidate {
            provider,
            record: ContentProviderRecord::new(cid, size, format, provider, 1).unwrap(),
        };
        let selected = select_content_providers(
            vec![
                candidate(first, 42, "gguf"),
                candidate(second, 42, "gguf"),
                candidate(first, 42, "gguf"),
                candidate(PeerId::random(), 43, "gguf"),
                candidate(PeerId::random(), 42, "safetensors"),
            ],
            &expected,
        );
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&first));
        assert!(selected.contains(&second));

        let attempts = provider_attempt_plan(&selected, 5);
        assert_eq!(attempts.len(), 5);
        assert_eq!(attempts[0], attempts[2]);
        assert_eq!(attempts[1], attempts[3]);
        assert_eq!(attempts[0], attempts[4]);
    }

    #[test]
    fn transfer_state_rejects_wrong_total_offset_chunks_and_eof() {
        let mut wrong_total = TransferState::new(10, 3);
        assert!(wrong_total
            .apply(BlobStreamFrameKind::Accepted {
                total_size: 11,
                offset: 3,
            })
            .is_err());

        let mut wrong_resume = TransferState::new(10, 3);
        assert!(wrong_resume
            .apply(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 2,
            })
            .is_err());

        let mut state = TransferState::new(10, 3);
        assert_eq!(
            state
                .apply(BlobStreamFrameKind::Accepted {
                    total_size: 10,
                    offset: 3,
                })
                .unwrap(),
            TransferAction::Accepted
        );
        for invalid in [
            BlobStreamFrameKind::Chunk {
                offset: 2,
                bytes: vec![1],
            },
            BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: Vec::new(),
            },
            BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: vec![1; BLOB_STREAM_MAX_CHUNK_BYTES + 1],
            },
            BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: vec![1; 8],
            },
            BlobStreamFrameKind::Eof { offset: 3 },
        ] {
            let mut candidate = TransferState::new(10, 3);
            candidate
                .apply(BlobStreamFrameKind::Accepted {
                    total_size: 10,
                    offset: 3,
                })
                .unwrap();
            assert!(candidate.apply(invalid).is_err());
        }
    }

    #[test]
    fn transfer_state_accepts_exact_resumed_sequence() {
        let mut state = TransferState::new(10, 3);
        state
            .apply(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 3,
            })
            .unwrap();
        assert_eq!(
            state
                .apply(BlobStreamFrameKind::Chunk {
                    offset: 3,
                    bytes: vec![4, 5, 6],
                })
                .unwrap(),
            TransferAction::Chunk(vec![4, 5, 6])
        );
        state
            .apply(BlobStreamFrameKind::Chunk {
                offset: 6,
                bytes: vec![7, 8, 9, 10],
            })
            .unwrap();
        assert_eq!(
            state
                .apply(BlobStreamFrameKind::Eof { offset: 10 })
                .unwrap(),
            TransferAction::Complete
        );
    }

    #[tokio::test]
    async fn inbound_handler_serves_exact_ordered_resume_and_eof() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let content = vec![7_u8; BLOB_STREAM_MAX_CHUNK_BYTES + 17];
        let blob_id = store.add_blob(&content).unwrap();
        let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
        let request = BlobStreamRequest {
            schema_version: BLOB_STREAM_SCHEMA_VERSION,
            content_id: cid.0,
            offset: 13,
            deadline_unix_ms: unix_ms() + 10_000,
            idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
            metadata: Vec::new(),
        };
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(serve_blob(store, content.len() as u64, request, tx));
        let mut frames = Vec::new();
        while let Some(frame) = rx.recv().await {
            frames.push(frame);
        }
        task.await.unwrap();

        assert!(matches!(
            frames.first().map(|frame| &frame.kind),
            Some(BlobStreamFrameKind::Accepted { total_size, offset })
                if *total_size == content.len() as u64 && *offset == 13
        ));
        let mut cursor = 13_u64;
        let mut received = Vec::new();
        for frame in &frames[1..frames.len() - 1] {
            let BlobStreamFrameKind::Chunk { offset, bytes } = &frame.kind else {
                panic!("expected only chunks between Accepted and EOF");
            };
            assert_eq!(*offset, cursor);
            assert!(!bytes.is_empty());
            assert!(bytes.len() <= BLOB_STREAM_MAX_CHUNK_BYTES);
            cursor += bytes.len() as u64;
            received.extend_from_slice(bytes);
        }
        assert_eq!(received, content[13..]);
        assert!(matches!(
            frames.last().map(|frame| &frame.kind),
            Some(BlobStreamFrameKind::Eof { offset }) if *offset == content.len() as u64
        ));
    }

    #[tokio::test]
    async fn inbound_handler_never_serves_uninstalled_artifact_store_blobs() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let blob_id = store.add_blob(b"unrelated artifact").unwrap();
        let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
        let identity = NodeIdentity::generate();
        let registry = Arc::new(ModelRegistry::new(
            identity.clone(),
            Arc::new(TestDht::default()),
        ));
        let network = Arc::new(
            Discovery::new(phase_net::DiscoveryConfig {
                identity: Some(identity),
                ..Default::default()
            })
            .unwrap(),
        );
        let plane = ContentPlane::new(
            network,
            registry,
            store,
            temp.path().join("verified"),
            ContentPlaneConfig::default(),
        )
        .unwrap();
        let handler = plane.blob_stream_handler();
        let (tx, mut rx) = mpsc::channel(1);
        handler(
            PeerId::random(),
            BlobStreamRequest {
                schema_version: BLOB_STREAM_SCHEMA_VERSION,
                content_id: cid.0,
                offset: 0,
                deadline_unix_ms: unix_ms() + 10_000,
                idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
                metadata: Vec::new(),
            },
            tx,
        )
        .await;
        assert!(matches!(
            rx.recv().await.map(|frame| frame.kind),
            Some(BlobStreamFrameKind::Rejected { reason }) if reason == "content unavailable"
        ));
    }

    #[tokio::test]
    async fn inbound_rejects_metadata_bad_offset_and_receiver_loss() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let blob_id = store.add_blob(b"content").unwrap();
        let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();

        for (offset, metadata) in [(0, vec![1]), (8, Vec::new())] {
            let (tx, mut rx) = mpsc::channel(1);
            serve_blob(
                store.clone(),
                7,
                BlobStreamRequest {
                    schema_version: BLOB_STREAM_SCHEMA_VERSION,
                    content_id: cid.0,
                    offset,
                    deadline_unix_ms: unix_ms() + 10_000,
                    idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
                    metadata,
                },
                tx,
            )
            .await;
            assert!(matches!(
                rx.recv().await.map(|frame| frame.kind),
                Some(BlobStreamFrameKind::Rejected { .. })
            ));
        }

        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        tokio::time::timeout(
            Duration::from_secs(1),
            serve_blob(
                store,
                7,
                BlobStreamRequest {
                    schema_version: BLOB_STREAM_SCHEMA_VERSION,
                    content_id: cid.0,
                    offset: 0,
                    deadline_unix_ms: unix_ms() + 10_000,
                    idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
                    metadata: Vec::new(),
                },
                tx,
            ),
        )
        .await
        .expect("receiver loss must stop the serving task");
    }

    #[tokio::test]
    async fn stable_partial_resumes_and_corrupt_full_partial_allows_clean_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let content = b"verified provider fallback";
        let blob_id = BlobId::from_content(content);
        let (path, initial) = prepare_staging(store.clone(), blob_id.clone(), content.len() as u64)
            .await
            .unwrap();
        assert_eq!(initial, 0);
        tokio::fs::write(&path, &content[..8]).await.unwrap();
        let (same_path, resumed) =
            prepare_staging(store.clone(), blob_id.clone(), content.len() as u64)
                .await
                .unwrap();
        assert_eq!(same_path, path);
        assert_eq!(resumed, 8);
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        file.write_all(&content[8..]).await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);
        commit_staging(
            store.clone(),
            path.clone(),
            blob_id.clone(),
            content.len() as u64,
        )
        .await
        .unwrap();
        assert!(store.get_blob(&blob_id).unwrap().is_some());

        let other = b"correct after corrupt provider";
        let other_id = BlobId::from_content(other);
        let (other_path, _) = prepare_staging(store.clone(), other_id.clone(), other.len() as u64)
            .await
            .unwrap();
        tokio::fs::write(&other_path, vec![0_u8; other.len()])
            .await
            .unwrap();
        assert!(commit_staging(
            store.clone(),
            other_path.clone(),
            other_id.clone(),
            other.len() as u64,
        )
        .await
        .is_err());
        let (_, reset) = prepare_staging(store.clone(), other_id.clone(), other.len() as u64)
            .await
            .unwrap();
        assert_eq!(reset, 0, "bad full content must not poison fallback");
        tokio::fs::write(&other_path, other).await.unwrap();
        commit_staging(store, other_path, other_id, other.len() as u64)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn progress_disconnect_cancels_and_same_cid_lock_serializes() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(matches!(
            emit_progress(
                &Some(tx),
                PullProgress::Resolving {
                    alias: "model".to_string()
                }
            )
            .await,
            Err(ContentError::Cancelled)
        ));

        let locks = Arc::new(
            (0..CID_LOCK_STRIPES)
                .map(|_| Mutex::new(()))
                .collect::<Vec<_>>(),
        );
        let cid = ModelCid([11; 32]);
        let index = cid_lock_index(&cid);
        let first = locks[index].lock().await;
        let second_locks = locks.clone();
        let waiter = tokio::spawn(async move {
            let _guard = second_locks[index].lock().await;
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiter.is_finished());
        drop(first);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("same-CID waiter should proceed after release")
            .unwrap();
    }

    #[tokio::test]
    async fn installed_catalog_persists_union_and_restores_verified_content_silently() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let verified_dir = temp.path().join("verified");
        let dht = Arc::new(TestDht::default());
        let registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht.clone()));
        let plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            registry.clone(),
            store.clone(),
            verified_dir.clone(),
            ContentPlaneConfig::default(),
        )
        .unwrap();

        let first_bytes = b"first durable consume-only model";
        let first_blob = store.add_blob(first_bytes).unwrap();
        let first_cid = ModelCid::from_hex(first_blob.as_str()).unwrap();
        plane
            .register_installed("durable-first", first_cid, first_bytes.len() as u64, &None)
            .await
            .unwrap();

        let imported_bytes = b"startup imported durable model";
        let imported_blob = store.add_blob(imported_bytes).unwrap();
        let imported_cid = ModelCid::from_hex(imported_blob.as_str()).unwrap();
        registry
            .register_verified_gguf_blob(
                store.clone(),
                verified_dir.clone(),
                "durable-import",
                imported_cid,
                imported_bytes.len() as u64,
            )
            .await
            .unwrap();
        plane.persist_installed_catalog().await.unwrap();

        let catalog_path = verified_dir.join(CONTENT_CATALOG_FILE);
        assert!(std::fs::metadata(&catalog_path).unwrap().len() <= MAX_CONTENT_CATALOG_BYTES);
        assert!(!verified_dir.join(CONTENT_CATALOG_TEMP_FILE).exists());

        let restored_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
        let restored_plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            restored_registry.clone(),
            store,
            verified_dir.clone(),
            ContentPlaneConfig::default(),
        )
        .unwrap();
        let restored = restored_plane.restore_installed_catalog().await.unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored_registry.local_installed_async().await.len(), 2);
        assert!(restored_registry.local_models_async().await.is_empty());
        assert!(restored_registry
            .find_content_providers(&first_cid)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(verified_dir.join(format!("{}.gguf", first_cid.to_hex()))).unwrap(),
            first_bytes
        );
        assert_eq!(
            std::fs::read(verified_dir.join(format!("{}.gguf", imported_cid.to_hex()))).unwrap(),
            imported_bytes
        );
    }

    #[tokio::test]
    async fn verified_gguf_activates_only_when_a_local_llama_worker_is_configured() {
        let temp = tempfile::tempdir().unwrap();
        let dht = Arc::new(TestDht::default());
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let verified_dir = temp.path().join("verified");
        let bytes = b"verified local activation model";
        let blob = store.add_blob(bytes).unwrap();
        let cid = ModelCid::from_hex(blob.as_str()).unwrap();

        let cache_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht.clone()));
        let cache_plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            cache_registry.clone(),
            store.clone(),
            verified_dir.clone(),
            ContentPlaneConfig::default(),
        )
        .unwrap();
        cache_plane
            .register_installed("activation-test", cid, bytes.len() as u64, &None)
            .await
            .unwrap();
        assert!(cache_registry.local_models_async().await.is_empty());

        let worker_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
        let worker_plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            worker_registry.clone(),
            store,
            verified_dir,
            ContentPlaneConfig {
                local_gguf_activation: Some(LocalGgufActivation {
                    context_length: 16_384,
                    max_concurrent: 1,
                    backend: "llama.cpp".to_string(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let restored = worker_plane.restore_installed_catalog().await.unwrap();
        assert_eq!(restored.len(), 1);
        let loaded = worker_registry.local_models_async().await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].model_id, "activation-test");
        assert_eq!(loaded[0].model_cid, cid);
        assert_eq!(loaded[0].backend, "llama.cpp");
        assert_eq!(loaded[0].context_length, 16_384);
    }

    #[tokio::test]
    async fn catalog_failure_rolls_back_installed_and_loaded_registry_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let verified_dir = temp.path().join("verified");
        std::fs::create_dir_all(verified_dir.join(CONTENT_CATALOG_FILE)).unwrap();
        let bytes = b"rollback on catalog failure";
        let blob = store.add_blob(bytes).unwrap();
        let cid = ModelCid::from_hex(blob.as_str()).unwrap();
        let registry = Arc::new(ModelRegistry::new(
            NodeIdentity::generate(),
            Arc::new(TestDht::default()),
        ));
        let plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            registry.clone(),
            store,
            verified_dir,
            ContentPlaneConfig {
                local_gguf_activation: Some(LocalGgufActivation {
                    context_length: 8_192,
                    max_concurrent: 1,
                    backend: "llama.cpp".to_string(),
                }),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(plane
            .register_installed("rollback-test", cid, bytes.len() as u64, &None)
            .await
            .is_err());
        assert!(registry.local_installed_async().await.is_empty());
        assert!(registry.local_models_async().await.is_empty());
    }

    #[tokio::test]
    async fn corrupt_or_unverifiable_catalog_fails_before_exposing_any_entry() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let verified_dir = temp.path().join("verified");
        std::fs::create_dir_all(&verified_dir).unwrap();
        let good_bytes = b"valid but must not be partially restored";
        let good_blob = store.add_blob(good_bytes).unwrap();
        let good_cid = ModelCid::from_hex(good_blob.as_str()).unwrap();
        let absent_cid = ModelCid([0x93; 32]);
        let untrusted = ContentCatalog {
            schema_version: CONTENT_CATALOG_SCHEMA_VERSION,
            entries: vec![
                ContentCatalogEntry {
                    alias: "good-entry".to_string(),
                    cid: good_cid.to_hex(),
                    size: good_bytes.len() as u64,
                    format: "gguf".to_string(),
                },
                ContentCatalogEntry {
                    alias: "absent-entry".to_string(),
                    cid: absent_cid.to_hex(),
                    size: 1,
                    format: "gguf".to_string(),
                },
            ],
        };
        std::fs::write(
            verified_dir.join(CONTENT_CATALOG_FILE),
            serde_json::to_vec(&untrusted).unwrap(),
        )
        .unwrap();

        let registry = Arc::new(ModelRegistry::new(
            NodeIdentity::generate(),
            Arc::new(TestDht::default()),
        ));
        let plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            registry.clone(),
            store,
            verified_dir.clone(),
            ContentPlaneConfig::default(),
        )
        .unwrap();
        assert!(plane.restore_installed_catalog().await.is_err());
        assert!(registry.local_installed_async().await.is_empty());
        assert!(!verified_dir
            .join(format!("{}.gguf", good_cid.to_hex()))
            .exists());

        std::fs::write(
            verified_dir.join(CONTENT_CATALOG_FILE),
            b"{\"schema_version\":1,",
        )
        .unwrap();
        assert!(plane.restore_installed_catalog().await.is_err());
        assert!(registry.local_installed_async().await.is_empty());
    }

    #[tokio::test]
    async fn catalog_restore_rehashes_and_rejects_same_size_blob_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let verified_dir = temp.path().join("verified");
        let bytes = b"catalog-bound content";
        let blob_id = store.add_blob(bytes).unwrap();
        let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
        write_catalog(
            &verified_dir,
            &[InstalledModel {
                model_id: "tamper-test".to_string(),
                model_cid: cid,
                format: "gguf".to_string(),
                size_bytes: bytes.len() as u64,
                installed_at: 1,
            }],
        )
        .unwrap();
        let stored_path = store.get_blob_path(&blob_id).unwrap();
        make_test_file_owner_writable(&stored_path);
        std::fs::write(stored_path, vec![0xA5; bytes.len()]).unwrap();

        let registry = Arc::new(ModelRegistry::new(
            NodeIdentity::generate(),
            Arc::new(TestDht::default()),
        ));
        let plane = ContentPlane::new(
            Arc::new(Discovery::new(Default::default()).unwrap()),
            registry.clone(),
            store,
            verified_dir.clone(),
            ContentPlaneConfig::default(),
        )
        .unwrap();
        assert!(plane.restore_installed_catalog().await.is_err());
        assert!(registry.local_installed_async().await.is_empty());
        assert!(!verified_dir.join(format!("{}.gguf", cid.to_hex())).exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_discovery_nodes_resolve_signed_content_resume_and_reject_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let dht = Arc::new(TestDht::default());
        let provider_identity = NodeIdentity::generate();
        let consumer_identity = NodeIdentity::generate();
        let provider_network = Arc::new(
            Discovery::new(phase_net::DiscoveryConfig {
                identity: Some(provider_identity.clone()),
                ..Default::default()
            })
            .unwrap(),
        );
        let consumer_network = Arc::new(
            Discovery::new(phase_net::DiscoveryConfig {
                identity: Some(consumer_identity.clone()),
                ..Default::default()
            })
            .unwrap(),
        );
        connect_discoveries(&consumer_network, &provider_network).await;

        let provider_store = Arc::new(
            ArtifactStore::new(temp.path().join("provider-store")).expect("provider store"),
        );
        let provider_verified = temp.path().join("provider-verified");
        let provider_registry = Arc::new(ModelRegistry::new(provider_identity, dht.clone()));
        let provider_plane = ContentPlane::new(
            provider_network.clone(),
            provider_registry,
            provider_store.clone(),
            provider_verified,
            ContentPlaneConfig {
                publish_provider: true,
                pull_deadline: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        provider_network
            .set_blob_stream_handler(Some(provider_plane.blob_stream_handler()))
            .unwrap();

        let content = (0..(BLOB_STREAM_MAX_CHUNK_BYTES + 137))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let provider_blob = provider_store.add_blob(&content).unwrap();
        let cid = ModelCid::from_hex(provider_blob.as_str()).unwrap();
        provider_plane
            .register_installed("remote-exact", cid, content.len() as u64, &None)
            .await
            .unwrap();

        let consumer_store =
            Arc::new(ArtifactStore::new(temp.path().join("consumer-store")).unwrap());
        let consumer_verified = temp.path().join("consumer-verified");
        let consumer_registry = Arc::new(ModelRegistry::new(consumer_identity, dht.clone()));
        let consumer_plane = ContentPlane::new(
            consumer_network.clone(),
            consumer_registry.clone(),
            consumer_store.clone(),
            consumer_verified.clone(),
            ContentPlaneConfig {
                max_provider_attempts: 1,
                pull_deadline: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        let resume_offset = 777usize;
        let staging = consumer_store.prepare_staging_path(&provider_blob).unwrap();
        std::fs::write(&staging, &content[..resume_offset]).unwrap();
        let (progress_tx, mut progress_rx) = mpsc::channel(32);
        let installed = consumer_plane
            .pull("remote-exact", Some(progress_tx))
            .await
            .unwrap();
        assert_eq!(installed.model_cid, cid);
        let progress = std::iter::from_fn(|| progress_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(progress.iter().any(|event| matches!(
            event,
            PullProgress::Downloading { completed, total, .. }
                if *completed == resume_offset as u64 && *total == content.len() as u64
        )));
        let installed_blob = consumer_store.get_blob(&provider_blob).unwrap().unwrap();
        assert_eq!(std::fs::read(installed_blob.path).unwrap(), content);
        assert_eq!(
            std::fs::read(consumer_verified.join(format!("{}.gguf", cid.to_hex()))).unwrap(),
            content
        );
        assert_eq!(consumer_registry.local_installed_async().await.len(), 1);
        assert!(consumer_registry.local_models_async().await.is_empty());

        // A serving store that changes after its signed advertisement can
        // still send bytes, but the consumer's exact CID commit must reject
        // them before installation or catalog publication.
        let provider_path = provider_store.get_blob_path(&provider_blob).unwrap();
        make_test_file_owner_writable(&provider_path);
        std::fs::write(provider_path, vec![0xFF; content.len()]).unwrap();
        let tampered_store =
            Arc::new(ArtifactStore::new(temp.path().join("tampered-store")).unwrap());
        let tampered_verified = temp.path().join("tampered-verified");
        let tampered_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
        let tampered_plane = ContentPlane::new(
            consumer_network,
            tampered_registry.clone(),
            tampered_store.clone(),
            tampered_verified.clone(),
            ContentPlaneConfig {
                max_provider_attempts: 1,
                pull_deadline: Duration::from_secs(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            tampered_plane.pull("remote-exact", None).await,
            Err(ContentError::ProvidersExhausted { attempts: 1, .. })
        ));
        assert!(tampered_store.get_blob(&provider_blob).unwrap().is_none());
        assert!(tampered_registry.local_installed_async().await.is_empty());
        assert!(!tampered_verified
            .join(format!("{}.gguf", cid.to_hex()))
            .exists());
        assert!(!tampered_verified.join(CONTENT_CATALOG_FILE).exists());
    }

    #[tokio::test]
    async fn committed_blob_registration_is_exact_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let content = b"registered exact model";
        let blob_id = store.add_blob(content).unwrap();
        let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
        let registry = ModelRegistry::new(NodeIdentity::generate(), Arc::new(TestDht::default()));
        let verified_dir = temp.path().join("verified");

        let first = registry
            .register_verified_gguf_blob(
                store.clone(),
                verified_dir.clone(),
                "exact-model",
                cid,
                content.len() as u64,
            )
            .await
            .unwrap();
        let second = registry
            .register_verified_gguf_blob(
                store,
                verified_dir.clone(),
                "exact-model",
                cid,
                content.len() as u64,
            )
            .await
            .unwrap();
        assert_eq!(first.model_cid, cid);
        assert_eq!(second.model_cid, cid);
        assert_eq!(registry.local_installed_async().await.len(), 1);
        assert!(registry.local_models_async().await.is_empty());
        assert!(registry
            .find_content_providers(&cid)
            .await
            .unwrap()
            .is_empty());
        registry
            .publish_installed_content_provider(&cid)
            .await
            .unwrap();
        let providers = registry.find_content_providers(&cid).await.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].record.model_cid, cid);
        assert_eq!(providers[0].record.size_bytes, content.len() as u64);
        assert_eq!(
            tokio::fs::read(verified_dir.join(format!("{}.gguf", cid.to_hex())))
                .await
                .unwrap(),
            content
        );
    }
}
