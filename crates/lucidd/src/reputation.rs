// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local, privacy-minimal execution evidence and derived peer assessment.
//!
//! Raw evidence is deliberately separate from model advertisements and policy.
//! It records attributable protocol outcomes, not prompts, output tokens,
//! embeddings, model bytes, or claims that a computation was correct. Derived
//! assessments are deterministic, local judgments only; this module does not
//! alter routing or authorization.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phase_net::PeerId;
use phase_protocol::JobSpecKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::registry::ModelCid;

const FILE_MAGIC: &[u8; 8] = b"LUCEV001";
const CHECKSUM_DOMAIN: &[u8] = b"lucid-evidence-record:v1\0";
const EVENT_ID_DOMAIN: &[u8] = b"lucid-evidence-event-id:v1\0";
pub const EVIDENCE_SCHEMA_VERSION: u16 = 1;
const LENGTH_PREFIX_BYTES: usize = 4;
const CHECKSUM_BYTES: usize = 32;
const MAX_PROTOCOL_VERSION_BYTES: usize = 128;
const MAX_SOFTWARE_VERSION_BYTES: usize = 128;
const MAX_FUTURE_SKEW_MS: u64 = 5 * 60 * 1_000;
const DECAY_HALF_LIFE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const DECAY_UNIT: u64 = 1_024;
const FULL_CONFIDENCE_WEIGHT: u64 = 8 * DECAY_UNIT;
const OBSERVED_CONFIDENCE_PERMILLE: u16 = 500;

/// Maximum encoded postcard payload for one evidence record.
pub const MAX_EVIDENCE_RECORD_BYTES: usize = 4 * 1_024;
/// Maximum evidence file size accepted or produced by this store.
pub const MAX_EVIDENCE_FILE_BYTES: u64 = 64 * 1_024 * 1_024;
/// Maximum number of physical records accepted from one file.
pub const MAX_EVIDENCE_RECORDS: usize = 100_000;
/// Maximum peers assessed in one routing snapshot.
pub const MAX_RUNTIME_ASSESSMENT_PEERS: usize = 256;
/// Maximum explicit operator overrides retained by one runtime.
pub const MAX_OPERATOR_OVERRIDES: usize = 4_096;
/// Default local retention window. Evidence older than this has passed four
/// score half-lives and is periodically removed from the bounded store.
pub const DEFAULT_EVIDENCE_RETENTION: Duration = Duration::from_secs(28 * 24 * 60 * 60);

/// The release-defined execution evidence taxonomy. Similar-looking failures
/// remain separate because they carry different evidentiary value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    VerifiedSuccessfulCompletion,
    VerifiedCancellation,
    VerifiedWorkerError,
    PolicyRefusal,
    CapacityRefusal,
    PreOutputTransportFailure,
    PreOutputDiscoveryFailure,
    MidStreamTransportLoss,
    MissingTerminalEvent,
    MissingReceipt,
    InvalidReceiptSignature,
    ManifestMismatch,
    JobMismatch,
    SignerPeerIdMismatch,
    OutputCommitmentMismatch,
    ChunkCountMismatch,
    SequenceMismatch,
    DeadlineTimeout,
    IdleTimeout,
    RedundantExecutionAgreement,
    RedundantExecutionDisagreement,
    RedundantExecutionIncomparableResult,
    OperatorReviewedAbuseEvidence,
}

/// Privacy-minimal construction input. No raw request or result content is
/// accepted by the schema.
#[derive(Debug, Clone)]
pub struct EvidenceContext {
    pub observer_peer_id: PeerId,
    pub remote_peer_id: PeerId,
    pub job_spec_hash: [u8; 32],
    pub job_class: JobSpecKind,
    pub model_cid: ModelCid,
    pub protocol_version: String,
    pub software_version: String,
    pub observed_at_unix_ms: u64,
}

/// One locally observed execution outcome. The event ID is a stable SHA-256
/// over every other field, so retrying identical ingestion is idempotent while
/// any mutation produces a different ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEvidence {
    schema_version: u16,
    event_id: [u8; 32],
    observer_peer_id: String,
    remote_peer_id: String,
    job_spec_hash: [u8; 32],
    job_class: JobSpecKind,
    model_cid: ModelCid,
    protocol_version: String,
    software_version: String,
    observed_at_unix_ms: u64,
    outcome: EvidenceOutcome,
    output_commitment: Option<[u8; 32]>,
}

#[derive(Serialize)]
struct EventIdMaterial<'a> {
    schema_version: u16,
    observer_peer_id: &'a str,
    remote_peer_id: &'a str,
    job_spec_hash: &'a [u8; 32],
    job_class: JobSpecKind,
    model_cid: ModelCid,
    protocol_version: &'a str,
    software_version: &'a str,
    observed_at_unix_ms: u64,
    outcome: EvidenceOutcome,
    output_commitment: &'a Option<[u8; 32]>,
}

impl ExecutionEvidence {
    pub fn new(
        context: EvidenceContext,
        outcome: EvidenceOutcome,
        output_commitment: Option<[u8; 32]>,
    ) -> Result<Self, EvidenceError> {
        let mut record = Self {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            event_id: [0; 32],
            observer_peer_id: context.observer_peer_id.to_string(),
            remote_peer_id: context.remote_peer_id.to_string(),
            job_spec_hash: context.job_spec_hash,
            job_class: context.job_class,
            model_cid: context.model_cid,
            protocol_version: context.protocol_version,
            software_version: context.software_version,
            observed_at_unix_ms: context.observed_at_unix_ms,
            outcome,
            output_commitment,
        };
        record.event_id = record.compute_event_id()?;
        record.validate_static()?;
        Ok(record)
    }

    pub fn event_id(&self) -> [u8; 32] {
        self.event_id
    }

    pub fn observer_peer_id(&self) -> &str {
        &self.observer_peer_id
    }

    pub fn remote_peer_id(&self) -> &str {
        &self.remote_peer_id
    }

    pub fn job_spec_hash(&self) -> [u8; 32] {
        self.job_spec_hash
    }

    pub fn job_class(&self) -> JobSpecKind {
        self.job_class
    }

    pub fn model_cid(&self) -> ModelCid {
        self.model_cid
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn software_version(&self) -> &str {
        &self.software_version
    }

    pub fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }

    pub fn outcome(&self) -> EvidenceOutcome {
        self.outcome
    }

    pub fn output_commitment(&self) -> Option<[u8; 32]> {
        self.output_commitment
    }

