// SPDX-License-Identifier: Apache-2.0

//! Job specifications — the workload payload carried inside a signed manifest.
//!
//! [`JobSpec`] is the discriminated union of every workload type Phase can
//! carry. Plasm cares about [`JobSpec::Wasm`]; LUCID cares about
//! [`JobSpec::Inference`]; future workers will add new variants without
//! disturbing the trait surface in [`crate::worker`].
//!
//! ## Compatibility contract
//!
//! - `JobSpec` is `#[non_exhaustive]`. Adding a new variant is a minor-version
//!   change for downstreams that only construct `JobSpec` values they
//!   recognize; downstream matches must always include a `_` arm.
//! - Workers advertise the kinds they handle via
//!   [`crate::Worker::supported_kinds`] and may reject a job with
//!   [`crate::WorkerError::Unsupported`] if asked to run one they don't.
//! - Field additions inside an individual `*JobSpec` struct are minor as long
//!   as new fields are `Option<_>` or `#[serde(default)]`.
//!
//! ## Why an enum, not a trait object?
//!
//! A `Box<dyn Job>` would force every JobSpec to share a runtime vtable but
//! would also defeat the point of `SignedManifest<JobSpec>` — the wire format
//! has to be exhaustively deserializable from any honest peer's bytes, which
//! demands a closed (in the cryptographic sense — what bytes count as a valid
//! manifest) set of recognised variants per protocol version. The enum makes
//! that set explicit and versionable; new variants are a protocol-level event,
//! not a runtime plugin.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A workload payload carried inside a [`crate::SignedManifest`].
///
/// Marked `#[non_exhaustive]` so downstream crates that match on `JobSpec` are
/// required to keep a wildcard arm — protecting them when new variants land.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobSpec {
    /// A WebAssembly module + arguments — Plasm's domain.
    Wasm(WasmJobSpec),
    /// A model inference request — LUCID's domain.
    Inference(InferenceJobSpec),
    // Future variants slot in here. Examples that have been considered:
    //   ImageGen(ImageGenJobSpec),
    //   Embedding(EmbeddingJobSpec),
    //   FineTune(FineTuneJobSpec),
    //   Render(RenderJobSpec),
    //   Science(ScienceJobSpec),
}

impl JobSpec {
    /// Returns the lightweight discriminator a worker uses to decide whether
    /// it can serve this job *before* deserializing the full payload.
    pub fn kind(&self) -> JobSpecKind {
        match self {
            JobSpec::Wasm(_) => JobSpecKind::Wasm,
            JobSpec::Inference(_) => JobSpecKind::Inference,
        }
    }
}

/// A lightweight discriminator for a [`JobSpec`] — what a worker advertises
/// it can serve, and what a scheduler matches against before paying the cost
/// of deserialising the full payload.
///
/// `Copy + Eq + Hash` so it can sit in `&[JobSpecKind]` capability lists and
/// `HashMap<JobSpecKind, _>` dispatch tables without ceremony.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobSpecKind {
    Wasm,
    Inference,
}

// ---------------------------------------------------------------------------
// WasmJobSpec
// ---------------------------------------------------------------------------

/// Plasm's WASM workload. Mirrors the existing `daemon/src/wasm/` job shape
/// the November 2025 MVP already wire-formats and signs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmJobSpec {
    /// Content-address of the `.wasm` module — resolved against
    /// `phase-artifact-server`.
    pub module_cid: String,

    /// Stdin / argv-style input handed to the module.
    #[serde(with = "serde_bytes")]
    pub input: Vec<u8>,

    /// Wall-clock cap. The worker enforces this; jobs that overrun terminate
    /// with [`crate::WorkerError::DeadlineExceeded`].
    #[serde(default)]
    pub max_duration_ms: Option<u64>,

    /// Memory cap in bytes (mapped to wasmtime's `Store` limits).
    #[serde(default)]
    pub max_memory_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// InferenceJobSpec
// ---------------------------------------------------------------------------

/// LUCID's inference workload. Generic enough to map onto llama.cpp, MLX,
/// or a future remote backend without leaking backend-specific knobs into
/// the protocol layer.
///
/// The Ollama-compat HTTP surface in `lucidd::api` translates incoming
/// `/api/chat` / `/api/generate` requests into this spec; the worker
/// translates back when emitting `OutputChunk`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceJobSpec {
    /// Content-address (or human alias resolved to one) of the model weights.
    pub model_cid: String,

    /// Conversation messages. Empty `messages` + non-empty `prompt` selects
    /// the `/api/generate`-style path; non-empty `messages` selects `/api/chat`.
    #[serde(default)]
    pub messages: Vec<ChatMessage>,

    /// Single-turn completion prompt. Mutually exclusive with `messages`
    /// in practice; workers MAY reject specs with both populated.
    #[serde(default)]
    pub prompt: Option<String>,

    /// Resumption handle from a previous call's [`crate::JobEvent::Final`].
    /// Workers SHOULD attempt cache reuse against this; if the underlying
    /// state is gone, they MUST fall back to a cold prefill rather than
    /// failing the request.
    ///
    /// See the KV-cache contract in `SPEC.md`.
    #[serde(default)]
    pub resume_from: Option<ConversationToken>,

    /// Sampling parameters. Backend-specific knobs that the worker passes
    /// through; unrecognised keys are ignored. Keeping this open-ended is
    /// what lets `top_p` / `min_p` / `temperature` / `repetition_penalty`
    /// / `seed` / etc. land without protocol churn.
    #[serde(default)]
    pub sampling: SamplingParams,

    /// Maximum tokens to generate. `None` = backend default.
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// If true, the worker SHOULD stream `OutputChunk`s as tokens are
    /// produced. If false, the worker MAY still stream internally but
    /// MUST emit only the terminal [`crate::JobEvent::Final`].
    #[serde(default = "default_true")]
    pub stream: bool,
}

