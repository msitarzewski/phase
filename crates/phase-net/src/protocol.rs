// SPDX-License-Identifier: Apache-2.0

//! Wire-level protocol types for the JobOffer / JobResponse exchange.
//!
//! These types deliberately keep the same field shape and JSON wire format
//! they had inside `daemon/src/network/protocol.rs` before phase-core M2 —
//! the M2 extraction is strictly a relocation, not a wire-format change.
//! Generalization of the request/result shape against the new Worker trait
//! happens in `phase-protocol`'s `JobSpec` / `JobResult`; the types here
//! remain in the coarse-grained "is this peer willing to take this job"
//! shape that the November 2025 MVP shipped.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// JobRelay — LUCID M5 peer-relay wire format.
// ---------------------------------------------------------------------------

/// Outer envelope for an inference job a requesting peer asks a serving
/// peer to execute on its behalf. The `payload` is an opaque, bincode-
/// encoded `SignedManifest<JobSpec>` (LUCID owns that schema); phase-net
/// stays inference-agnostic and only ferries the bytes.
///
/// CBOR-encoded on the wire (via libp2p's `cbor::Behaviour`). CBOR was
/// picked over JSON because the inner payload is binary — JSON would
/// expand each byte 4-5×.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRelayRequest {
    /// bincode(`SignedManifest<JobSpec>`). Decoded by the serving side.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

/// Serving peer's response. Either a successful batch of `JobEvent`s
/// (bincode-encoded by lucidd) or a structured error. This legacy v1 path is
/// batch-only: the serving peer drains the local worker's stream and ships
/// every `JobEvent` in one shot. Live token delivery uses the distinct v2
/// protocol defined below and is never represented as a v1 success.
///
/// ## SEC-05 schema v2: `Ok.receipt`
///
/// v1 of `Ok` carried only `events`; the worker's `SignedReceipt<JobResult>`
/// stayed local and the requesting side had no cryptographic proof that a
/// *specific* worker ran a *specific* job. SEC-05 adds the `receipt` field so
/// the requester can `verify()` the signature, bind `job_id` →
/// dispatched-manifest-hash, bind `worker_pubkey` → dispatched-PeerId, and
/// recompute the output commitment over the received chunks.
///
/// The field is `#[serde(default)]` so a v1 serving peer (no receipt) still
/// deserializes — the requesting side treats an empty receipt as
/// "unverifiable" rather than failing the round-trip, preserving
/// wire-compatibility with pre-SEC-05 nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobRelayResponse {
    /// bincode(Vec<JobEvent>) plus the worker's signed receipt.
    Ok {
        #[serde(with = "serde_bytes")]
        events: Vec<u8>,
        /// JSON(`SignedReceipt<JobResult>`), owned by lucidd. Empty on a
        /// v1 (pre-SEC-05) serving peer. phase-net stays receipt-agnostic
        /// and only ferries the bytes.
        #[serde(with = "serde_bytes", default)]
        receipt: Vec<u8>,
    },
    /// Serving peer refused or hit an in-flight error.
    Err { reason: String },
}

// ---------------------------------------------------------------------------
// JobRelay v2 — live, bounded, bidirectional stream framing.
// ---------------------------------------------------------------------------

/// The v2 live relay protocol is a distinct negotiation target.  The v1
/// request/response protocol above remains batch-shaped and is never decoded
/// as a live stream by accident.
pub const JOB_RELAY_STREAM_PROTOCOL: &str = "/phase/job-relay/2.0.0";

/// Schema version carried inside every v2 frame.
pub const JOB_RELAY_STREAM_SCHEMA_VERSION: u16 = 1;

/// Maximum opaque signed-manifest envelope accepted on a live relay stream.
pub const JOB_RELAY_STREAM_MAX_OPEN_BYTES: usize = 256 * 1024;

/// Maximum opaque worker event carried by one live relay frame.
pub const JOB_RELAY_STREAM_MAX_EVENT_BYTES: usize = 1024 * 1024;

/// Maximum encoded signed receipt carried after the terminal event.
pub const JOB_RELAY_STREAM_MAX_RECEIPT_BYTES: usize = 256 * 1024;

/// Maximum human-readable refusal, failure, or cancellation detail.
pub const JOB_RELAY_STREAM_MAX_REASON_BYTES: usize = 1024;

/// Liveness bounds negotiated in every live-relay open envelope. The idle
/// timeout applies to every response frame, including the initial decision;
/// the absolute deadline remains the hard ceiling for the complete job.
pub const JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS: u64 = 250;
pub const JOB_RELAY_STREAM_MAX_IDLE_TIMEOUT_MS: u64 = 60_000;
pub const JOB_RELAY_STREAM_DEFAULT_IDLE_TIMEOUT_MS: u64 = 30_000;
pub const JOB_RELAY_STREAM_MAX_DEADLINE_AHEAD_MS: u64 = 15 * 60 * 1000;

/// First requester-to-server frame on a v2 stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRelayStreamOpen {
    pub schema_version: u16,
    pub job_id: [u8; 32],
    /// Opaque encoded `SignedManifest<JobSpec>` owned by the workload layer.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    /// Absolute requester deadline. A zero value is invalid.
    pub deadline_unix_ms: u64,
    /// Maximum silence between protocol frames in either direction.
    pub idle_timeout_ms: u64,
}