    fn event_id_material(&self) -> EventIdMaterial<'_> {
        EventIdMaterial {
            schema_version: self.schema_version,
            observer_peer_id: &self.observer_peer_id,
            remote_peer_id: &self.remote_peer_id,
            job_spec_hash: &self.job_spec_hash,
            job_class: self.job_class,
            model_cid: self.model_cid,
            protocol_version: &self.protocol_version,
            software_version: &self.software_version,
            observed_at_unix_ms: self.observed_at_unix_ms,
            outcome: self.outcome,
            output_commitment: &self.output_commitment,
        }
    }

    fn compute_event_id(&self) -> Result<[u8; 32], EvidenceError> {
        let encoded = postcard::to_stdvec(&self.event_id_material())
            .map_err(|error| EvidenceError::Encode(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(EVENT_ID_DOMAIN);
        hasher.update(encoded);
        Ok(hasher.finalize().into())
    }

    fn validate_static(&self) -> Result<(), EvidenceError> {
        if self.schema_version != EVIDENCE_SCHEMA_VERSION {
            return Err(EvidenceError::UnsupportedSchema(self.schema_version));
        }
        let observer = parse_peer_id("observer_peer_id", &self.observer_peer_id)?;
        let remote = parse_peer_id("remote_peer_id", &self.remote_peer_id)?;
        if observer == remote {
            return Err(EvidenceError::InvalidField(
                "remote_peer_id must differ from the local observer",
            ));
        }
        if self.job_spec_hash == [0; 32] {
            return Err(EvidenceError::InvalidField(
                "job_spec_hash must be non-zero",
            ));
        }
        if self.model_cid.0 == [0; 32] {
            return Err(EvidenceError::InvalidField("model_cid must be non-zero"));
        }
        if self.output_commitment == Some([0; 32]) {
            return Err(EvidenceError::InvalidField(
                "output_commitment must be non-zero when present",
            ));
        }
        if matches!(
            self.outcome,
            EvidenceOutcome::PolicyRefusal
                | EvidenceOutcome::CapacityRefusal
                | EvidenceOutcome::PreOutputTransportFailure
                | EvidenceOutcome::PreOutputDiscoveryFailure
        ) && self.output_commitment.is_some()
        {
            return Err(EvidenceError::InvalidField(
                "pre-output evidence cannot carry an output commitment",
            ));
        }
        validate_version(
            "protocol_version",
            &self.protocol_version,
            MAX_PROTOCOL_VERSION_BYTES,
        )?;
        validate_version(
            "software_version",
            &self.software_version,
            MAX_SOFTWARE_VERSION_BYTES,
        )?;
        if self.observed_at_unix_ms == 0 {
            return Err(EvidenceError::InvalidField(
                "observed_at_unix_ms must be non-zero",
            ));
        }
        if self.event_id != self.compute_event_id()? {
            return Err(EvidenceError::EventIdMismatch);
        }
        Ok(())
    }
}

/// Cloneable, thread-safe runtime around one bounded [`EvidenceStore`]. File
/// appends run on Tokio's blocking pool, while routing snapshots clone only the
/// already bounded, privacy-minimal evidence records.
#[derive(Clone)]
pub struct EvidenceRuntime {
    store: Arc<Mutex<EvidenceStore>>,
    observer_peer_id: PeerId,
    operator_overrides: Arc<RwLock<BTreeMap<String, OperatorOverride>>>,
}

impl EvidenceRuntime {
    pub fn open(path: impl Into<PathBuf>, observer: PeerId) -> Result<Self, EvidenceError> {
        Self::from_store(EvidenceStore::open(path, observer)?)
    }

    pub fn from_store(store: EvidenceStore) -> Result<Self, EvidenceError> {
        let observer_peer_id = parse_peer_id("observer_peer_id", &store.observer_peer_id)?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            observer_peer_id,
            operator_overrides: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    pub fn observer_peer_id(&self) -> PeerId {
        self.observer_peer_id
    }

    pub fn set_operator_override(
        &self,
        peer_id: PeerId,
        operator: OperatorOverride,
    ) -> Result<(), EvidenceError> {
        let key = peer_id.to_string();
        let mut overrides = self
            .operator_overrides
            .write()
            .map_err(|_| EvidenceError::RuntimeLockPoisoned)?;
        if operator == OperatorOverride::default() {
            overrides.remove(&key);
            return Ok(());
        }
        if !overrides.contains_key(&key) && overrides.len() >= MAX_OPERATOR_OVERRIDES {
            return Err(EvidenceError::OperatorOverrideLimitExceeded);
        }
        overrides.insert(key, operator);
        Ok(())
    }

    pub fn operator_override(&self, peer_id: &PeerId) -> Result<OperatorOverride, EvidenceError> {
        self.operator_overrides
            .read()
            .map_err(|_| EvidenceError::RuntimeLockPoisoned)
            .map(|overrides| {
                overrides
                    .get(&peer_id.to_string())
                    .copied()
                    .unwrap_or_default()
            })
    }

    pub async fn append(&self, record: ExecutionEvidence) -> Result<AppendResult, EvidenceError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .map_err(|_| EvidenceError::RuntimeLockPoisoned)?
                .append(record)
        })
        .await
        .map_err(|error| EvidenceError::RuntimeJoin(error.to_string()))?
    }

    pub async fn record(
        &self,
        context: EvidenceContext,
        outcome: EvidenceOutcome,
        output_commitment: Option<[u8; 32]>,
    ) -> Result<AppendResult, EvidenceError> {
        self.append(ExecutionEvidence::new(context, outcome, output_commitment)?)
            .await
    }

    /// Atomically retain only recent records. Intended for the daemon's
    /// periodic maintenance task so a long-lived bounded store does not freeze
    /// routing assessments at the first size/count limit.
    pub async fn compact_retaining(&self, retention: Duration) -> Result<usize, EvidenceError> {
        if retention.is_zero() {
            return Err(EvidenceError::InvalidRetention);
        }
        let retain_since =
            unix_time_ms().saturating_sub(retention.as_millis().min(u128::from(u64::MAX)) as u64);
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            store
                .lock()
                .map_err(|_| EvidenceError::RuntimeLockPoisoned)?
                .compact(retain_since)
        })
        .await
        .map_err(|error| EvidenceError::RuntimeJoin(error.to_string()))?
    }

    pub async fn assess_peers(
        &self,
        peers: &[PeerId],
        now_unix_ms: u64,
    ) -> Result<Vec<PeerAssessment>, EvidenceError> {
        if peers.len() > MAX_RUNTIME_ASSESSMENT_PEERS {
            return Err(EvidenceError::AssessmentPeerLimitExceeded);
        }
        let store = Arc::clone(&self.store);
        let overrides = self
            .operator_overrides
            .read()
            .map_err(|_| EvidenceError::RuntimeLockPoisoned)?
            .clone();
        let observer = self.observer_peer_id;
        let peers = peers.to_vec();
        tokio::task::spawn_blocking(move || {
            let records = store
                .lock()
                .map_err(|_| EvidenceError::RuntimeLockPoisoned)?
                .records
                .clone();
            Ok(peers
                .iter()
                .map(|peer| {
                    assess_peer(
                        &observer,
                        peer,
                        &records,
                        now_unix_ms,
                        overrides
                            .get(&peer.to_string())
                            .copied()
                            .unwrap_or_default(),
                    )
                })
                .collect())
        })
        .await
        .map_err(|error| EvidenceError::RuntimeJoin(error.to_string()))?
    }
}

