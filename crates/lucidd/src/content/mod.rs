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
        let existing = self.registry.local_installed_async().await;
        let loaded = self.registry.local_models_async().await;
        let already_verified: HashMap<ModelCid, u64> = existing
            .iter()
            .map(|record| (record.model_cid, record.size_bytes))
            .collect();
        let verified_dir = self.verified_model_dir.clone();
        let store = self.store.clone();
        let max_model_bytes = self.config.max_model_bytes;
        let entries = tokio::task::spawn_blocking(move || {
            load_and_verify_catalog(&verified_dir, &store, max_model_bytes, &already_verified)
        })
        .await
        .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?
        .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;

        preflight_catalog_registration(&entries, &existing, &loaded)
            .map_err(|error| ContentError::Registration(bounded_diagnostic(error)))?;

        let existing_by_alias: HashMap<String, InstalledModel> = existing
            .into_iter()
            .map(|record| (record.model_id.clone(), record))
            .collect();
        let mut restored = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(already) = existing_by_alias.get(&entry.alias) {
                if already.model_cid == entry.cid && already.size_bytes == entry.size {
                    restored.push(already.clone());
                    continue;
                }
            }
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
    already_verified: &HashMap<ModelCid, u64>,
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
        // Skip redundant multi-GB re-hashing when this exact daemon startup
        // already independently hashed and verified the same CID/size during
        // local model import.
        if already_verified.get(&cid) != Some(&entry.size) {
            let (actual_id, actual_size) = ArtifactStore::compute_blob_id(&blob.path)?;
            if actual_id != blob_id || actual_size != entry.size {
                bail!("catalog blob failed independent CID/size verification");
            }
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
mod tests;