impl JobRelayStreamOpen {
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), JobRelayStreamError> {
        validate_schema(self.schema_version)?;
        if self.payload.is_empty() || self.payload.len() > JOB_RELAY_STREAM_MAX_OPEN_BYTES {
            return Err(JobRelayStreamError::InvalidOpenSize {
                actual: self.payload.len(),
                maximum: JOB_RELAY_STREAM_MAX_OPEN_BYTES,
            });
        }
        if !(JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS..=JOB_RELAY_STREAM_MAX_IDLE_TIMEOUT_MS)
            .contains(&self.idle_timeout_ms)
        {
            return Err(JobRelayStreamError::InvalidIdleTimeout {
                actual_ms: self.idle_timeout_ms,
                minimum_ms: JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS,
                maximum_ms: JOB_RELAY_STREAM_MAX_IDLE_TIMEOUT_MS,
            });
        }
        let deadline_ahead = self
            .deadline_unix_ms
            .checked_sub(now_unix_ms)
            .ok_or(JobRelayStreamError::ExpiredDeadline)?;
        if deadline_ahead == 0 {
            return Err(JobRelayStreamError::ExpiredDeadline);
        }
        if deadline_ahead > JOB_RELAY_STREAM_MAX_DEADLINE_AHEAD_MS {
            return Err(JobRelayStreamError::DeadlineTooFar {
                maximum_ahead_ms: JOB_RELAY_STREAM_MAX_DEADLINE_AHEAD_MS,
            });
        }
        Ok(())
    }
}

/// Requester-to-server control frames after the open envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRelayStreamControl {
    pub schema_version: u16,
    pub job_id: [u8; 32],
    pub kind: JobRelayStreamControlKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobRelayStreamControlKind {
    Cancel {
        reason: String,
    },
    /// Confirms that the requester received and verified the terminal receipt.
    ReceiptAck,
}

impl JobRelayStreamControl {
    pub fn validate_for(&self, job_id: [u8; 32]) -> Result<(), JobRelayStreamError> {
        validate_schema(self.schema_version)?;
        validate_job_id(self.job_id, job_id)?;
        if let JobRelayStreamControlKind::Cancel { reason } = &self.kind {
            validate_reason(reason)?;
        }
        Ok(())
    }
}

/// Server-to-requester frame. `payload` stays opaque to `phase-net`; the
/// LUCID router owns `JobEvent` and signed-receipt decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRelayStreamFrame {
    pub schema_version: u16,
    pub job_id: [u8; 32],
    /// Strictly contiguous sequence, beginning at zero for the decision.
    pub sequence: u64,
    pub kind: JobRelayStreamFrameKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobRelayStreamFrameKind {
    Accepted,
    Rejected {
        reason: String,
    },
    /// Opaque encoded `JobEvent`. `terminal` declares whether it is the one
    /// and only `JobEvent::Final`; the workload layer must verify that claim.
    Event {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        terminal: bool,
    },
    /// Opaque encoded `SignedReceipt<JobResult>`, permitted only immediately
    /// after the terminal event.
    Receipt {
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },
    /// Attributable transport/serving failure. This is terminal and can
    /// never be interpreted as successful completion.
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobRelayStreamState {
    AwaitingDecision,
    Streaming,
    AwaitingReceipt,
    Complete,
}

/// Fail-closed v2 response state machine. It enforces transport-level
/// ordering and bounds before the workload layer allocates or decodes an
/// opaque event. A separate instance is required for each accepted stream.
#[derive(Debug, Clone)]
pub struct JobRelayStreamValidator {
    job_id: [u8; 32],
    next_sequence: u64,
    state: JobRelayStreamState,
}

impl JobRelayStreamValidator {
    pub fn new(job_id: [u8; 32]) -> Self {
        Self {
            job_id,
            next_sequence: 0,
            state: JobRelayStreamState::AwaitingDecision,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.state == JobRelayStreamState::Complete
    }

    pub fn validate(&mut self, frame: &JobRelayStreamFrame) -> Result<(), JobRelayStreamError> {
        validate_schema(frame.schema_version)?;
        validate_job_id(frame.job_id, self.job_id)?;
        if frame.sequence != self.next_sequence {
            return Err(JobRelayStreamError::UnexpectedSequence {
                expected: self.next_sequence,
                actual: frame.sequence,
            });
        }

        let next_state = match (self.state, &frame.kind) {
            (JobRelayStreamState::AwaitingDecision, JobRelayStreamFrameKind::Accepted) => {
                JobRelayStreamState::Streaming
            }
            (
                JobRelayStreamState::AwaitingDecision,
                JobRelayStreamFrameKind::Rejected { reason },
            ) => {
                validate_reason(reason)?;
                JobRelayStreamState::Complete
            }
            (
                JobRelayStreamState::Streaming,
                JobRelayStreamFrameKind::Event { payload, terminal },
            ) => {
                validate_payload(payload, JOB_RELAY_STREAM_MAX_EVENT_BYTES, "event")?;
                if *terminal {
                    JobRelayStreamState::AwaitingReceipt
                } else {
                    JobRelayStreamState::Streaming
                }
            }
            (JobRelayStreamState::Streaming, JobRelayStreamFrameKind::Failed { reason })
            | (JobRelayStreamState::AwaitingReceipt, JobRelayStreamFrameKind::Failed { reason }) => {
                validate_reason(reason)?;
                JobRelayStreamState::Complete
            }
            (
                JobRelayStreamState::AwaitingReceipt,
                JobRelayStreamFrameKind::Receipt { payload },
            ) => {
                validate_payload(payload, JOB_RELAY_STREAM_MAX_RECEIPT_BYTES, "receipt")?;
                JobRelayStreamState::Complete
            }
            (state, kind) => {
                return Err(JobRelayStreamError::InvalidTransition {
                    state: state.as_str(),
                    frame: kind.name(),
                });
            }
        };

        self.state = next_state;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(JobRelayStreamError::SequenceOverflow)?;
        Ok(())
    }

    /// EOF is valid only after an explicit rejection, failure, or receipt.
    pub fn validate_eof(&self) -> Result<(), JobRelayStreamError> {
        if self.is_complete() {
            Ok(())
        } else {
            Err(JobRelayStreamError::UnexpectedEof)
        }
    }
}

impl JobRelayStreamState {
    fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingDecision => "awaiting_decision",
            Self::Streaming => "streaming",
            Self::AwaitingReceipt => "awaiting_receipt",
            Self::Complete => "complete",
        }
    }
}