/// Result of an append attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendResult {
    Inserted,
    Duplicate,
}

/// Bounded, append-oriented evidence file. One mutable store is intended per
/// process; callers should not open concurrent writers for the same path.
#[derive(Debug)]
pub struct EvidenceStore {
    path: PathBuf,
    observer_peer_id: String,
    file: File,
    records: Vec<ExecutionEvidence>,
    event_indexes: BTreeMap<[u8; 32], usize>,
}

impl EvidenceStore {
    pub fn open(path: impl Into<PathBuf>, observer: PeerId) -> Result<Self, EvidenceError> {
        let path = path.into();
        if let Some(parent) = nonempty_parent(&path) {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let (mut file, created) = open_store_file(&path)?;
        if created {
            file.write_all(FILE_MAGIC)
                .and_then(|()| file.sync_all())
                .map_err(|source| io_error(&path, source))?;
        }

        let observer_peer_id = observer.to_string();
        let (records, last_good_offset) = load_records(&path, &observer_peer_id)?;
        let current_len = file
            .metadata()
            .map_err(|source| io_error(&path, source))?
            .len();
        if last_good_offset == 0 {
            file.set_len(0)
                .and_then(|()| file.write_all(FILE_MAGIC))
                .and_then(|()| file.sync_all())
                .map_err(|source| io_error(&path, source))?;
        } else if current_len != last_good_offset {
            file.set_len(last_good_offset)
                .and_then(|()| file.sync_all())
                .map_err(|source| io_error(&path, source))?;
        }

        let event_indexes = records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.event_id, index))
            .collect();
        Ok(Self {
            path,
            observer_peer_id,
            file,
            records,
            event_indexes,
        })
    }

    pub fn records(&self) -> &[ExecutionEvidence] {
        &self.records
    }

    pub fn append(&mut self, record: ExecutionEvidence) -> Result<AppendResult, EvidenceError> {
        self.append_at(record, unix_time_ms())
    }

    fn append_at(
        &mut self,
        record: ExecutionEvidence,
        now_unix_ms: u64,
    ) -> Result<AppendResult, EvidenceError> {
        self.validate_for_ingest(&record, now_unix_ms)?;
        if let Some(index) = self.event_indexes.get(&record.event_id) {
            if self.records[*index] == record {
                return Ok(AppendResult::Duplicate);
            }
            return Err(EvidenceError::EventIdCollision);
        }
        if self.records.len() >= MAX_EVIDENCE_RECORDS {
            return Err(EvidenceError::RecordLimitExceeded);
        }

        let frame = encode_frame(&record)?;
        let file_len = self
            .file
            .metadata()
            .map_err(|source| io_error(&self.path, source))?
            .len();
        let projected = file_len
            .checked_add(frame.len() as u64)
            .ok_or(EvidenceError::FileTooLarge)?;
        if projected > MAX_EVIDENCE_FILE_BYTES {
            return Err(EvidenceError::FileTooLarge);
        }
        self.file
            .write_all(&frame)
            .and_then(|()| self.file.sync_data())
            .map_err(|source| io_error(&self.path, source))?;

        let index = self.records.len();
        self.event_indexes.insert(record.event_id, index);
        self.records.push(record);
        Ok(AppendResult::Inserted)
    }

    /// Atomically rewrite the store with records at or after `retain_since`.
    /// Returns the number of expired records removed. This also eliminates any
    /// identical duplicate frames recovered from a retry after an uncertain
    /// append result.
    pub fn compact(&mut self, retain_since_unix_ms: u64) -> Result<usize, EvidenceError> {
        let retained: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.observed_at_unix_ms >= retain_since_unix_ms)
            .cloned()
            .collect();
        let removed = self.records.len().saturating_sub(retained.len());
        let temp_path = compact_temp_path(&self.path);
        let result = (|| {
            let mut temp = open_new_private(&temp_path)?;
            temp.write_all(FILE_MAGIC)
                .map_err(|source| io_error(&temp_path, source))?;
            let mut written = FILE_MAGIC.len() as u64;
            for record in &retained {
                let frame = encode_frame(record)?;
                written = written
                    .checked_add(frame.len() as u64)
                    .ok_or(EvidenceError::FileTooLarge)?;
                if written > MAX_EVIDENCE_FILE_BYTES {
                    return Err(EvidenceError::FileTooLarge);
                }
                temp.write_all(&frame)
                    .map_err(|source| io_error(&temp_path, source))?;
            }
            temp.sync_all()
                .map_err(|source| io_error(&temp_path, source))?;
            fs::rename(&temp_path, &self.path).map_err(|source| io_error(&self.path, source))?;
            Ok(temp)
        })();
        let replacement_file = match result {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
        };

        self.file = replacement_file;
        self.records = retained;
        self.event_indexes = self
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.event_id, index))
            .collect();
        if let Some(parent) = nonempty_parent(&self.path) {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| io_error(parent, source))?;
        }
        Ok(removed)
    }

    fn validate_for_ingest(
        &self,
        record: &ExecutionEvidence,
        now_unix_ms: u64,
    ) -> Result<(), EvidenceError> {
        record.validate_static()?;
        if record.observer_peer_id != self.observer_peer_id {
            return Err(EvidenceError::ObserverMismatch);
        }
        if record.observed_at_unix_ms > now_unix_ms.saturating_add(MAX_FUTURE_SKEW_MS) {
            return Err(EvidenceError::FutureTimestamp);
        }
        Ok(())
    }
}

/// Explicit local operator inputs. If both are true, block wins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OperatorOverride {
    pub pinned: bool,
    pub blocked: bool,
}

/// Advisory classification of a peer. It is intentionally not an authorization
/// decision and does not claim computation correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssessmentClass {
    Blocked,
    Pinned,
    ColdStart,
    Observed,
}

/// Deterministic local judgment derived from raw evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAssessment {
    pub peer_id: String,
    pub class: AssessmentClass,
    pub score: i16,
    pub confidence_permille: u16,
    pub evidence_count: usize,
    pub explanation: String,
}