fn default_true() -> bool {
    true
}

/// A chat-style message. Mirrors the Ollama `/api/chat` request shape so the
/// edge translator in `lucidd::api::ollama` is a straight field copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Base64-encoded images (vision models). The protocol layer doesn't
    /// inspect these; it's the worker's job to know whether its model
    /// supports them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Open-ended sampling knobs. Workers extract what they understand and
/// silently ignore the rest. Concrete keys consumers may set today:
/// `temperature`, `top_p`, `top_k`, `min_p`, `repetition_penalty`, `seed`,
/// `stop` (as a JSON-encoded array).
///
/// Values are JSON-encoded strings so this struct is portable across the
/// protocol without dragging `serde_json::Value` into the wire schema —
/// workers parse what they understand and ignore the rest. The cost is a
/// double-encoding for numeric params (e.g. `"0.7"` instead of `0.7`),
/// which is negligible vs the upside of not coupling the protocol crate
/// to a specific dynamic-JSON representation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SamplingParams {
    #[serde(flatten)]
    pub params: BTreeMap<String, String>,
}

/// Opaque KV-cache resumption handle. Bytes are interpreted only by the
/// worker that issued them — typically `(slot_id || prefix_hash || nonce)`
/// for llama.cpp, or an MLX session ID, or a remote-backend cursor.
///
/// Conversations route to the same worker when possible (see SPEC.md §
/// "Resumption affinity"); when not possible the worker silently cold-starts
/// and the next emitted token is just slower, never wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationToken {
    /// The worker peer that issued this token. Schedulers SHOULD route the
    /// follow-up turn back to the same peer for cache reuse.
    pub issuer: PeerId,

    /// Worker-defined opaque bytes. MUST be small enough to fit in a
    /// libp2p control message (target: <= 256 bytes).
    #[serde(with = "serde_bytes_field")]
    pub state: Bytes,

    /// Wall-clock the worker promised to retain this state until.
    /// Workers MAY evict earlier under memory pressure (see SPEC.md).
    pub valid_until_unix_ms: u64,
}

/// Placeholder for `phase-identity::PeerId`. When that crate lands and the
/// `real-envelopes` feature flips on, this becomes a re-export.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

// Local serde adapter — `bytes::Bytes` round-trips through `serde_bytes`
// but inside a struct field we need a tiny module wrapper.
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

// ---------------------------------------------------------------------------
// JobResult
// ---------------------------------------------------------------------------

/// The terminal record for a completed job. Wrapped in
/// [`crate::SignedReceipt`] for accounting and verification.
///
/// Why a single `JobResult` type for every workload? Because all the
/// workload-specific bits are already in the [`crate::OutputChunk`] stream
/// that produced them, and re-encoding them here would either duplicate
/// (bloat) or summarise (lossy). `JobResult` instead carries the *commitment*
/// over the stream and the metadata a verifier needs to replay it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    /// Hash of the [`JobSpec`] this result corresponds to. Lets a verifier
    /// link receipt → manifest without having to carry the full spec twice.
    pub job_spec_hash: [u8; 32],

    /// Final value of the streaming commitment accumulator (SHA-256 chain
    /// over `OutputChunk`s, in order). The signed receipt's signature is
    /// over a message containing this value; reconstructing the accumulator
    /// from the on-the-wire chunk stream lets a verifier confirm the
    /// signature without trusting any intermediate node. See SPEC.md.
    pub output_commitment: [u8; 32],

    /// Number of `OutputChunk`s emitted. Combined with `output_commitment`,
    /// this lets a verifier detect truncation attacks (an attacker dropping
    /// the last N chunks would change the commitment AND the count).
    pub output_chunk_count: u64,

    /// Whether the run completed normally, was cancelled, or errored.
    pub completion: Completion,

    /// Optional resumption token for KV-cache reuse on the next turn.
    /// Distinct from the `Final` event's token (this is the persisted
    /// version in the signed receipt; the live one is in the event).
    #[serde(default)]
    pub resumption: Option<ConversationToken>,

    /// Worker-attested metrics — populated for observability, not for
    /// verification. Verifiers MUST NOT trust these for correctness; they're
    /// here so reputation systems and the Ollama API can surface
    /// `total_duration` / `eval_count` / etc.
    #[serde(default)]
    pub metrics: JobMetrics,
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    /// Job finished naturally — model emitted EOS, WASM returned, etc.
    Stop,
    /// Hit a generation cap (max_tokens, max_duration_ms, etc.).
    Length,
    /// Client cancelled mid-stream. The accumulator and chunk count still
    /// cover everything that was emitted before cancellation, so partial
    /// results are signed and verifiable.
    Cancelled,
    /// Worker-side error. The error message is in the receipt's signed
    /// envelope alongside this completion code.
    Error,
}

/// Best-effort metrics. Not load-bearing for verification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobMetrics {
    #[serde(default)]
    pub total_duration_ms: u64,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    /// Free-form backend telemetry — e.g. `gpu_layers`, `slot_id`,
    /// `model_revision`. Surfaces in observability, never affects signing.
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}