impl JobRelayStreamFrameKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::Event { .. } => "event",
            Self::Receipt { .. } => "receipt",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JobRelayStreamError {
    #[error("unsupported live relay schema {actual}; expected {expected}")]
    UnsupportedSchema { actual: u16, expected: u16 },
    #[error("live relay frame belongs to a different job")]
    WrongJob,
    #[error("expected live relay sequence {expected}, got {actual}")]
    UnexpectedSequence { expected: u64, actual: u64 },
    #[error("invalid live relay transition from {state} using {frame}")]
    InvalidTransition {
        state: &'static str,
        frame: &'static str,
    },
    #[error("{kind} payload is empty or exceeds {maximum} bytes (got {actual})")]
    InvalidPayloadSize {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("open payload is empty or exceeds {maximum} bytes (got {actual})")]
    InvalidOpenSize { actual: usize, maximum: usize },
    #[error("live relay deadline has already expired")]
    ExpiredDeadline,
    #[error("live relay idle timeout {actual_ms}ms is outside {minimum_ms}..={maximum_ms}ms")]
    InvalidIdleTimeout {
        actual_ms: u64,
        minimum_ms: u64,
        maximum_ms: u64,
    },
    #[error("live relay deadline exceeds {maximum_ahead_ms}ms maximum horizon")]
    DeadlineTooFar { maximum_ahead_ms: u64 },
    #[error("live relay reason is empty or exceeds its size limit")]
    InvalidReason,
    #[error("live relay stream ended before an explicit terminal frame")]
    UnexpectedEof,
    #[error("live relay sequence overflow")]
    SequenceOverflow,
}

fn validate_schema(schema_version: u16) -> Result<(), JobRelayStreamError> {
    if schema_version == JOB_RELAY_STREAM_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(JobRelayStreamError::UnsupportedSchema {
            actual: schema_version,
            expected: JOB_RELAY_STREAM_SCHEMA_VERSION,
        })
    }
}

fn validate_job_id(actual: [u8; 32], expected: [u8; 32]) -> Result<(), JobRelayStreamError> {
    if actual == expected {
        Ok(())
    } else {
        Err(JobRelayStreamError::WrongJob)
    }
}