/// Derive an advisory assessment for one peer. Evidence is recency-weighted;
/// confidence needs several recent observations, so a new identity cannot gain
/// high confidence through a self-claim or a single event.
pub fn assess_peer(
    observer_peer_id: &PeerId,
    peer_id: &PeerId,
    records: &[ExecutionEvidence],
    now_unix_ms: u64,
    operator: OperatorOverride,
) -> PeerAssessment {
    let observer = observer_peer_id.to_string();
    let peer = peer_id.to_string();
    let mut weighted_score = 0_i64;
    let mut total_weight = 0_u64;
    let mut evidence_count = 0_usize;

    for record in records
        .iter()
        .filter(|record| record.observer_peer_id == observer && record.remote_peer_id == peer)
    {
        evidence_count = evidence_count.saturating_add(1);
        let age = now_unix_ms.saturating_sub(record.observed_at_unix_ms);
        let weight =
            decay_weight(age).saturating_mul(outcome_confidence_weight(record.outcome)) / 1_000;
        if weight == 0 {
            continue;
        }
        weighted_score += i64::from(outcome_score(record.outcome)) * weight as i64;
        total_weight = total_weight.saturating_add(weight);
    }

    let score = if total_weight == 0 {
        0
    } else {
        (weighted_score / total_weight as i64).clamp(-100, 100) as i16
    };
    let confidence_permille = total_weight
        .saturating_mul(1_000)
        .checked_div(FULL_CONFIDENCE_WEIGHT)
        .unwrap_or(0)
        .min(1_000) as u16;

    let class = if operator.blocked {
        AssessmentClass::Blocked
    } else if operator.pinned {
        AssessmentClass::Pinned
    } else if confidence_permille < OBSERVED_CONFIDENCE_PERMILLE {
        AssessmentClass::ColdStart
    } else {
        AssessmentClass::Observed
    };
    let explanation = match class {
        AssessmentClass::Blocked => format!(
            "operator block takes precedence over pin and {evidence_count} local evidence records; reputation is advisory and does not prove correctness"
        ),
        AssessmentClass::Pinned => format!(
            "operator pin takes precedence over the derived score {score} at confidence {confidence_permille}/1000; reputation does not prove correctness"
        ),
        AssessmentClass::ColdStart => format!(
            "cold start: {evidence_count} decayed local evidence records, score {score}, confidence {confidence_permille}/1000; bounded opportunity requires external policy"
        ),
        AssessmentClass::Observed => format!(
            "derived from {evidence_count} decayed local evidence records: score {score}, confidence {confidence_permille}/1000; this is not proof of correctness"
        ),
    };

    PeerAssessment {
        peer_id: peer,
        class,
        score,
        confidence_permille,
        evidence_count,
        explanation,
    }
}

/// Ordering helper for callers that explicitly choose to consume assessments.
/// Better assessments sort first; exact ties fall back to canonical PeerId text.
pub fn compare_assessments(left: &PeerAssessment, right: &PeerAssessment) -> Ordering {
    assessment_rank(right.class)
        .cmp(&assessment_rank(left.class))
        .then_with(|| right.score.cmp(&left.score))
        .then_with(|| right.confidence_permille.cmp(&left.confidence_permille))
        .then_with(|| left.peer_id.cmp(&right.peer_id))
}

fn assessment_rank(class: AssessmentClass) -> u8 {
    match class {
        AssessmentClass::Blocked => 0,
        AssessmentClass::ColdStart => 1,
        AssessmentClass::Observed => 2,
        AssessmentClass::Pinned => 3,
    }
}

fn outcome_score(outcome: EvidenceOutcome) -> i16 {
    match outcome {
        EvidenceOutcome::VerifiedSuccessfulCompletion => 100,
        EvidenceOutcome::VerifiedCancellation
        | EvidenceOutcome::PolicyRefusal
        | EvidenceOutcome::CapacityRefusal
        | EvidenceOutcome::PreOutputTransportFailure
        | EvidenceOutcome::PreOutputDiscoveryFailure
        | EvidenceOutcome::MidStreamTransportLoss
        | EvidenceOutcome::DeadlineTimeout
        | EvidenceOutcome::IdleTimeout
        | EvidenceOutcome::RedundantExecutionDisagreement
        | EvidenceOutcome::RedundantExecutionIncomparableResult => 0,
        EvidenceOutcome::VerifiedWorkerError => -20,
        EvidenceOutcome::MissingTerminalEvent | EvidenceOutcome::MissingReceipt => -40,
        EvidenceOutcome::RedundantExecutionAgreement => 20,
        EvidenceOutcome::InvalidReceiptSignature
        | EvidenceOutcome::ManifestMismatch
        | EvidenceOutcome::JobMismatch
        | EvidenceOutcome::SignerPeerIdMismatch
        | EvidenceOutcome::OutputCommitmentMismatch
        | EvidenceOutcome::ChunkCountMismatch
        | EvidenceOutcome::SequenceMismatch
        | EvidenceOutcome::OperatorReviewedAbuseEvidence => -100,
    }
}

fn outcome_confidence_weight(outcome: EvidenceOutcome) -> u64 {
    match outcome {
        EvidenceOutcome::PolicyRefusal
        | EvidenceOutcome::CapacityRefusal
        | EvidenceOutcome::PreOutputTransportFailure
        | EvidenceOutcome::PreOutputDiscoveryFailure
        | EvidenceOutcome::MidStreamTransportLoss
        | EvidenceOutcome::DeadlineTimeout
        | EvidenceOutcome::IdleTimeout
        | EvidenceOutcome::RedundantExecutionDisagreement
        | EvidenceOutcome::RedundantExecutionIncomparableResult => 0,
        EvidenceOutcome::MissingTerminalEvent | EvidenceOutcome::MissingReceipt => 500,
        EvidenceOutcome::RedundantExecutionAgreement => 250,
        EvidenceOutcome::VerifiedSuccessfulCompletion
        | EvidenceOutcome::VerifiedCancellation
        | EvidenceOutcome::VerifiedWorkerError
        | EvidenceOutcome::InvalidReceiptSignature
        | EvidenceOutcome::ManifestMismatch
        | EvidenceOutcome::JobMismatch
        | EvidenceOutcome::SignerPeerIdMismatch
        | EvidenceOutcome::OutputCommitmentMismatch
        | EvidenceOutcome::ChunkCountMismatch
        | EvidenceOutcome::SequenceMismatch
        | EvidenceOutcome::OperatorReviewedAbuseEvidence => 1_000,
    }
}

fn decay_weight(age_ms: u64) -> u64 {
    let half_lives = age_ms / DECAY_HALF_LIFE_MS;
    if half_lives >= 11 {
        return 0;
    }
    let base = DECAY_UNIT >> half_lives;
    let next = base / 2;
    let remainder = age_ms % DECAY_HALF_LIFE_MS;
    base.saturating_sub(base.saturating_sub(next).saturating_mul(remainder) / DECAY_HALF_LIFE_MS)
}

fn validate_version(field: &'static str, value: &str, maximum: usize) -> Result<(), EvidenceError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(EvidenceError::InvalidVersion(field));
    }
    Ok(())
}