fn validate_payload(
    payload: &[u8],
    maximum: usize,
    kind: &'static str,
) -> Result<(), JobRelayStreamError> {
    if payload.is_empty() || payload.len() > maximum {
        Err(JobRelayStreamError::InvalidPayloadSize {
            kind,
            actual: payload.len(),
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_reason(reason: &str) -> Result<(), JobRelayStreamError> {
    if reason.is_empty() || reason.len() > JOB_RELAY_STREAM_MAX_REASON_BYTES {
        Err(JobRelayStreamError::InvalidReason)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Blob stream — workload-neutral, resumable content transfer.
// ---------------------------------------------------------------------------

/// Generic peer content stream. Phase-net treats content IDs and metadata as
/// opaque bytes and never opens files or interprets model formats.
pub const BLOB_STREAM_PROTOCOL: &str = "/phase/blob/1.0.0";

pub const BLOB_STREAM_SCHEMA_VERSION: u16 = 1;
pub const BLOB_STREAM_MAX_CHUNK_BYTES: usize = 64 * 1024;
pub const BLOB_STREAM_MAX_METADATA_BYTES: usize = 4 * 1024;
pub const BLOB_STREAM_MAX_REASON_BYTES: usize = 1024;
pub const BLOB_STREAM_MIN_IDLE_TIMEOUT_MS: u64 = 250;
pub const BLOB_STREAM_MAX_IDLE_TIMEOUT_MS: u64 = 60_000;
pub const BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS: u64 = 30_000;
pub const BLOB_STREAM_MAX_DEADLINE_AHEAD_MS: u64 = 60 * 60 * 1000;

/// First and only requester-to-server message on a blob substream.
///
/// `[u8; 32]` makes the content identifier fixed-width at the type and wire
/// boundaries. `metadata` is optional, opaque routing/authorization context
/// for the workload layer and is never interpreted by phase-net.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobStreamRequest {
    pub schema_version: u16,
    pub content_id: [u8; 32],
    pub offset: u64,
    pub deadline_unix_ms: u64,
    pub idle_timeout_ms: u64,
    #[serde(with = "serde_bytes")]
    pub metadata: Vec<u8>,
}

impl BlobStreamRequest {
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), BlobStreamError> {
        validate_blob_schema(self.schema_version)?;
        if self.metadata.len() > BLOB_STREAM_MAX_METADATA_BYTES {
            return Err(BlobStreamError::MetadataTooLarge {
                actual: self.metadata.len(),
                maximum: BLOB_STREAM_MAX_METADATA_BYTES,
            });
        }
        if !(BLOB_STREAM_MIN_IDLE_TIMEOUT_MS..=BLOB_STREAM_MAX_IDLE_TIMEOUT_MS)
            .contains(&self.idle_timeout_ms)
        {
            return Err(BlobStreamError::InvalidIdleTimeout {
                actual_ms: self.idle_timeout_ms,
                minimum_ms: BLOB_STREAM_MIN_IDLE_TIMEOUT_MS,
                maximum_ms: BLOB_STREAM_MAX_IDLE_TIMEOUT_MS,
            });
        }
        let deadline_ahead = self
            .deadline_unix_ms
            .checked_sub(now_unix_ms)
            .ok_or(BlobStreamError::ExpiredDeadline)?;
        if deadline_ahead == 0 {
            return Err(BlobStreamError::ExpiredDeadline);
        }
        if deadline_ahead > BLOB_STREAM_MAX_DEADLINE_AHEAD_MS {
            return Err(BlobStreamError::DeadlineTooFar {
                maximum_ahead_ms: BLOB_STREAM_MAX_DEADLINE_AHEAD_MS,
            });
        }
        Ok(())
    }
}

/// One server-to-requester blob frame. The accepted header binds the exact
/// total size and resume offset before any content is delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobStreamFrame {
    pub schema_version: u16,
    pub content_id: [u8; 32],
    pub kind: BlobStreamFrameKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlobStreamFrameKind {
    Accepted {
        total_size: u64,
        offset: u64,
    },
    Rejected {
        reason: String,
    },
    Chunk {
        offset: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    Eof {
        offset: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobStreamState {
    AwaitingDecision,
    Streaming { total_size: u64 },
    Complete,
}

/// Fail-closed validator shared by both blob-stream endpoints.
///
/// Chunks must begin exactly at the current cursor and may not cross the total
/// size declared by `Accepted`. A transfer is complete only after `Eof` at the
/// declared total size or an explicit sequence-zero rejection.
#[derive(Debug, Clone)]
pub struct BlobStreamValidator {
    content_id: [u8; 32],
    requested_offset: u64,
    cursor: u64,
    state: BlobStreamState,
}

impl BlobStreamValidator {
    pub fn new(content_id: [u8; 32], requested_offset: u64) -> Self {
        Self {
            content_id,
            requested_offset,
            cursor: requested_offset,
            state: BlobStreamState::AwaitingDecision,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.state == BlobStreamState::Complete
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn validate(&mut self, frame: &BlobStreamFrame) -> Result<(), BlobStreamError> {
        validate_blob_schema(frame.schema_version)?;
        if frame.content_id != self.content_id {
            return Err(BlobStreamError::WrongContent);
        }

        match (self.state, &frame.kind) {
            (
                BlobStreamState::AwaitingDecision,
                BlobStreamFrameKind::Accepted { total_size, offset },
            ) => {
                if *offset != self.requested_offset {
                    return Err(BlobStreamError::OffsetMismatch {
                        expected: self.requested_offset,
                        actual: *offset,
                    });
                }
                if *total_size < *offset {
                    return Err(BlobStreamError::OffsetBeyondTotal {
                        offset: *offset,
                        total_size: *total_size,
                    });
                }
                self.state = BlobStreamState::Streaming {
                    total_size: *total_size,
                };
            }
            (BlobStreamState::AwaitingDecision, BlobStreamFrameKind::Rejected { reason }) => {
                validate_blob_reason(reason)?;
                self.state = BlobStreamState::Complete;
            }
            (
                BlobStreamState::Streaming { total_size },
                BlobStreamFrameKind::Chunk { offset, bytes },
            ) => {
                if *offset != self.cursor {
                    return Err(BlobStreamError::OffsetMismatch {
                        expected: self.cursor,
                        actual: *offset,
                    });
                }
                if bytes.is_empty() || bytes.len() > BLOB_STREAM_MAX_CHUNK_BYTES {
                    return Err(BlobStreamError::InvalidChunkSize {
                        actual: bytes.len(),
                        maximum: BLOB_STREAM_MAX_CHUNK_BYTES,
                    });
                }
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or(BlobStreamError::OffsetOverflow)?;
                if end > total_size {
                    return Err(BlobStreamError::ChunkBeyondTotal { end, total_size });
                }
                self.cursor = end;
            }
            (BlobStreamState::Streaming { total_size }, BlobStreamFrameKind::Eof { offset }) => {
                if *offset != self.cursor {
                    return Err(BlobStreamError::OffsetMismatch {
                        expected: self.cursor,
                        actual: *offset,
                    });
                }
                if *offset != total_size {
                    return Err(BlobStreamError::PrematureEof {
                        offset: *offset,
                        total_size,
                    });
                }
                self.state = BlobStreamState::Complete;
            }
            (state, kind) => {
                return Err(BlobStreamError::InvalidTransition {
                    state: state.as_str(),
                    frame: kind.name(),
                });
            }
        }
        Ok(())
    }

    pub fn validate_eof(&self) -> Result<(), BlobStreamError> {
        if self.is_complete() {
            Ok(())
        } else {
            Err(BlobStreamError::UnexpectedEof)
        }
    }
}

impl BlobStreamState {
    fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingDecision => "awaiting_decision",
            Self::Streaming { .. } => "streaming",
            Self::Complete => "complete",
        }
    }
}

impl BlobStreamFrameKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::Chunk { .. } => "chunk",
            Self::Eof { .. } => "eof",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BlobStreamError {
    #[error("unsupported blob stream schema {actual}; expected {expected}")]
    UnsupportedSchema { actual: u16, expected: u16 },
    #[error("blob stream frame belongs to a different content ID")]
    WrongContent,
    #[error("expected blob offset {expected}, got {actual}")]
    OffsetMismatch { expected: u64, actual: u64 },
    #[error("requested offset {offset} exceeds total size {total_size}")]
    OffsetBeyondTotal { offset: u64, total_size: u64 },
    #[error("blob chunk end {end} exceeds total size {total_size}")]
    ChunkBeyondTotal { end: u64, total_size: u64 },
    #[error("blob offset overflow")]
    OffsetOverflow,
    #[error("blob chunk must contain 1..={maximum} bytes (got {actual})")]
    InvalidChunkSize { actual: usize, maximum: usize },
    #[error("blob EOF at {offset} does not match total size {total_size}")]
    PrematureEof { offset: u64, total_size: u64 },
    #[error("invalid blob stream transition from {state} using {frame}")]
    InvalidTransition {
        state: &'static str,
        frame: &'static str,
    },
    #[error("blob request metadata exceeds {maximum} bytes (got {actual})")]
    MetadataTooLarge { actual: usize, maximum: usize },
    #[error("blob idle timeout {actual_ms}ms is outside {minimum_ms}..={maximum_ms}ms")]
    InvalidIdleTimeout {
        actual_ms: u64,
        minimum_ms: u64,
        maximum_ms: u64,
    },
    #[error("blob request deadline has already expired")]
    ExpiredDeadline,
    #[error("blob request deadline exceeds {maximum_ahead_ms}ms maximum horizon")]
    DeadlineTooFar { maximum_ahead_ms: u64 },
    #[error("blob rejection reason is empty or exceeds its size limit")]
    InvalidReason,
    #[error("blob stream ended before an explicit EOF or rejection")]
    UnexpectedEof,
}

fn validate_blob_schema(schema_version: u16) -> Result<(), BlobStreamError> {
    if schema_version == BLOB_STREAM_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(BlobStreamError::UnsupportedSchema {
            actual: schema_version,
            expected: BLOB_STREAM_SCHEMA_VERSION,
        })
    }
}

fn validate_blob_reason(reason: &str) -> Result<(), BlobStreamError> {
    if reason.is_empty() || reason.len() > BLOB_STREAM_MAX_REASON_BYTES {
        Err(BlobStreamError::InvalidReason)
    } else {
        Ok(())
    }
}

/// Job offer from client to node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOffer {
    /// Unique job ID
    pub job_id: String,

    /// Nonce for replay protection
    pub nonce: String,

    /// SHA-256 hash of WASM module
    pub module_hash: String,

    /// Resource requirements
    pub requirements: JobRequirements,
}

/// Resource requirements for a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequirements {
    /// CPU cores required
    pub cpu_cores: u32,

    /// Memory required (MB)
    pub memory_mb: u64,

    /// Timeout (seconds)
    pub timeout_seconds: u64,

    /// Required architecture (e.g., "x86_64", "aarch64")
    pub arch: String,

    /// Required WASM runtime
    pub wasm_runtime: String,
}

/// Response to job offer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobResponse {
    /// Job accepted - node will execute it
    Accepted {
        /// Job ID (matching offer)
        job_id: String,

        /// Estimated start time (unix timestamp)
        estimated_start: u64,

        /// Node's peer ID
        node_peer_id: String,
    },

    /// Job rejected - node cannot execute it
    Rejected {
        /// Job ID (matching offer)
        job_id: String,

        /// Rejection reason
        reason: RejectionReason,
    },
}

/// Reasons for rejecting a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Resource requirements exceed node capacity
    InsufficientResources { missing: String },

    /// Architecture mismatch
    ArchMismatch { required: String, available: String },

    /// Runtime not supported
    RuntimeNotSupported { required: String },

    /// Node is at capacity
    QueueFull,

    /// Malformed request
    InvalidRequest { details: String },
}

/// Complete job request with WASM payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    /// Unique job ID
    pub job_id: String,

    /// SHA-256 hash of WASM module
    pub module_hash: String,

    /// WASM module bytes
    #[serde(with = "serde_bytes_base64")]
    pub wasm_bytes: Vec<u8>,

    /// Arguments to pass to WASM
    pub args: Vec<String>,

    /// Resource requirements
    pub requirements: JobRequirements,
}

/// Job execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    /// Job ID (matching request)
    pub job_id: String,

    /// Standard output
    pub stdout: String,

    /// Standard error
    pub stderr: String,

    /// Exit code (0 = success)
    pub exit_code: u32,

    /// Execution receipt (JSON)
    pub receipt_json: String,
}