fn parse_peer_id(field: &'static str, value: &str) -> Result<PeerId, EvidenceError> {
    PeerId::from_str(value).map_err(|_| EvidenceError::InvalidPeerId(field))
}

fn encode_frame(record: &ExecutionEvidence) -> Result<Vec<u8>, EvidenceError> {
    let payload =
        postcard::to_stdvec(record).map_err(|error| EvidenceError::Encode(error.to_string()))?;
    if payload.is_empty() || payload.len() > MAX_EVIDENCE_RECORD_BYTES {
        return Err(EvidenceError::RecordTooLarge(payload.len()));
    }
    let length =
        u32::try_from(payload.len()).map_err(|_| EvidenceError::RecordTooLarge(payload.len()))?;
    let checksum = record_checksum(&payload);
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len() + CHECKSUM_BYTES);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&checksum);
    Ok(frame)
}

fn record_checksum(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CHECKSUM_DOMAIN);
    hasher.update(payload);
    hasher.finalize().into()
}

fn load_records(
    path: &Path,
    observer_peer_id: &str,
) -> Result<(Vec<ExecutionEvidence>, u64), EvidenceError> {
    let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
    if metadata.len() > MAX_EVIDENCE_FILE_BYTES {
        return Err(EvidenceError::FileTooLarge);
    }
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    if bytes.len() < FILE_MAGIC.len() {
        return Ok((Vec::new(), 0));
    }
    if &bytes[..FILE_MAGIC.len()] != FILE_MAGIC {
        return Err(EvidenceError::InvalidFileHeader);
    }

    let mut records = Vec::new();
    let mut indexes: BTreeMap<[u8; 32], usize> = BTreeMap::new();
    let mut physical_records = 0_usize;
    let mut cursor = FILE_MAGIC.len();
    while cursor < bytes.len() {
        let frame_start = cursor;
        if bytes.len() - cursor < LENGTH_PREFIX_BYTES {
            break;
        }
        let length = u32::from_le_bytes(
            bytes[cursor..cursor + LENGTH_PREFIX_BYTES]
                .try_into()
                .map_err(|_| EvidenceError::CorruptRecord(frame_start as u64))?,
        ) as usize;
        cursor += LENGTH_PREFIX_BYTES;
        if length == 0 || length > MAX_EVIDENCE_RECORD_BYTES {
            return Err(EvidenceError::RecordTooLarge(length));
        }
        let frame_end = cursor
            .checked_add(length)
            .and_then(|end| end.checked_add(CHECKSUM_BYTES))
            .ok_or(EvidenceError::CorruptRecord(frame_start as u64))?;
        if frame_end > bytes.len() {
            break;
        }
        physical_records = physical_records.saturating_add(1);
        if physical_records > MAX_EVIDENCE_RECORDS {
            return Err(EvidenceError::RecordLimitExceeded);
        }
        let payload = &bytes[cursor..cursor + length];
        let expected_checksum = record_checksum(payload);
        let actual_checksum = &bytes[cursor + length..frame_end];
        if actual_checksum != expected_checksum {
            return Err(EvidenceError::ChecksumMismatch(frame_start as u64));
        }
        let record: ExecutionEvidence = postcard::from_bytes(payload)
            .map_err(|error| EvidenceError::Decode(frame_start as u64, error.to_string()))?;
        record.validate_static()?;
        if record.observer_peer_id != observer_peer_id {
            return Err(EvidenceError::ObserverMismatch);
        }
        if let Some(existing) = indexes.get(&record.event_id) {
            if records[*existing] != record {
                return Err(EvidenceError::EventIdCollision);
            }
        } else {
            indexes.insert(record.event_id, records.len());
            records.push(record);
        }
        cursor = frame_end;
    }
    Ok((records, cursor as u64))
}

fn open_store_file(path: &Path) -> Result<(File, bool), EvidenceError> {
    match open_new_private(path) {
        Ok(file) => Ok((file, true)),
        Err(EvidenceError::Io { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
            open_existing_store(path).map(|file| (file, false))
        }
        Err(error) => Err(error),
    }
}

fn open_new_private(path: &Path) -> Result<File, EvidenceError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).append(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| io_error(path, source))
}

fn open_existing_store(path: &Path) -> Result<File, EvidenceError> {
    let path_metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if path_metadata.file_type().is_symlink() {
        return Err(EvidenceError::UnsafeStorePath(
            "symbolic links are forbidden",
        ));
    }
    if !path_metadata.is_file() {
        return Err(EvidenceError::UnsafeStorePath(
            "existing evidence store is not a regular file",
        ));
    }
    #[cfg(unix)]
    validate_private_unix_metadata(&path_metadata)?;

    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let opened_metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !opened_metadata.is_file() {
        return Err(EvidenceError::UnsafeStorePath(
            "opened evidence store is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        validate_private_unix_metadata(&opened_metadata)?;
        if path_metadata.dev() != opened_metadata.dev()
            || path_metadata.ino() != opened_metadata.ino()
        {
            return Err(EvidenceError::UnsafeStorePath(
                "evidence store changed while it was being opened",
            ));
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn validate_private_unix_metadata(metadata: &fs::Metadata) -> Result<(), EvidenceError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 || mode & 0o600 != 0o600 {
        return Err(EvidenceError::UnsafeStorePath(
            "evidence store must be owner-readable/writable and inaccessible to group/other",
        ));
    }
    Ok(())
}

fn compact_temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "evidence".into());
    name.push(format!(".compact-{}", uuid::Uuid::new_v4()));
    path.with_file_name(name)
}

fn nonempty_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn io_error(path: &Path, source: io::Error) -> EvidenceError {
    EvidenceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("evidence I/O failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe evidence-store path: {0}")]
    UnsafeStorePath(&'static str),
    #[error("unsupported evidence schema version {0}")]
    UnsupportedSchema(u16),
    #[error("invalid evidence field: {0}")]
    InvalidField(&'static str),
    #[error("invalid {0}")]
    InvalidPeerId(&'static str),
    #[error("invalid {0}: expected non-empty printable ASCII within its length bound")]
    InvalidVersion(&'static str),
    #[error("event ID does not match the evidence fields")]
    EventIdMismatch,
    #[error("evidence observer does not match this local store")]
    ObserverMismatch,
    #[error("evidence timestamp is too far in the future")]
    FutureTimestamp,
    #[error("evidence event ID collision")]
    EventIdCollision,
    #[error("evidence record is too large: {0} bytes")]
    RecordTooLarge(usize),
    #[error("evidence file exceeds its size limit")]
    FileTooLarge,
    #[error("evidence record-count limit exceeded")]
    RecordLimitExceeded,
    #[error("evidence runtime assessment peer limit exceeded")]
    AssessmentPeerLimitExceeded,
    #[error("evidence runtime operator override limit exceeded")]
    OperatorOverrideLimitExceeded,
    #[error("evidence retention duration must be greater than zero")]
    InvalidRetention,
    #[error("evidence runtime lock was poisoned")]
    RuntimeLockPoisoned,
    #[error("evidence runtime blocking task failed: {0}")]
    RuntimeJoin(String),
    #[error("invalid evidence file header")]
    InvalidFileHeader,
    #[error("evidence checksum mismatch at byte offset {0}")]
    ChecksumMismatch(u64),
    #[error("corrupt evidence record at byte offset {0}")]
    CorruptRecord(u64),
    #[error("could not encode evidence: {0}")]
    Encode(String),
    #[error("could not decode evidence at byte offset {0}: {1}")]
    Decode(u64, String),
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use tempfile::tempdir;

    use super::*;

    fn context(
        observer: PeerId,
        remote: PeerId,
        timestamp: u64,
        discriminator: u8,
    ) -> EvidenceContext {
        EvidenceContext {
            observer_peer_id: observer,
            remote_peer_id: remote,
            job_spec_hash: [discriminator; 32],
            job_class: JobSpecKind::Inference,
            model_cid: ModelCid([discriminator.saturating_add(1); 32]),
            protocol_version: "/phase/job-relay-stream/2.0.0".to_string(),
            software_version: "lucidd/0.2.0".to_string(),
            observed_at_unix_ms: timestamp,
        }
    }

    fn evidence(
        observer: PeerId,
        remote: PeerId,
        timestamp: u64,
        discriminator: u8,
        outcome: EvidenceOutcome,
    ) -> ExecutionEvidence {
        ExecutionEvidence::new(
            context(observer, remote, timestamp, discriminator),
            outcome,
            Some([discriminator.saturating_add(2); 32]),
        )
        .unwrap()
    }

    #[test]
    fn duplicate_ingestion_is_idempotent_and_event_id_is_stable() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("evidence.log");
        let observer = PeerId::random();
        let remote = PeerId::random();
        let record = evidence(
            observer,
            remote,
            1_000,
            1,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        let duplicate = evidence(
            observer,
            remote,
            1_000,
            1,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        assert_eq!(record.event_id(), duplicate.event_id());

        let mut store = EvidenceStore::open(&path, observer).unwrap();
        assert_eq!(
            store.append_at(record, 1_000).unwrap(),
            AppendResult::Inserted
        );
        let length_after_insert = fs::metadata(&path).unwrap().len();
        assert_eq!(
            store.append_at(duplicate, 1_000).unwrap(),
            AppendResult::Duplicate
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), length_after_insert);
        drop(store);
        assert_eq!(
            EvidenceStore::open(path, observer).unwrap().records().len(),
            1
        );
    }

    #[test]
    fn forged_identity_event_id_and_invalid_fields_are_rejected() {
        let temp = tempdir().unwrap();
        let observer = PeerId::random();
        let remote = PeerId::random();
        let mut store = EvidenceStore::open(temp.path().join("evidence.log"), observer).unwrap();

        let mut forged_observer = evidence(
            observer,
            remote,
            1_000,
            1,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        forged_observer.observer_peer_id = PeerId::random().to_string();
        forged_observer.event_id = forged_observer.compute_event_id().unwrap();
        assert!(matches!(
            store.append_at(forged_observer, 1_000),
            Err(EvidenceError::ObserverMismatch)
        ));

        let mut forged_remote = evidence(
            observer,
            remote,
            1_000,
            2,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        forged_remote.remote_peer_id = "not-a-peer-id".to_string();
        forged_remote.event_id = forged_remote.compute_event_id().unwrap();
        assert!(matches!(
            store.append_at(forged_remote, 1_000),
            Err(EvidenceError::InvalidPeerId("remote_peer_id"))
        ));

        let mut forged_event_id = evidence(
            observer,
            remote,
            1_000,
            3,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        forged_event_id.event_id[0] ^= 0xff;
        assert!(matches!(
            store.append_at(forged_event_id, 1_000),
            Err(EvidenceError::EventIdMismatch)
        ));

        let mut invalid_version = context(observer, remote, 1_000, 4);
        invalid_version.protocol_version = "bad\nversion".to_string();
        assert!(matches!(
            ExecutionEvidence::new(
                invalid_version,
                EvidenceOutcome::VerifiedSuccessfulCompletion,
                None,
            ),
            Err(EvidenceError::InvalidVersion("protocol_version"))
        ));

        let mut zero_hash = context(observer, remote, 1_000, 5);
        zero_hash.job_spec_hash = [0; 32];
        assert!(matches!(
            ExecutionEvidence::new(
                zero_hash,
                EvidenceOutcome::VerifiedSuccessfulCompletion,
                None,
            ),
            Err(EvidenceError::InvalidField(_))
        ));

        assert!(matches!(
            ExecutionEvidence::new(
                context(observer, remote, 1_000, 6),
                EvidenceOutcome::VerifiedSuccessfulCompletion,
                Some([0; 32]),
            ),
            Err(EvidenceError::InvalidField(_))
        ));
    }

    #[test]
    fn manipulated_timestamps_are_rejected() {
        let temp = tempdir().unwrap();
        let observer = PeerId::random();
        let remote = PeerId::random();
        let mut store = EvidenceStore::open(temp.path().join("evidence.log"), observer).unwrap();

        assert!(matches!(
            ExecutionEvidence::new(
                context(observer, remote, 0, 1),
                EvidenceOutcome::VerifiedSuccessfulCompletion,
                None,
            ),
            Err(EvidenceError::InvalidField(_))
        ));
        let future = evidence(
            observer,
            remote,
            1_000 + MAX_FUTURE_SKEW_MS + 1,
            2,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        assert!(matches!(
            store.append_at(future, 1_000),
            Err(EvidenceError::FutureTimestamp)
        ));
    }

    #[test]
    fn partial_tail_is_recovered_but_full_record_corruption_is_detected() {
        let temp = tempdir().unwrap();
        let partial_header_path = temp.path().join("partial-header.log");
        fs::write(&partial_header_path, &FILE_MAGIC[..3]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&partial_header_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        drop(EvidenceStore::open(&partial_header_path, PeerId::random()).unwrap());
        assert_eq!(fs::read(&partial_header_path).unwrap(), FILE_MAGIC);

        let partial_path = temp.path().join("partial.log");
        let observer = PeerId::random();
        let remote = PeerId::random();
        let mut store = EvidenceStore::open(&partial_path, observer).unwrap();
        store
            .append_at(
                evidence(
                    observer,
                    remote,
                    1_000,
                    1,
                    EvidenceOutcome::VerifiedSuccessfulCompletion,
                ),
                1_000,
            )
            .unwrap();
        drop(store);
        let clean_length = fs::metadata(&partial_path).unwrap().len();
        OpenOptions::new()
            .append(true)
            .open(&partial_path)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        let recovered = EvidenceStore::open(&partial_path, observer).unwrap();
        assert_eq!(recovered.records().len(), 1);
        assert_eq!(fs::metadata(&partial_path).unwrap().len(), clean_length);
        drop(recovered);

        let corrupt_path = temp.path().join("corrupt.log");
        let mut store = EvidenceStore::open(&corrupt_path, observer).unwrap();
        store
            .append_at(
                evidence(
                    observer,
                    remote,
                    2_000,
                    2,
                    EvidenceOutcome::VerifiedSuccessfulCompletion,
                ),
                2_000,
            )
            .unwrap();
        drop(store);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&corrupt_path)
            .unwrap();
        file.seek(SeekFrom::Start(
            (FILE_MAGIC.len() + LENGTH_PREFIX_BYTES + 1) as u64,
        ))
        .unwrap();
        let mut byte = [0; 1];
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Current(-1)).unwrap();
        byte[0] ^= 0xff;
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();
        assert!(matches!(
            EvidenceStore::open(corrupt_path, observer),
            Err(EvidenceError::ChecksumMismatch(_))
        ));
    }

    #[test]
    fn oversized_length_prefix_is_corruption_not_a_partial_tail() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("oversized.log");
        let observer = PeerId::random();
        drop(EvidenceStore::open(&path, observer).unwrap());
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&((MAX_EVIDENCE_RECORD_BYTES as u32) + 1).to_le_bytes())
            .unwrap();
        assert!(matches!(
            EvidenceStore::open(path, observer),
            Err(EvidenceError::RecordTooLarge(_))
        ));
    }

    #[test]
    fn retention_compaction_is_stable_and_reopenable() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("evidence.log");
        let observer = PeerId::random();
        let remote = PeerId::random();
        let mut store = EvidenceStore::open(&path, observer).unwrap();
        for (timestamp, discriminator) in [(100, 1), (200, 2), (300, 3)] {
            store
                .append_at(
                    evidence(
                        observer,
                        remote,
                        timestamp,
                        discriminator,
                        EvidenceOutcome::VerifiedSuccessfulCompletion,
                    ),
                    1_000,
                )
                .unwrap();
        }
        assert_eq!(store.compact(200).unwrap(), 1);
        assert_eq!(
            store
                .records()
                .iter()
                .map(ExecutionEvidence::observed_at_unix_ms)
                .collect::<Vec<_>>(),
            vec![200, 300]
        );
        drop(store);
        assert_eq!(
            EvidenceStore::open(path, observer).unwrap().records().len(),
            2
        );
    }

    #[tokio::test]
    async fn runtime_compaction_rejects_zero_retention() {
        let temp = tempdir().unwrap();
        let runtime =
            EvidenceRuntime::open(temp.path().join("runtime-evidence.log"), PeerId::random())
                .unwrap();
        assert!(matches!(
            runtime.compact_retaining(Duration::ZERO).await,
            Err(EvidenceError::InvalidRetention)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn new_store_is_private_at_creation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let path = temp.path().join("evidence.log");
        let store = EvidenceStore::open(&path, PeerId::random()).unwrap();
        let mode = store.file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn existing_store_rejects_symlink_nonregular_and_permissive_paths() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempdir().unwrap();
        let observer = PeerId::random();
        let target = temp.path().join("target.log");
        drop(EvidenceStore::open(&target, observer).unwrap());

        let link = temp.path().join("link.log");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            EvidenceStore::open(&link, observer),
            Err(EvidenceError::UnsafeStorePath(_))
        ));

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            EvidenceStore::open(&target, observer),
            Err(EvidenceError::UnsafeStorePath(_))
        ));

        assert!(matches!(
            EvidenceStore::open(temp.path(), observer),
            Err(EvidenceError::UnsafeStorePath(_))
        ));
    }

    #[test]
    fn assessment_has_decay_confidence_and_cold_start() {
        let observer = PeerId::random();
        let remote = PeerId::random();
        let now = 30 * 24 * 60 * 60 * 1_000;
        let mut records = vec![evidence(
            observer,
            remote,
            now - 2 * DECAY_HALF_LIFE_MS,
            1,
            EvidenceOutcome::InvalidReceiptSignature,
        )];
        records.push(evidence(
            observer,
            remote,
            now,
            2,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        ));

        let assessment = assess_peer(
            &observer,
            &remote,
            &records,
            now,
            OperatorOverride::default(),
        );
        assert_eq!(assessment.class, AssessmentClass::ColdStart);
        assert!(
            assessment.score > 0,
            "recent success should outweigh decayed failure"
        );
        assert!(assessment.confidence_permille < OBSERVED_CONFIDENCE_PERMILLE);

        for discriminator in 3..=9 {
            records.push(evidence(
                observer,
                remote,
                now,
                discriminator,
                EvidenceOutcome::VerifiedSuccessfulCompletion,
            ));
        }
        let observed = assess_peer(
            &observer,
            &remote,
            &records,
            now,
            OperatorOverride::default(),
        );
        assert_eq!(observed.class, AssessmentClass::Observed);
        assert!(observed.confidence_permille >= OBSERVED_CONFIDENCE_PERMILLE);
        assert_eq!(
            observed,
            assess_peer(
                &observer,
                &remote,
                &records,
                now,
                OperatorOverride::default(),
            )
        );
    }

    #[test]
    fn sybil_identities_remain_cold_without_local_evidence() {
        let now = 1_000;
        let local_observer = PeerId::random();
        for _ in 0..64 {
            let sybil = PeerId::random();
            let voucher = PeerId::random();
            let self_claim = evidence(
                voucher,
                sybil,
                now,
                1,
                EvidenceOutcome::VerifiedSuccessfulCompletion,
            );
            let assessment = assess_peer(
                &local_observer,
                &sybil,
                &[self_claim],
                now,
                OperatorOverride::default(),
            );
            assert_eq!(assessment.class, AssessmentClass::ColdStart);
            assert_eq!(assessment.confidence_permille, 0);
            assert_eq!(assessment.score, 0);
        }
    }

    #[test]
    fn operator_block_precedes_pin_and_ties_are_deterministic() {
        let observer = PeerId::random();
        let first_peer = PeerId::random();
        let second_peer = PeerId::random();
        let blocked = assess_peer(
            &observer,
            &first_peer,
            &[],
            1_000,
            OperatorOverride {
                pinned: true,
                blocked: true,
            },
        );
        assert_eq!(blocked.class, AssessmentClass::Blocked);
        assert!(blocked.explanation.contains("block takes precedence"));

        let pinned = assess_peer(
            &observer,
            &first_peer,
            &[],
            1_000,
            OperatorOverride {
                pinned: true,
                blocked: false,
            },
        );
        assert_eq!(pinned.class, AssessmentClass::Pinned);

        let first = assess_peer(
            &observer,
            &first_peer,
            &[],
            1_000,
            OperatorOverride::default(),
        );
        let second = assess_peer(
            &observer,
            &second_peer,
            &[],
            1_000,
            OperatorOverride::default(),
        );
        let expected = first.peer_id.cmp(&second.peer_id);
        assert_eq!(compare_assessments(&first, &second), expected);
    }

    #[test]
    fn serialized_schema_contains_no_private_payload_fields() {
        let record = evidence(
            PeerId::random(),
            PeerId::random(),
            1_000,
            1,
            EvidenceOutcome::VerifiedSuccessfulCompletion,
        );
        let value = serde_json::to_value(record).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "event_id",
                "job_class",
                "job_spec_hash",
                "model_cid",
                "observed_at_unix_ms",
                "observer_peer_id",
                "outcome",
                "output_commitment",
                "protocol_version",
                "remote_peer_id",
                "schema_version",
                "software_version",
            ]
        );
        for prohibited in ["prompt", "token", "embedding", "model_bytes", "output"] {
            assert!(!value.as_object().unwrap().contains_key(prohibited));
        }
    }

    #[test]
    fn pre_output_failures_reject_commitments_and_add_no_confidence() {
        let observer = PeerId::random();
        let remote = PeerId::random();
        let outcomes = [
            EvidenceOutcome::PolicyRefusal,
            EvidenceOutcome::CapacityRefusal,
            EvidenceOutcome::PreOutputTransportFailure,
            EvidenceOutcome::PreOutputDiscoveryFailure,
        ];
        let mut records = Vec::new();
        for (index, outcome) in outcomes.into_iter().enumerate() {
            let context = context(observer, remote, 1_000 + index as u64, index as u8 + 1);
            assert!(matches!(
                ExecutionEvidence::new(context.clone(), outcome, Some([9; 32])),
                Err(EvidenceError::InvalidField(_))
            ));
            records.push(ExecutionEvidence::new(context, outcome, None).unwrap());
        }
        let assessment = assess_peer(
            &observer,
            &remote,
            &records,
            1_000,
            OperatorOverride::default(),
        );
        assert_eq!(assessment.score, 0);
        assert_eq!(assessment.confidence_permille, 0);
        assert_eq!(assessment.class, AssessmentClass::ColdStart);
    }

    #[tokio::test]
    async fn runtime_serializes_appends_and_applies_bounded_operator_overrides() {
        let temp = tempdir().unwrap();
        let observer = PeerId::random();
        let remote = PeerId::random();
        let runtime = EvidenceRuntime::open(temp.path().join("runtime.log"), observer).unwrap();
        let mut tasks = Vec::new();
        for discriminator in 1..=16_u8 {
            let runtime = runtime.clone();
            tasks.push(tokio::spawn(async move {
                runtime
                    .append(evidence(
                        observer,
                        remote,
                        1_000 + u64::from(discriminator),
                        discriminator,
                        EvidenceOutcome::VerifiedSuccessfulCompletion,
                    ))
                    .await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), AppendResult::Inserted);
        }
        let assessment = runtime
            .assess_peers(&[remote], 2_000)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(assessment.evidence_count, 16);
        assert_eq!(assessment.class, AssessmentClass::Observed);

        runtime
            .set_operator_override(
                remote,
                OperatorOverride {
                    pinned: true,
                    blocked: true,
                },
            )
            .unwrap();
        let blocked = runtime
            .assess_peers(&[remote], 2_000)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(blocked.class, AssessmentClass::Blocked);

        {
            let mut overrides = runtime.operator_overrides.write().unwrap();
            overrides.clear();
            for index in 0..MAX_OPERATOR_OVERRIDES {
                overrides.insert(format!("synthetic-{index}"), OperatorOverride::default());
            }
        }
        assert!(matches!(
            runtime.set_operator_override(
                PeerId::random(),
                OperatorOverride {
                    pinned: true,
                    blocked: false,
                }
            ),
            Err(EvidenceError::OperatorOverrideLimitExceeded)
        ));
        assert!(matches!(
            runtime
                .assess_peers(
                    &vec![PeerId::random(); MAX_RUNTIME_ASSESSMENT_PEERS + 1],
                    2_000
                )
                .await,
            Err(EvidenceError::AssessmentPeerLimitExceeded)
        ));
    }

    #[test]
    fn wire_taxonomy_keeps_every_release_outcome_distinct() {
        let outcomes = [
            EvidenceOutcome::VerifiedSuccessfulCompletion,
            EvidenceOutcome::VerifiedCancellation,
            EvidenceOutcome::VerifiedWorkerError,
            EvidenceOutcome::PolicyRefusal,
            EvidenceOutcome::CapacityRefusal,
            EvidenceOutcome::PreOutputTransportFailure,
            EvidenceOutcome::PreOutputDiscoveryFailure,
            EvidenceOutcome::MidStreamTransportLoss,
            EvidenceOutcome::MissingTerminalEvent,
            EvidenceOutcome::MissingReceipt,
            EvidenceOutcome::InvalidReceiptSignature,
            EvidenceOutcome::ManifestMismatch,
            EvidenceOutcome::JobMismatch,
            EvidenceOutcome::SignerPeerIdMismatch,
            EvidenceOutcome::OutputCommitmentMismatch,
            EvidenceOutcome::ChunkCountMismatch,
            EvidenceOutcome::SequenceMismatch,
            EvidenceOutcome::DeadlineTimeout,
            EvidenceOutcome::IdleTimeout,
            EvidenceOutcome::RedundantExecutionAgreement,
            EvidenceOutcome::RedundantExecutionDisagreement,
            EvidenceOutcome::RedundantExecutionIncomparableResult,
            EvidenceOutcome::OperatorReviewedAbuseEvidence,
        ];
        assert_eq!(
            serde_json::to_value(outcomes).unwrap(),
            serde_json::json!([
                "verified_successful_completion",
                "verified_cancellation",
                "verified_worker_error",
                "policy_refusal",
                "capacity_refusal",
                "pre_output_transport_failure",
                "pre_output_discovery_failure",
                "mid_stream_transport_loss",
                "missing_terminal_event",
                "missing_receipt",
                "invalid_receipt_signature",
                "manifest_mismatch",
                "job_mismatch",
                "signer_peer_id_mismatch",
                "output_commitment_mismatch",
                "chunk_count_mismatch",
                "sequence_mismatch",
                "deadline_timeout",
                "idle_timeout",
                "redundant_execution_agreement",
                "redundant_execution_disagreement",
                "redundant_execution_incomparable_result",
                "operator_reviewed_abuse_evidence",
            ])
        );
    }
}