/// Helper module for base64 encoding of bytes (for JSON compatibility)
mod serde_bytes_base64 {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

impl JobRequest {
    /// Create a new job request
    pub fn new(
        job_id: String,
        module_hash: String,
        wasm_bytes: Vec<u8>,
        args: Vec<String>,
        requirements: JobRequirements,
    ) -> Self {
        Self {
            job_id,
            module_hash,
            wasm_bytes,
            args,
            requirements,
        }
    }

    /// Validate the job request
    pub fn validate(&self) -> Result<(), String> {
        if self.job_id.is_empty() {
            return Err("job_id cannot be empty".to_string());
        }
        if self.wasm_bytes.is_empty() {
            return Err("wasm_bytes cannot be empty".to_string());
        }
        if self.requirements.cpu_cores < 1 {
            return Err("cpu_cores must be at least 1".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM_JOB: [u8; 32] = [7; 32];
    const STREAM_CONTENT: [u8; 32] = [11; 32];

    fn stream_frame(sequence: u64, kind: JobRelayStreamFrameKind) -> JobRelayStreamFrame {
        JobRelayStreamFrame {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: STREAM_JOB,
            sequence,
            kind,
        }
    }

    fn blob_frame(kind: BlobStreamFrameKind) -> BlobStreamFrame {
        BlobStreamFrame {
            schema_version: BLOB_STREAM_SCHEMA_VERSION,
            content_id: STREAM_CONTENT,
            kind,
        }
    }

    #[test]
    fn live_relay_accepts_only_complete_ordered_lifecycle() {
        let mut validator = JobRelayStreamValidator::new(STREAM_JOB);
        validator
            .validate(&stream_frame(0, JobRelayStreamFrameKind::Accepted))
            .unwrap();
        validator
            .validate(&stream_frame(
                1,
                JobRelayStreamFrameKind::Event {
                    payload: vec![1],
                    terminal: false,
                },
            ))
            .unwrap();
        validator
            .validate(&stream_frame(
                2,
                JobRelayStreamFrameKind::Event {
                    payload: vec![2],
                    terminal: true,
                },
            ))
            .unwrap();
        assert_eq!(
            validator.validate_eof(),
            Err(JobRelayStreamError::UnexpectedEof)
        );
        validator
            .validate(&stream_frame(
                3,
                JobRelayStreamFrameKind::Receipt { payload: vec![3] },
            ))
            .unwrap();
        assert!(validator.is_complete());
        validator.validate_eof().unwrap();
    }

    #[test]
    fn live_relay_rejects_cross_job_duplicate_gapped_and_reversed_frames() {
        let mut wrong_job = stream_frame(0, JobRelayStreamFrameKind::Accepted);
        wrong_job.job_id = [8; 32];
        assert_eq!(
            JobRelayStreamValidator::new(STREAM_JOB).validate(&wrong_job),
            Err(JobRelayStreamError::WrongJob)
        );

        for actual in [1, 2, u64::MAX] {
            let err = JobRelayStreamValidator::new(STREAM_JOB)
                .validate(&stream_frame(actual, JobRelayStreamFrameKind::Accepted))
                .unwrap_err();
            assert_eq!(
                err,
                JobRelayStreamError::UnexpectedSequence {
                    expected: 0,
                    actual,
                }
            );
        }

        let mut duplicate = JobRelayStreamValidator::new(STREAM_JOB);
        duplicate
            .validate(&stream_frame(0, JobRelayStreamFrameKind::Accepted))
            .unwrap();
        assert!(matches!(
            duplicate.validate(&stream_frame(0, JobRelayStreamFrameKind::Accepted)),
            Err(JobRelayStreamError::UnexpectedSequence { .. })
        ));
    }

    #[test]
    fn live_relay_rejects_invalid_transitions_and_post_terminal_output() {
        let mut validator = JobRelayStreamValidator::new(STREAM_JOB);
        assert!(matches!(
            validator.validate(&stream_frame(
                0,
                JobRelayStreamFrameKind::Event {
                    payload: vec![1],
                    terminal: false,
                },
            )),
            Err(JobRelayStreamError::InvalidTransition { .. })
        ));

        validator
            .validate(&stream_frame(0, JobRelayStreamFrameKind::Accepted))
            .unwrap();
        validator
            .validate(&stream_frame(
                1,
                JobRelayStreamFrameKind::Event {
                    payload: vec![1],
                    terminal: true,
                },
            ))
            .unwrap();
        assert!(matches!(
            validator.validate(&stream_frame(
                2,
                JobRelayStreamFrameKind::Event {
                    payload: vec![2],
                    terminal: false,
                },
            )),
            Err(JobRelayStreamError::InvalidTransition { .. })
        ));
        validator
            .validate(&stream_frame(
                2,
                JobRelayStreamFrameKind::Receipt { payload: vec![3] },
            ))
            .unwrap();
        assert!(matches!(
            validator.validate(&stream_frame(
                3,
                JobRelayStreamFrameKind::Event {
                    payload: vec![4],
                    terminal: false,
                },
            )),
            Err(JobRelayStreamError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn live_relay_rejects_unknown_schema_empty_and_oversized_payloads() {
        let mut unknown = stream_frame(0, JobRelayStreamFrameKind::Accepted);
        unknown.schema_version = 0;
        assert!(matches!(
            JobRelayStreamValidator::new(STREAM_JOB).validate(&unknown),
            Err(JobRelayStreamError::UnsupportedSchema { .. })
        ));

        for payload in [Vec::new(), vec![0; JOB_RELAY_STREAM_MAX_EVENT_BYTES + 1]] {
            let mut validator = JobRelayStreamValidator::new(STREAM_JOB);
            validator
                .validate(&stream_frame(0, JobRelayStreamFrameKind::Accepted))
                .unwrap();
            assert!(matches!(
                validator.validate(&stream_frame(
                    1,
                    JobRelayStreamFrameKind::Event {
                        payload,
                        terminal: false,
                    },
                )),
                Err(JobRelayStreamError::InvalidPayloadSize { .. })
            ));
        }
    }

    #[test]
    fn live_relay_rejection_failure_control_and_open_are_bounded() {
        let mut rejected = JobRelayStreamValidator::new(STREAM_JOB);
        rejected
            .validate(&stream_frame(
                0,
                JobRelayStreamFrameKind::Rejected {
                    reason: "policy denied".into(),
                },
            ))
            .unwrap();
        rejected.validate_eof().unwrap();

        let open = JobRelayStreamOpen {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: STREAM_JOB,
            payload: vec![1],
            deadline_unix_ms: 101,
            idle_timeout_ms: JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS,
        };
        open.validate(100).unwrap();
        assert_eq!(
            open.validate(101),
            Err(JobRelayStreamError::ExpiredDeadline)
        );

        let mut invalid_idle = open.clone();
        invalid_idle.deadline_unix_ms = 102;
        invalid_idle.idle_timeout_ms = JOB_RELAY_STREAM_MIN_IDLE_TIMEOUT_MS - 1;
        assert!(matches!(
            invalid_idle.validate(101),
            Err(JobRelayStreamError::InvalidIdleTimeout { .. })
        ));

        let mut too_far = open.clone();
        too_far.deadline_unix_ms = 101 + JOB_RELAY_STREAM_MAX_DEADLINE_AHEAD_MS + 1;
        assert!(matches!(
            too_far.validate(101),
            Err(JobRelayStreamError::DeadlineTooFar { .. })
        ));

        let cancel = JobRelayStreamControl {
            schema_version: JOB_RELAY_STREAM_SCHEMA_VERSION,
            job_id: STREAM_JOB,
            kind: JobRelayStreamControlKind::Cancel {
                reason: "client disconnected".into(),
            },
        };
        cancel.validate_for(STREAM_JOB).unwrap();
        assert_eq!(
            cancel.validate_for([9; 32]),
            Err(JobRelayStreamError::WrongJob)
        );
    }

    #[test]
    fn blob_stream_accepts_exact_resumed_order_and_explicit_eof() {
        let mut validator = BlobStreamValidator::new(STREAM_CONTENT, 3);
        validator
            .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 3,
            }))
            .unwrap();
        validator
            .validate(&blob_frame(BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: vec![3, 4, 5],
            }))
            .unwrap();
        assert_eq!(validator.cursor(), 6);
        validator
            .validate(&blob_frame(BlobStreamFrameKind::Chunk {
                offset: 6,
                bytes: vec![6, 7, 8, 9],
            }))
            .unwrap();
        assert_eq!(
            validator.validate_eof(),
            Err(BlobStreamError::UnexpectedEof)
        );
        validator
            .validate(&blob_frame(BlobStreamFrameKind::Eof { offset: 10 }))
            .unwrap();
        assert!(validator.is_complete());
        validator.validate_eof().unwrap();
    }

    #[test]
    fn blob_stream_rejects_wrong_content_offsets_and_transitions() {
        let mut wrong_content = blob_frame(BlobStreamFrameKind::Rejected {
            reason: "not found".into(),
        });
        wrong_content.content_id = [12; 32];
        assert_eq!(
            BlobStreamValidator::new(STREAM_CONTENT, 0).validate(&wrong_content),
            Err(BlobStreamError::WrongContent)
        );

        let wrong_resume = BlobStreamValidator::new(STREAM_CONTENT, 5)
            .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 4,
            }))
            .unwrap_err();
        assert_eq!(
            wrong_resume,
            BlobStreamError::OffsetMismatch {
                expected: 5,
                actual: 4,
            }
        );

        let beyond_total = BlobStreamValidator::new(STREAM_CONTENT, 11)
            .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 11,
            }))
            .unwrap_err();
        assert_eq!(
            beyond_total,
            BlobStreamError::OffsetBeyondTotal {
                offset: 11,
                total_size: 10,
            }
        );

        let mut ordered = BlobStreamValidator::new(STREAM_CONTENT, 0);
        ordered
            .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                total_size: 4,
                offset: 0,
            }))
            .unwrap();
        assert_eq!(
            ordered
                .validate(&blob_frame(BlobStreamFrameKind::Chunk {
                    offset: 1,
                    bytes: vec![1],
                }))
                .unwrap_err(),
            BlobStreamError::OffsetMismatch {
                expected: 0,
                actual: 1,
            }
        );
        assert_eq!(
            ordered
                .validate(&blob_frame(BlobStreamFrameKind::Eof { offset: 0 }))
                .unwrap_err(),
            BlobStreamError::PrematureEof {
                offset: 0,
                total_size: 4,
            }
        );

        let mut rejected = BlobStreamValidator::new(STREAM_CONTENT, 99);
        rejected
            .validate(&blob_frame(BlobStreamFrameKind::Rejected {
                reason: "offset outside content".into(),
            }))
            .unwrap();
        rejected.validate_eof().unwrap();
        assert!(matches!(
            rejected.validate(&blob_frame(BlobStreamFrameKind::Eof { offset: 99 })),
            Err(BlobStreamError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn blob_stream_bounds_metadata_deadlines_idle_and_malformed_chunks() {
        let request = BlobStreamRequest {
            schema_version: BLOB_STREAM_SCHEMA_VERSION,
            content_id: STREAM_CONTENT,
            offset: 0,
            deadline_unix_ms: 1_001,
            idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
            metadata: vec![],
        };
        request.validate(1_000).unwrap();

        let mut invalid = request.clone();
        invalid.metadata = vec![0; BLOB_STREAM_MAX_METADATA_BYTES + 1];
        assert!(matches!(
            invalid.validate(1_000),
            Err(BlobStreamError::MetadataTooLarge { .. })
        ));
        invalid = request.clone();
        invalid.deadline_unix_ms = 1_000;
        assert_eq!(
            invalid.validate(1_000),
            Err(BlobStreamError::ExpiredDeadline)
        );
        invalid = request.clone();
        invalid.deadline_unix_ms = 1_000 + BLOB_STREAM_MAX_DEADLINE_AHEAD_MS + 1;
        assert!(matches!(
            invalid.validate(1_000),
            Err(BlobStreamError::DeadlineTooFar { .. })
        ));
        for idle_timeout_ms in [
            BLOB_STREAM_MIN_IDLE_TIMEOUT_MS - 1,
            BLOB_STREAM_MAX_IDLE_TIMEOUT_MS + 1,
        ] {
            invalid = request.clone();
            invalid.idle_timeout_ms = idle_timeout_ms;
            assert!(matches!(
                invalid.validate(1_000),
                Err(BlobStreamError::InvalidIdleTimeout { .. })
            ));
        }

        for bytes in [Vec::new(), vec![0; BLOB_STREAM_MAX_CHUNK_BYTES + 1]] {
            let mut validator = BlobStreamValidator::new(STREAM_CONTENT, 0);
            validator
                .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                    total_size: (BLOB_STREAM_MAX_CHUNK_BYTES + 1) as u64,
                    offset: 0,
                }))
                .unwrap();
            assert!(matches!(
                validator.validate(&blob_frame(BlobStreamFrameKind::Chunk { offset: 0, bytes })),
                Err(BlobStreamError::InvalidChunkSize { .. })
            ));
        }

        let mut overrun = BlobStreamValidator::new(STREAM_CONTENT, 0);
        overrun
            .validate(&blob_frame(BlobStreamFrameKind::Accepted {
                total_size: 1,
                offset: 0,
            }))
            .unwrap();
        assert_eq!(
            overrun
                .validate(&blob_frame(BlobStreamFrameKind::Chunk {
                    offset: 0,
                    bytes: vec![1, 2],
                }))
                .unwrap_err(),
            BlobStreamError::ChunkBeyondTotal {
                end: 2,
                total_size: 1,
            }
        );
    }

    #[test]
    fn test_job_offer_serialization() {
        let offer = JobOffer {
            job_id: "test-job-123".to_string(),
            nonce: "nonce-abc".to_string(),
            module_hash: "sha256:abc123".to_string(),
            requirements: JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        };

        let json = serde_json::to_string(&offer).unwrap();
        let deserialized: JobOffer = serde_json::from_str(&json).unwrap();

        assert_eq!(offer.job_id, deserialized.job_id);
        assert_eq!(offer.module_hash, deserialized.module_hash);
    }

    #[test]
    fn test_job_response_accepted() {
        let response = JobResponse::Accepted {
            job_id: "test-job-123".to_string(),
            estimated_start: 1699564800,
            node_peer_id: "12D3KooW...".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Accepted"));
    }

    #[test]
    fn test_job_response_rejected() {
        let response = JobResponse::Rejected {
            job_id: "test-job-123".to_string(),
            reason: RejectionReason::InsufficientResources {
                missing: "memory".to_string(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Rejected"));
    }

    #[test]
    fn test_job_request_serialization() {
        let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d]; // WASM magic bytes
        let request = JobRequest::new(
            "job-456".to_string(),
            "sha256:def456".to_string(),
            wasm_bytes.clone(),
            vec!["arg1".to_string(), "arg2".to_string()],
            JobRequirements {
                cpu_cores: 2,
                memory_mb: 256,
                timeout_seconds: 60,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: JobRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(request.job_id, deserialized.job_id);
        assert_eq!(request.module_hash, deserialized.module_hash);
        assert_eq!(request.wasm_bytes, deserialized.wasm_bytes);
        assert_eq!(request.args, deserialized.args);
    }

    #[test]
    fn test_job_request_validation() {
        let valid = JobRequest::new(
            "job-789".to_string(),
            "sha256:abc".to_string(),
            vec![0x00, 0x61, 0x73, 0x6d],
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );
        assert!(valid.validate().is_ok());

        let empty_id = JobRequest::new(
            "".to_string(),
            "sha256:abc".to_string(),
            vec![0x00],
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );
        assert!(empty_id.validate().is_err());
    }

    #[test]
    fn test_job_result_serialization() {
        let result = JobResult {
            job_id: "job-result-1".to_string(),
            stdout: "Hello, world!".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
            receipt_json: r#"{"version":"0.1","module_hash":"sha256:abc"}"#.to_string(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: JobResult = serde_json::from_str(&json).unwrap();

        assert_eq!(result.job_id, deserialized.job_id);
        assert_eq!(result.stdout, deserialized.stdout);
        assert_eq!(result.exit_code, deserialized.exit_code);
    }
}
