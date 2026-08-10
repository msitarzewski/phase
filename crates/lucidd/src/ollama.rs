// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ollama-compatible HTTP surface for the supported LUCID API contract.
//!
//! Implements just enough of the Ollama native API for a real client (the
//! `ollama` CLI, `curl`, Open WebUI) to stream tokens off our worker:
//!
//! - `POST /api/chat` — full NDJSON streaming, the load-bearing path.
//! - `GET /api/tags` — list the registry's verified local models.
//! - `GET /api/version` — clients capability-sniff here on startup.
//! - `POST /api/show` — registry-backed metadata for a requested alias/CID.
//! - `GET /api/ps` — an honest empty list until worker residency is tracked.
//! - `POST /api/embed` / `POST /api/embeddings` — embedding vectors over the
//!   same router/manifest/receipt pipeline as `/api/chat` (the legacy
//!   `/api/embeddings` is the singular-`prompt` shape Ollama shipped first).
//! - `POST /api/pull` — imports a local GGUF when present, otherwise resolves
//!   signed alias/provider records and performs a resumable verified pull.
//! - Anything else under `/api/*` returns 404 rather than claiming support.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt;
use phase_identity::NodeIdentity;
use phase_manifest::ManifestBuilder;
use phase_net::PeerId;
use phase_protocol::{
    ChatMessage as PhaseChatMessage, ChatRole as PhaseChatRole, Completion, InferenceJobSpec,
    JobEvent, JobHandle, JobSpec, OutputChunk, SamplingParams,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::content::{ContentError, PullProgress, PullSelection};
use crate::router::{
    ReceiptVerification, RouteDecision, RouteVia, Router as LucidRouter, RouterError,
};

/// Explicit caller-signed generation bound for Ollama requests that omit
/// `options.num_predict`. Remote executors reject unbounded manifests, while
/// llama.cpp keeps an independent operator-owned ceiling.
const DEFAULT_MAX_TOKENS: u32 = 512;
const MAX_HTTP_MAX_TOKENS: u32 = 8192;
const MAX_HTTP_INPUT_CHARS: usize = 256 * 1024;
const MAX_EMBED_INPUTS: usize = 128;
const MAX_EMBED_DIMENSIONS: usize = 65_536;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_ROUTE_EXPLANATION_BYTES: usize = 192;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<WireMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub keep_alive: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<serde_json::Value>,
    #[serde(default)]
    pub format: Option<serde_json::Value>,
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WireMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ChatChunkResponse<'a> {
    model: &'a str,
    created_at: String,
    message: ChatChunkMessage<'a>,
    done: bool,
}

#[derive(Debug, Serialize)]
struct ChatFinalResponse<'a> {
    model: &'a str,
    created_at: String,
    message: ChatChunkMessage<'a>,
    done: bool,
    done_reason: &'a str,
    total_duration: u64,
    load_duration: u64,
    prompt_eval_count: u64,
    prompt_eval_duration: u64,
    eval_count: u64,
    eval_duration: u64,
}

#[derive(Debug, Serialize)]
struct ChatChunkMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct VersionResponse {
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Debug, Serialize)]
struct TagModel {
    name: String,
    model: String,
    modified_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    digest: String,
    details: TagModelDetails,
}

#[derive(Debug, Serialize)]
struct TagModelDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    family: String,
    families: Vec<String>,
    quantization_level: String,
}

#[derive(Debug, Deserialize)]
struct ShowRequest {
    model: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShowResponse {
    details: TagModelDetails,
    capabilities: Vec<&'static str>,
    model_info: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct PsResponse {
    models: Vec<TagModel>,
}

/// `/api/embed` request. Ollama accepts `input` as either a single string or
/// an array of strings. Controls that the signed embedding job cannot yet
/// represent are parsed so the handler can reject them explicitly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbedRequest {
    model: String,
    #[serde(default)]
    input: Option<EmbedInput>,
    #[serde(default)]
    truncate: Option<bool>,
    #[serde(default)]
    dimensions: Option<usize>,
    #[serde(default)]
    keep_alive: Option<serde_json::Value>,
    #[serde(default)]
    options: Option<serde_json::Value>,
}

/// The two shapes Ollama allows for `input`. `untagged` tries `One` (a bare
/// string) first, then `Many` (an array), so `"x"` and `["x","y"]` both
/// deserialize without the caller declaring which.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EmbedInput {
    One(String),
    Many(Vec<String>),
}

impl EmbedInput {
    /// Normalize either shape to the worker's `Vec<String>` input.
    fn into_vec(self) -> Vec<String> {
        match self {
            EmbedInput::One(s) => vec![s],
            EmbedInput::Many(v) => v,
        }
    }
}

/// Legacy `/api/embeddings` request — the singular-`prompt` shape Ollama
/// shipped before `/api/embed`. Exactly one input, returned as a single
/// `embedding` field rather than an `embeddings` array.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingsRequest {
    model: String,
    #[serde(default)]
    prompt: String,
}

/// `/api/pull` request. Ollama clients send the model name under either
/// `model` or `name` depending on version; `stream` defaults to true.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
    /// Optional exact 64-hex model CID pin. This is a LUCID extension; normal
    /// Ollama clients omit it.
    #[serde(default)]
    cid: Option<String>,
    /// Optional alias-publisher PeerId pin. This constrains who asserted the
    /// name mapping, not which peer may transfer the already pinned bytes.
    #[serde(default)]
    publisher: Option<String>,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppState {
    /// LUCID M5 router. Replaces the direct-worker dispatch the M4 spike
    /// shipped — the router decides per-request whether to serve locally
    /// or relay to a peer over `/phase/job-relay/1.0.0`.
    pub router: Arc<LucidRouter>,
    /// Identity used to sign job manifests submitted to the worker. Per
    /// `phase-core M5`, manifests are real `SignedManifest<JobSpec>` values
    /// rather than the previous unsigned placeholders.
    pub client_identity: NodeIdentity,
    /// M6 registry — resolves aliases to exact immutable model CIDs.
    pub registry: std::sync::Arc<crate::registry::ModelRegistry>,
    /// Operator-managed source model directory. None in echo/consume-only mode.
    pub model_dir: Option<std::path::PathBuf>,
    /// Verified content-addressed store used by `/api/pull`.
    pub artifact_store: Option<std::sync::Arc<phase_artifact_server::ArtifactStore>>,
    /// Worker-visible directory containing immutable `<cid>.gguf` hard links.
    pub verified_model_dir: Option<std::path::PathBuf>,
    /// Verified peer-to-peer content transfer coordinator. Present on real
    /// llama.cpp nodes and consume-only caches, absent from the explicit echo
    /// development fixture unless an artifact store was requested.
    pub content_plane: Option<std::sync::Arc<crate::content::ContentPlane>>,
}

/// HTTP header that flips this request to local-only mode. Honored on
/// every dispatch path — if the model isn't loaded locally we refuse
/// rather than route to a peer.
pub const HEADER_LOCAL_ONLY: &str = "x-lucid-local-only";

/// HTTP response header advertising where the request was actually
/// served. `local` or `peer:<short>`; omitted on Refused.
pub const HEADER_ROUTED_VIA: &str = "x-lucid-routed-via";

/// SEC-05: HTTP response header reporting whether a peer-served job's signed
/// receipt verified and bound to the dispatched job + peer. `true` / `false` /
/// `unverifiable`; omitted on the local path (no peer receipt to assert).
pub const HEADER_RECEIPT_VERIFIED: &str = "x-lucid-receipt-verified";

/// Bounded, public routing rationale. The value is a sanitized summary only:
/// it never contains raw evidence records or a peer identifier.
pub const HEADER_ROUTE_EXPLANATION: &str = "x-lucid-route-explanation";

/// Browser access is intentionally limited to loopback origins. LUCID's HTTP
/// API is unauthenticated, so reflecting arbitrary origins would let any web
/// page drive a contributor's local daemon. `localhost`, IPv4 loopback, and
/// IPv6 loopback are sufficient for local web clients while preserving the
/// same trust boundary as the default `127.0.0.1` listener.
fn is_loopback_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    matches!(
        uri.host(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    )
}

fn loopback_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _request| {
            is_loopback_origin(origin)
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static(HEADER_LOCAL_ONLY),
        ])
        .expose_headers([
            HeaderName::from_static("x-phase-worker"),
            HeaderName::from_static(HEADER_ROUTED_VIA),
            HeaderName::from_static(HEADER_ROUTE_EXPLANATION),
            HeaderName::from_static(HEADER_RECEIPT_VERIFIED),
            HeaderName::from_static("x-phase-receipt"),
        ])
}

/// Parse `X-Lucid-Local-Only`. Anything that looks truthy ("1", "true",
/// "yes", case-insensitive) flips the flag. Absent / empty → false.
fn parse_local_only(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER_LOCAL_ONLY)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Build a 503 response carrying the human-readable refusal reason.
fn refused_response(reason: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("router refused: {reason}"),
    )
        .into_response()
}

async fn resolve_job_model_cid(state: &AppState, model: &str) -> Result<String, Response> {
    match state.registry.resolve_model_cid(model).await {
        Ok(Some(cid)) => Ok(cid.to_hex()),
        Ok(None) => Err(refused_response(&format!(
            "model '{model}' has no verified local or signed network CID mapping"
        ))),
        Err(error) => {
            tracing::warn!(
                model = %sanitize_for_log(model),
                error = %error,
                "verified model CID resolution failed"
            );
            Err(refused_response("model CID resolution failed"))
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/chat", post(handle_chat))
        .route("/api/generate", post(handle_generate))
        .route("/api/version", get(handle_version))
        .route("/api/tags", get(handle_tags))
        .route("/api/show", post(handle_show))
        .route("/api/ps", get(handle_ps))
        // Embeddings — non-streaming; both the current (`/api/embed`) and the
        // legacy singular-prompt (`/api/embeddings`) request shapes.
        .route("/api/embed", post(handle_embed))
        .route("/api/embeddings", post(handle_embeddings))
        .route("/api/pull", post(handle_pull))
        // Health check for liveness probes.
        .route("/", get(|| async { "lucidd ready: see /api/version" }))
        // Log unsupported routes while preserving an honest 404 contract.
        .fallback(unknown)
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(loopback_cors())
        .with_state(state)
}

/// Max length of a sanitized log field (SEC-10). A 10 KB request path
/// shouldn't be able to blow up a log line.
const LOG_FIELD_CAP: usize = 256;

/// SEC-10: sanitize an attacker-controlled string before it goes into a
/// log line. Defends against log forging (embedded CR/LF spawning fake log
/// entries) and ANSI/terminal-escape abuse against anyone tailing logs.
///
/// - Drops C0 control bytes (`< 0x20`, includes CR/LF/TAB/NUL) and DEL
///   (`0x7f`).
/// - Replaces the ESC introducer (`0x1b`) — already covered by the C0
///   rule, but called out for clarity — so CSI/OSC sequences can't form.
/// - Caps the result at [`LOG_FIELD_CAP`] chars, appending an ellipsis
///   marker when truncated so the cap is visible in the log.
fn sanitize_for_log(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(LOG_FIELD_CAP));
    for ch in input.chars() {
        // Strip C0 controls (< 0x20) and DEL (0x7f). This covers \r, \n,
        // \t, NUL, and the ESC (0x1b) that introduces ANSI sequences.
        if (ch as u32) < 0x20 || ch == '\u{7f}' {
            continue;
        }
        if out.chars().count() >= LOG_FIELD_CAP {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Convert the router's internal explanation into a response-header-safe
/// public summary. Router explanations contain derived policy summaries, not
/// raw evidence, but this boundary additionally redacts identifier-shaped
/// tokens, removes controls/non-ASCII bytes, collapses whitespace, and applies
/// a hard byte cap before the value reaches either a header or structured log.
fn public_route_explanation(input: &str) -> String {
    let visible = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_graphic() || ch == ' ' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>();
    let compact = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut public = String::with_capacity(compact.len().min(MAX_ROUTE_EXPLANATION_BYTES));
    let mut identifier_run = String::new();
    let flush_identifier = |output: &mut String, run: &mut String| {
        if run.len() > 32 {
            output.push_str("[redacted]");
        } else {
            output.push_str(run);
        }
        run.clear();
    };
    for ch in compact.chars() {
        if ch.is_ascii_alphanumeric() {
            identifier_run.push(ch);
        } else {
            flush_identifier(&mut public, &mut identifier_run);
            public.push(ch);
        }
    }
    flush_identifier(&mut public, &mut identifier_run);
    if public.is_empty() {
        public.push_str("routing policy selected this executor");
    }
    if public.len() > MAX_ROUTE_EXPLANATION_BYTES {
        public.truncate(MAX_ROUTE_EXPLANATION_BYTES - 3);
        public.push_str("...");
    }
    public
}

async fn unknown(req: axum::http::Request<Body>) -> impl IntoResponse {
    // SEC-10: log only the path (not query/fragment), sanitized + capped.
    // The method is from a fixed HTTP enum, not attacker-shaped free text.
    let path = sanitize_for_log(req.uri().path());
    tracing::warn!(method = %req.method(), path = %path, "unimplemented endpoint");
    StatusCode::NOT_FOUND
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn handle_version() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn handle_tags(State(state): State<AppState>) -> Json<TagsResponse> {
    let installed = state.registry.local_installed_async().await;
    let mut loaded_only = state.registry.local_models_async().await;
    loaded_only.retain(|caps| {
        !installed
            .iter()
            .any(|model| model.model_cid == caps.model_cid)
    });
    loaded_only.sort_by(|left, right| {
        left.model_id
            .cmp(&right.model_id)
            .then_with(|| left.model_cid.0.cmp(&right.model_cid.0))
    });
    Json(TagsResponse {
        models: installed
            .iter()
            .map(tag_model_from_installed)
            .chain(
                loaded_only
                    .iter()
                    .map(|caps| tag_model_from_caps(state.verified_model_dir.as_deref(), caps)),
            )
            .collect(),
    })
}

async fn handle_show(State(state): State<AppState>, Json(req): Json<ShowRequest>) -> Response {
    let requested = req.model.or(req.name).unwrap_or_default();
    if let Some(installed) = resolve_local_installed(&state, &requested).await {
        let loaded = state
            .registry
            .local_models_async()
            .await
            .into_iter()
            .find(|caps| caps.model_cid == installed.model_cid);
        return Json(show_response_from_installed_and_caps(
            &installed,
            loaded.as_ref(),
        ))
        .into_response();
    }
    let resolved = match resolve_local_model_caps(&state, &requested).await {
        Ok(Some(caps)) => caps,
        Ok(None) => return unknown_model_response(&requested),
        Err(error) => {
            tracing::warn!(
                model = %sanitize_for_log(&requested),
                error = %error,
                "model metadata resolution failed"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "model metadata resolution failed" })),
            )
                .into_response();
        }
    };

    Json(show_response_from_caps(&resolved)).into_response()
}

fn tag_model_from_installed(model: &crate::registry::InstalledModel) -> TagModel {
    let name = ollama_model_name(&model.model_id);
    let family = model
        .model_id
        .split([':', '-'])
        .next()
        .unwrap_or(&model.model_id)
        .to_string();
    TagModel {
        name: name.clone(),
        model: name,
        modified_at: rfc3339_from_unix_millis(model.installed_at),
        size: Some(model.size_bytes),
        digest: format!("sha256:{}", model.model_cid.to_hex()),
        details: TagModelDetails {
            format: Some(model.format.clone()),
            family: family.clone(),
            families: vec![family],
            quantization_level: "unknown".to_string(),
        },
    }
}

/// `/api/ps` can only report models known to be resident in worker memory.
/// The registry tracks verified availability, not runtime residency, so an
/// empty list is the only truthful response until the worker exports that
/// telemetry. In particular, do not synthesize zero-sized/all-zero entries.
async fn handle_ps() -> Json<PsResponse> {
    Json(PsResponse { models: Vec::new() })
}

fn tag_model_from_caps(
    verified_model_dir: Option<&std::path::Path>,
    caps: &crate::registry::ModelCapabilities,
) -> TagModel {
    let size = verified_model_dir.and_then(|dir| {
        std::fs::metadata(dir.join(format!("{}.gguf", caps.model_cid.to_hex())))
            .ok()
            .map(|metadata| metadata.len())
    });
    let name = ollama_model_name(&caps.model_id);
    TagModel {
        name: name.clone(),
        model: name,
        modified_at: rfc3339_from_unix_millis(caps.advertised_at),
        size,
        digest: format!("sha256:{}", caps.model_cid.to_hex()),
        details: model_details_from_caps(caps),
    }
}

fn model_details_from_caps(caps: &crate::registry::ModelCapabilities) -> TagModelDetails {
    let family = caps
        .model_id
        .split([':', '-'])
        .next()
        .unwrap_or(&caps.model_id)
        .to_string();
    let format = match caps.backend.as_str() {
        "llama.cpp" => Some("gguf".to_string()),
        "echo" => Some("phase-echo".to_string()),
        _ => None,
    };
    TagModelDetails {
        format,
        family: family.clone(),
        families: vec![family],
        quantization_level: caps.quantization.clone(),
    }
}

fn show_response_from_caps(caps: &crate::registry::ModelCapabilities) -> ShowResponse {
    let mut model_info = std::collections::BTreeMap::new();
    model_info.insert(
        "phase.model_cid".to_string(),
        serde_json::Value::String(caps.model_cid.to_hex()),
    );
    model_info.insert(
        "phase.backend".to_string(),
        serde_json::Value::String(caps.backend.clone()),
    );
    model_info.insert(
        "phase.context_length".to_string(),
        serde_json::Value::from(caps.context_length),
    );
    model_info.insert(
        "phase.max_concurrent".to_string(),
        serde_json::Value::from(caps.max_concurrent),
    );
    model_info.insert(
        "phase.advertised_at".to_string(),
        serde_json::Value::from(caps.advertised_at),
    );
    model_info.insert(
        "phase.valid_until".to_string(),
        serde_json::Value::from(caps.valid_until),
    );
    ShowResponse {
        details: model_details_from_caps(caps),
        capabilities: vec!["completion"],
        model_info,
    }
}

fn show_response_from_installed_and_caps(
    model: &crate::registry::InstalledModel,
    caps: Option<&crate::registry::ModelCapabilities>,
) -> ShowResponse {
    let mut model_info = std::collections::BTreeMap::new();
    model_info.insert(
        "phase.model_cid".to_string(),
        serde_json::Value::String(model.model_cid.to_hex()),
    );
    model_info.insert(
        "phase.verification".to_string(),
        serde_json::Value::String("content_verified".to_string()),
    );
    model_info.insert(
        "phase.size_bytes".to_string(),
        serde_json::Value::from(model.size_bytes),
    );
    model_info.insert(
        "phase.installed_at".to_string(),
        serde_json::Value::from(model.installed_at),
    );
    if let Some(caps) = caps {
        model_info.insert(
            "phase.backend".to_string(),
            serde_json::Value::String(caps.backend.clone()),
        );
        model_info.insert(
            "phase.context_length".to_string(),
            serde_json::Value::from(caps.context_length),
        );
        model_info.insert(
            "phase.max_concurrent".to_string(),
            serde_json::Value::from(caps.max_concurrent),
        );
        model_info.insert(
            "phase.advertised_at".to_string(),
            serde_json::Value::from(caps.advertised_at),
        );
        model_info.insert(
            "phase.valid_until".to_string(),
            serde_json::Value::from(caps.valid_until),
        );
    }
    let family = model
        .model_id
        .split([':', '-'])
        .next()
        .unwrap_or(&model.model_id)
        .to_string();
    ShowResponse {
        details: TagModelDetails {
            format: Some(model.format.clone()),
            family: family.clone(),
            families: vec![family],
            quantization_level: caps
                .map(|caps| caps.quantization.clone())
                .unwrap_or_else(|| "unknown".to_string()),
        },
        capabilities: if caps.is_some() {
            vec!["completion"]
        } else {
            Vec::new()
        },
        model_info,
    }
}

fn ollama_model_name(model_id: &str) -> String {
    if model_id.contains(':') {
        model_id.to_string()
    } else {
        format!("{model_id}:latest")
    }
}

fn ollama_routing_model_name(model: &str) -> String {
    crate::registry::normalize_model_alias(model)
        .map(|normalized| {
            normalized
                .strip_suffix(":latest")
                .unwrap_or(&normalized)
                .to_string()
        })
        .unwrap_or_else(|_| model.to_string())
}

async fn resolve_local_model_caps(
    state: &AppState,
    requested: &str,
) -> anyhow::Result<Option<crate::registry::ModelCapabilities>> {
    let local_models = state.registry.local_models_async().await;
    let requested_cid = match crate::registry::ModelCid::from_hex(requested) {
        Ok(cid) => Some(cid),
        Err(_) => {
            let normalized = match crate::registry::normalize_model_alias(requested) {
                Ok(alias) => alias,
                Err(_) => return Ok(None),
            };
            let alias = normalized.strip_suffix(":latest").unwrap_or(&normalized);
            if let Some(caps) = local_models.iter().find(|caps| caps.model_id == alias) {
                return Ok(Some(caps.clone()));
            }
            state.registry.resolve_model_cid(alias).await?
        }
    };
    Ok(requested_cid.and_then(|cid| local_models.into_iter().find(|caps| caps.model_cid == cid)))
}

async fn resolve_local_installed(
    state: &AppState,
    requested: &str,
) -> Option<crate::registry::InstalledModel> {
    let installed = state.registry.local_installed_async().await;
    if let Ok(cid) = crate::registry::ModelCid::from_hex(requested) {
        return installed.into_iter().find(|model| model.model_cid == cid);
    }
    let normalized = crate::registry::normalize_model_alias(requested).ok()?;
    let alias = normalized.strip_suffix(":latest").unwrap_or(&normalized);
    installed.into_iter().find(|model| model.model_id == alias)
}

fn unknown_model_response(requested: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("model '{}' not found", sanitize_for_log(requested)),
        })),
    )
        .into_response()
}

fn invalid_request_response(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

fn validate_chat_request(request: &ChatRequest) -> Result<(), &'static str> {
    if request.keep_alive.is_some() || request.format.is_some() || request.tools.is_some() {
        return Err("keep_alive, structured format, and tools are not supported");
    }
    let mut total_chars = 0usize;
    for message in &request.messages {
        if parse_role(&message.role).is_none() {
            return Err("chat message role must be system, user, assistant, or tool");
        }
        if !message.images.is_empty() {
            return Err("image inputs are not supported by the configured LUCID workers");
        }
        total_chars = total_chars
            .checked_add(message.content.chars().count())
            .ok_or("chat input is too large")?;
        if total_chars > MAX_HTTP_INPUT_CHARS {
            return Err("chat input exceeds the 262144-character limit");
        }
    }
    Ok(())
}

fn validate_generate_request(request: &GenerateRequest) -> Result<(), &'static str> {
    if request.system.is_some()
        || request.keep_alive.is_some()
        || request.raw.is_some()
        || request.suffix.is_some()
        || request.images.is_some()
        || request.template.is_some()
    {
        return Err(
            "system, keep_alive, raw, suffix, images, and template controls are not supported",
        );
    }
    if request
        .prompt
        .as_deref()
        .unwrap_or_default()
        .chars()
        .count()
        > MAX_HTTP_INPUT_CHARS
    {
        return Err("generate input exceeds the 262144-character limit");
    }
    Ok(())
}

fn validate_embedding_input(input: &[String]) -> Result<(), &'static str> {
    if input.len() > MAX_EMBED_INPUTS {
        return Err("embedding input count exceeds 128");
    }
    let mut total_chars = 0usize;
    for item in input {
        total_chars = total_chars
            .checked_add(item.chars().count())
            .ok_or("embedding input is too large")?;
        if total_chars > MAX_HTTP_INPUT_CHARS {
            return Err("embedding input exceeds the 262144-character limit");
        }
    }
    Ok(())
}

/// Translate the bounded Ollama option subset into Phase's signed sampling
/// map. Unknown keys are refused at the HTTP boundary instead of being
/// silently ignored or becoming meaningful after a backend upgrade.
fn parse_ollama_options(
    options: Option<&serde_json::Value>,
) -> Result<(SamplingParams, u32), String> {
    let Some(options) = options else {
        return Ok((SamplingParams::default(), DEFAULT_MAX_TOKENS));
    };
    let object = options
        .as_object()
        .ok_or_else(|| "options must be a JSON object".to_string())?;
    let mut sampling = SamplingParams::default();
    let mut max_tokens = DEFAULT_MAX_TOKENS;
    for (wire_key, value) in object {
        if wire_key == "num_predict" {
            let Some(requested) = value.as_u64() else {
                return Err("options.num_predict must be an integer within 1..=8192".to_string());
            };
            if !(1..=u64::from(MAX_HTTP_MAX_TOKENS)).contains(&requested) {
                return Err("options.num_predict must be an integer within 1..=8192".to_string());
            }
            max_tokens = requested as u32;
            continue;
        }

        let protocol_key = match wire_key.as_str() {
            "temperature" | "top_p" | "top_k" | "min_p" | "seed" | "stop" => wire_key.as_str(),
            "repeat_penalty" | "repetition_penalty" => "repetition_penalty",
            _ => return Err("request contains an unsupported generation option".to_string()),
        };
        if sampling.params.contains_key(protocol_key) {
            return Err("request repeats the same generation option".to_string());
        }
        let encoded = serde_json::to_string(value)
            .map_err(|_| "generation option could not be encoded".to_string())?;
        sampling.params.insert(protocol_key.to_string(), encoded);
    }
    Ok((sampling, max_tokens))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateRequest {
    pub model: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub keep_alive: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<serde_json::Value>,
    #[serde(default)]
    pub raw: Option<bool>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub images: Option<Vec<String>>,
    #[serde(default)]
    pub template: Option<String>,
}

/// `/api/generate` — same streaming machinery as `/api/chat`, but the
/// per-chunk field is `"response"` and there's no `"message"` wrapper.
/// `ollama run <model> "<prompt>"` (non-interactive) hits this path.
async fn handle_generate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GenerateRequest>,
) -> Response {
    if let Err(error) = validate_generate_request(&req) {
        return invalid_request_response(error);
    }
    let model = req.model.clone();
    let stream_mode = req.stream.unwrap_or(true);
    let prompt = req.prompt.clone().unwrap_or_default();

    let local_only = parse_local_only(&headers);

    // Route decision. Refusals short-circuit to 503 without ever
    // touching the worker.
    let routing_model = ollama_routing_model_name(&model);
    let decision = state.router.route(&routing_model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(&model), reason = %reason, "router refused /api/generate");
        return refused_response(reason);
    }
    let routed_via = decision.header_value();
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP generate route selected"
    );
    let model_cid = match resolve_job_model_cid(&state, &routing_model).await {
        Ok(cid) => cid,
        Err(response) => return response,
    };
    let (sampling, max_tokens) = match parse_ollama_options(req.options.as_ref()) {
        Ok(options) => options,
        Err(error) => return invalid_request_response(&error),
    };

    let job_spec = JobSpec::Inference(InferenceJobSpec {
        model_cid,
        messages: Vec::new(),
        prompt: Some(prompt),
        resume_from: None,
        sampling,
        max_tokens: Some(max_tokens),
        stream: stream_mode,
    });

    // Sign with the AppState identity. Each call's `created_at` differs by
    // wall-clock so successive jobs get distinct manifest hashes (and
    // therefore distinct JobIds) without needing a per-request UUID.
    let manifest = match http_manifest(job_spec).sign_with(&state.client_identity) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "manifest signing failed (/api/generate)");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("manifest signing failed: {e}"),
            )
                .into_response();
        }
    };

    let (handle, mut job_stream, receipt_verification) =
        match state.router.execute(&decision, manifest).await {
            Ok(t) => t,
            Err(RouterError::Refused { reason }) => return refused_response(&reason),
            Err(e) => {
                tracing::error!(error = %e, "router dispatch failed (/api/generate)");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("router dispatch failed: {e}"),
                )
                    .into_response();
            }
        };

    let job_id = handle.job_id().clone();
    let started_at = std::time::Instant::now();

    if !stream_mode {
        let mut acc = String::new();
        let mut terminal = HttpTerminal::default();
        let mut next_output_seq = 0u64;
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) if terminal.output_error.is_none() => {
                    match validated_text_chunk(&chunk, &mut next_output_seq) {
                        Ok(text) => acc.push_str(text),
                        Err(error) => {
                            tracing::error!(%job_id, seq = chunk.seq, "invalid generate output chunk");
                            terminal.observe_output_error(error);
                        }
                    }
                }
                JobEvent::Final { result, error } => {
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    terminal.observe(result.completion, error);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }
        let finished_receipt = finish_http_receipt(handle, receipt_verification).await;
        if let Some(reason) = execution_failure_reason(&terminal, finished_receipt.verification) {
            return execution_failure_response(
                reason,
                routed_via.as_deref(),
                finished_receipt.verification,
                Some(&route_explanation),
            );
        }
        let total_duration = started_at.elapsed().as_nanos() as u64;
        let body = serde_json::json!({
            "model": model,
            "created_at": rfc3339_now(),
            "response": acc,
            "done": true,
            "done_reason": terminal.done_reason(),
            "context": [],
            "total_duration": total_duration,
            "load_duration": 0u64,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0u64,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });
        let mut resp = (StatusCode::OK, Json(body)).into_response();
        if let Some(commitment) = finished_receipt.commitment.as_ref() {
            if let Ok(hv) = receipt_header_value(commitment).parse() {
                resp.headers_mut().insert("X-Phase-Receipt", hv);
            }
        }
        apply_result_headers(
            &mut resp,
            routed_via.as_deref(),
            finished_receipt.verification,
            Some(&route_explanation),
        );
        tracing::info!(%job_id, "non-streaming generate complete");
        return resp;
    }

    let model_for_body = model.clone();
    let ndjson = stream! {
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut terminal = HttpTerminal::default();
        let mut next_output_seq = 0u64;

        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) if terminal.output_error.is_none() => {
                    match validated_text_chunk(&chunk, &mut next_output_seq) {
                        Ok(text) => {
                            let payload = serde_json::json!({
                                "model": &model_for_body,
                                "created_at": rfc3339_now(),
                                "response": text,
                                "done": false,
                            });
                            if let Ok(mut bytes) = serde_json::to_vec(&payload) {
                                bytes.push(b'\n');
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(bytes));
                            }
                        }
                        Err(error) => {
                            tracing::error!(%job_id, seq = chunk.seq, "invalid generate output chunk");
                            terminal.observe_output_error(error);
                        }
                    }
                }
                JobEvent::Final { result, error } => {
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    terminal.observe(result.completion, error);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }
        let finished_receipt = finish_http_receipt(handle, receipt_verification).await;
        let total_duration = started_at.elapsed().as_nanos() as u64;
        if let Some(commitment) = finished_receipt.commitment.as_ref() {
            tracing::info!(
                %job_id,
                commitment = %hex32(commitment),
                verification = ?finished_receipt.verification,
                "terminal receipt resolved (generate)"
            );
        }
        let mut final_value = serde_json::json!({
            "model": &model_for_body,
            "created_at": rfc3339_now(),
            "response": "",
            "done": true,
            "done_reason": terminal.done_reason(),
            "context": [],
            "total_duration": total_duration,
            "load_duration": 0,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });
        annotate_stream_terminal(&mut final_value, &terminal, &finished_receipt);
        if let Ok(mut bytes) = serde_json::to_vec(&final_value) {
            bytes.push(b'\n');
            yield Ok(Bytes::from(bytes));
        }
    };
    let body = Body::from_stream(ndjson);
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header("X-Phase-Worker", "lucidd");
    if let Some(rv) = routed_via.as_deref() {
        builder = builder.header(HEADER_ROUTED_VIA, rv);
    }
    builder = builder.header(HEADER_ROUTE_EXPLANATION, &route_explanation);
    // A live peer starts as `pending`; only legacy pre-verified batches may
    // truthfully say `true` before the body. The final verdict is in NDJSON.
    if let Some(v) = receipt_verification.header_value() {
        builder = builder.header(HEADER_RECEIPT_VERIFIED, v);
    }
    builder.body(body).unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
    })
}

async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    if let Err(error) = validate_chat_request(&req) {
        return invalid_request_response(error);
    }
    let model = req.model.clone();
    let stream_mode = req.stream.unwrap_or(true);

    let local_only = parse_local_only(&headers);

    // Route decision (M5). Refusals short-circuit to 503 before we
    // build a manifest or touch a worker.
    let routing_model = ollama_routing_model_name(&model);
    let decision: RouteDecision = state.router.route(&routing_model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(&model), reason = %reason, "router refused /api/chat");
        return refused_response(reason);
    }
    let routed_via = decision.header_value();
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP chat route selected"
    );
    let model_cid = match resolve_job_model_cid(&state, &routing_model).await {
        Ok(cid) => cid,
        Err(response) => return response,
    };
    let (sampling, max_tokens) = match parse_ollama_options(req.options.as_ref()) {
        Ok(options) => options,
        Err(error) => return invalid_request_response(&error),
    };

    // Translate wire → JobSpec.
    let messages: Vec<PhaseChatMessage> = req
        .messages
        .iter()
        .map(|m| PhaseChatMessage {
            role: parse_role(&m.role).expect("chat role validated before manifest construction"),
            content: m.content.clone(),
            images: m.images.clone(),
        })
        .collect();

    let job_spec = JobSpec::Inference(InferenceJobSpec {
        model_cid,
        messages,
        prompt: None,
        resume_from: None,
        sampling,
        max_tokens: Some(max_tokens),
        stream: stream_mode,
    });

    // Real signed manifest. M5 swapped the pseudo-manifest UUID for a
    // canonical Ed25519 signature over the job spec; `created_at` carries
    // enough entropy to keep successive JobIds distinct.
    let manifest = match http_manifest(job_spec).sign_with(&state.client_identity) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "manifest signing failed (/api/chat)");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("manifest signing failed: {e}"),
            )
                .into_response();
        }
    };

    let (handle, mut job_stream, receipt_verification) =
        match state.router.execute(&decision, manifest).await {
            Ok(t) => t,
            Err(RouterError::Refused { reason }) => return refused_response(&reason),
            Err(e) => {
                tracing::error!(error = %e, "router dispatch failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("router dispatch failed: {e}"),
                )
                    .into_response();
            }
        };

    let job_id = handle.job_id().clone();
    let started_at = std::time::Instant::now();

    // ----- non-streaming path: collect everything, send a single JSON ----
    if !stream_mode {
        let mut acc = String::new();
        let mut terminal = HttpTerminal::default();
        let mut next_output_seq = 0u64;
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) if terminal.output_error.is_none() => {
                    match validated_text_chunk(&chunk, &mut next_output_seq) {
                        Ok(text) => acc.push_str(text),
                        Err(error) => {
                            tracing::error!(%job_id, seq = chunk.seq, "invalid chat output chunk");
                            terminal.observe_output_error(error);
                        }
                    }
                }
                JobEvent::Final { result, error } => {
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    terminal.observe(result.completion, error);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }

        let finished_receipt = finish_http_receipt(handle, receipt_verification).await;
        if let Some(reason) = execution_failure_reason(&terminal, finished_receipt.verification) {
            return execution_failure_response(
                reason,
                routed_via.as_deref(),
                finished_receipt.verification,
                Some(&route_explanation),
            );
        }

        let total_duration = started_at.elapsed().as_nanos() as u64;
        let body = serde_json::json!({
            "model": model,
            "created_at": rfc3339_now(),
            "message": { "role": "assistant", "content": acc },
            "done": true,
            "done_reason": terminal.done_reason(),
            "total_duration": total_duration,
            "load_duration": 0u64,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0u64,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });

        let mut resp = (StatusCode::OK, Json(body)).into_response();
        if let Some(commitment) = finished_receipt.commitment.as_ref() {
            if let Ok(hv) = receipt_header_value(commitment).parse() {
                resp.headers_mut().insert("X-Phase-Receipt", hv);
            }
        }
        apply_result_headers(
            &mut resp,
            routed_via.as_deref(),
            finished_receipt.verification,
            Some(&route_explanation),
        );
        tracing::info!(%job_id, "non-streaming chat complete");
        return resp;
    }

    // ----- streaming path: NDJSON body driven by the JobStream -----------
    let model_for_body = model.clone();
    let ndjson = stream! {
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut terminal = HttpTerminal::default();
        let mut next_output_seq = 0u64;

        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) if terminal.output_error.is_none() => {
                    match validated_text_chunk(&chunk, &mut next_output_seq) {
                        Ok(text) => {
                            let payload = ChatChunkResponse {
                                model: &model_for_body,
                                created_at: rfc3339_now(),
                                message: ChatChunkMessage {
                                    role: "assistant",
                                    content: text,
                                },
                                done: false,
                            };
                            match serde_json::to_vec(&payload) {
                                Ok(mut bytes) => {
                                    bytes.push(b'\n');
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(bytes));
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to serialize chunk");
                                }
                            }
                        }
                        Err(error) => {
                            tracing::error!(%job_id, seq = chunk.seq, "invalid chat output chunk");
                            terminal.observe_output_error(error);
                        }
                    }
                }
                JobEvent::Final { result, error } => {
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    terminal.observe(result.completion, error);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }

        // Trailer-free protocol: headers are already flushed, so resolve the
        // receipt after tokens and carry the final verification in-band.
        let finished_receipt = finish_http_receipt(handle, receipt_verification).await;
        let total_duration = started_at.elapsed().as_nanos() as u64;
        if let Some(commitment) = finished_receipt.commitment.as_ref() {
            tracing::info!(
                %job_id,
                commitment = %hex32(commitment),
                verification = ?finished_receipt.verification,
                "terminal receipt resolved"
            );
        }

        let final_payload = ChatFinalResponse {
            model: &model_for_body,
            created_at: rfc3339_now(),
            message: ChatChunkMessage { role: "assistant", content: "" },
            done: true,
            done_reason: terminal.done_reason(),
            total_duration,
            load_duration: 0,
            prompt_eval_count: prompt_tokens,
            prompt_eval_duration: 0,
            eval_count: completion_tokens,
            eval_duration: total_duration,
        };
        let mut final_value = serde_json::to_value(&final_payload).unwrap_or(serde_json::json!({}));
        annotate_stream_terminal(&mut final_value, &terminal, &finished_receipt);
        if let Ok(mut bytes) = serde_json::to_vec(&final_value) {
            bytes.push(b'\n');
            yield Ok(Bytes::from(bytes));
        }
    };

    let body = Body::from_stream(ndjson);
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header("X-Phase-Worker", "lucidd");
    if let Some(rv) = routed_via.as_deref() {
        builder = builder.header(HEADER_ROUTED_VIA, rv);
    }
    builder = builder.header(HEADER_ROUTE_EXPLANATION, &route_explanation);
    // A live peer starts as `pending`; the final verdict is emitted in-band
    // after the receipt arrives. Never promote this header optimistically.
    if let Some(v) = receipt_verification.header_value() {
        builder = builder.header(HEADER_RECEIPT_VERIFIED, v);
    }
    builder.body(body).unwrap_or_else(|e| {
        tracing::error!(error = %e, "failed to build streaming response");
        (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
    })
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

/// Strictly reconstruct one embedding vector per input from sequence-labelled
/// output chunks. The collector records the first protocol violation but the
/// caller continues draining the stream so receipt/commitment verification is
/// never bypassed by malformed worker output.
struct EmbeddingCollector {
    slots: Vec<Option<Vec<f32>>>,
    dimensions: Option<usize>,
    error: Option<String>,
}

impl EmbeddingCollector {
    fn new(expected_outputs: usize) -> Self {
        Self {
            slots: vec![None; expected_outputs],
            dimensions: None,
            error: None,
        }
    }

    fn observe(&mut self, chunk: &OutputChunk) {
        if self.error.is_some() {
            return;
        }
        let result = self.validate_and_store(chunk);
        if let Err(error) = result {
            self.error = Some(error);
        }
    }

    fn validate_and_store(&mut self, chunk: &OutputChunk) -> Result<(), String> {
        if chunk.kind != "embedding" {
            return Err("worker emitted a non-embedding chunk for embedding inference".to_string());
        }
        let index = usize::try_from(chunk.seq)
            .map_err(|_| "worker emitted an out-of-range embedding sequence".to_string())?;
        let slot = self
            .slots
            .get_mut(index)
            .ok_or_else(|| "worker emitted an extra embedding vector".to_string())?;
        if slot.is_some() {
            return Err("worker emitted a duplicate embedding vector".to_string());
        }
        let vector = serde_json::from_slice::<Vec<f32>>(&chunk.data)
            .map_err(|_| "worker emitted a malformed embedding vector".to_string())?;
        validate_embedding_values(&vector)?;
        match self.dimensions {
            Some(dimensions) if dimensions != vector.len() => {
                return Err("worker emitted inconsistent embedding dimensions".to_string());
            }
            None => self.dimensions = Some(vector.len()),
            Some(_) => {}
        }
        *slot = Some(vector);
        Ok(())
    }

    fn finish(self) -> Result<Vec<Vec<f32>>, String> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.slots
            .into_iter()
            .map(|slot| {
                slot.ok_or_else(|| {
                    "worker returned fewer embedding vectors than inputs".to_string()
                })
            })
            .collect()
    }
}

fn validate_embedding_values(vector: &[f32]) -> Result<(), String> {
    if vector.is_empty() || vector.len() > MAX_EMBED_DIMENSIONS {
        return Err("worker emitted an invalid embedding dimension".to_string());
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err("worker emitted a non-finite embedding value".to_string());
    }
    Ok(())
}

/// Outcome of running the embedding pipeline: the ordered vectors plus the
/// header values that `handle_chat`'s non-streaming branch also sets. Kept as
/// a small struct so both embedding handlers share the route → sign → execute
/// → drain machinery without duplicating it.
struct EmbedOutcome {
    /// Per-input embedding vectors, ordered by `OutputChunk::seq`.
    vectors: Vec<Vec<f32>>,
    /// `X-Lucid-Routed-Via` value (`local` / `peer:<short>`), if any.
    routed_via: Option<String>,
    /// `X-Lucid-Receipt-Verified` value (peer path only), if any.
    receipt_verified: Option<&'static str>,
    /// `X-Phase-Receipt` value (commitment) once `finish()` surfaced it.
    receipt: Option<String>,
    /// Sanitized, bounded explanation derived from the successful route.
    route_explanation: String,
}

/// Run an embedding job through the SAME pipeline as `handle_chat`:
/// `router.route()` → refused-check → `ManifestBuilder.sign_with` →
/// `router.execute()` → drain the `JobStream`. Embeddings are never streamed
/// to the client (Ollama returns them in one JSON body), so we always collect.
///
/// Per the shared embedding wire convention, every `JobEvent::Output` with
/// `kind == "embedding"` carries `serde_json::to_vec(&Vec<f32>)`; we key each
/// by `chunk.seq` and sort so the result order matches the input order
/// regardless of the order chunks arrived in. `Err` is a `Response` ready to
/// return (router refusal → 503, signing / dispatch failure → 500).
async fn run_embedding(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
    input: Vec<String>,
) -> Result<EmbedOutcome, Response> {
    let local_only = parse_local_only(headers);

    // Route decision. Refusals short-circuit to 503 before we build a
    // manifest or touch a worker — identical to handle_chat.
    let routing_model = ollama_routing_model_name(model);
    let decision: RouteDecision = state.router.route(&routing_model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(model), reason = %reason, "router refused /api/embed");
        return Err(refused_response(reason));
    }
    let routed_via = decision.header_value();
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP embedding route selected"
    );
    let model_cid = resolve_job_model_cid(state, &routing_model).await?;

    let expected_output_count = input.len();
    let job_spec = JobSpec::Embedding(phase_protocol::EmbeddingJobSpec { model_cid, input });

    let manifest = match http_manifest(job_spec).sign_with(&state.client_identity) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "manifest signing failed (/api/embed)");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("manifest signing failed: {e}"),
            )
                .into_response());
        }
    };

    let (handle, mut job_stream, receipt_verification) =
        match state.router.execute(&decision, manifest).await {
            Ok(t) => t,
            Err(RouterError::Refused { reason }) => return Err(refused_response(&reason)),
            Err(e) => {
                tracing::error!(error = %e, "router dispatch failed (/api/embed)");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("router dispatch failed: {e}"),
                )
                    .into_response());
            }
        };

    let job_id = handle.job_id().clone();

    let mut collector = EmbeddingCollector::new(expected_output_count);
    let mut terminal = HttpTerminal::default();
    while let Some(ev) = job_stream.next().await {
        match ev {
            JobEvent::Output(chunk) => {
                collector.observe(&chunk);
            }
            JobEvent::Final { result, error } => {
                terminal.observe(result.completion, error);
            }
            JobEvent::Progress(_) => {}
            _ => {}
        }
    }
    let vectors = match collector.finish() {
        Ok(vectors) => vectors,
        Err(error) => {
            tracing::error!(%job_id, "invalid embedding output stream");
            terminal.observe_output_error(error);
            Vec::new()
        }
    };

    let finished_receipt = finish_http_receipt(handle, receipt_verification).await;
    if let Some(reason) = execution_failure_reason(&terminal, finished_receipt.verification) {
        return Err(execution_failure_response(
            reason,
            routed_via.as_deref(),
            finished_receipt.verification,
            Some(&route_explanation),
        ));
    }
    let receipt = finished_receipt
        .commitment
        .as_ref()
        .map(receipt_header_value);

    tracing::info!(%job_id, count = vectors.len(), "embedding job complete");

    Ok(EmbedOutcome {
        vectors,
        routed_via,
        receipt_verified: finished_receipt.verification.header_value(),
        receipt,
        route_explanation,
    })
}

/// Attach the standard LUCID headers to an embedding response, mirroring the
/// header-setting logic in `handle_chat`'s non-streaming branch.
fn apply_embed_headers(resp: &mut Response, outcome: &EmbedOutcome) {
    if let Some(v) = outcome.receipt.as_deref() {
        if let Ok(hv) = v.parse() {
            resp.headers_mut().insert("X-Phase-Receipt", hv);
        }
    }
    if let Some(rv) = outcome.routed_via.as_deref() {
        if let Ok(hv) = rv.parse() {
            resp.headers_mut().insert(HEADER_ROUTED_VIA, hv);
        }
    }
    if let Ok(hv) = outcome.route_explanation.parse() {
        resp.headers_mut().insert(HEADER_ROUTE_EXPLANATION, hv);
    }
    // SEC-05: surface peer-receipt verification status.
    if let Some(v) = outcome.receipt_verified {
        if let Ok(hv) = v.parse() {
            resp.headers_mut().insert(HEADER_RECEIPT_VERIFIED, hv);
        }
    }
}

/// `/api/embed` — current Ollama embedding endpoint. Accepts `input` as a
/// single string or an array; returns an `embeddings` array (one vector per
/// input). Non-streaming: Ollama never streams embeddings.
async fn handle_embed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EmbedRequest>,
) -> Response {
    if req.truncate.is_some()
        || req.dimensions.is_some()
        || req.keep_alive.is_some()
        || req.options.is_some()
    {
        return invalid_request_response(
            "truncate, dimensions, keep_alive, and embedding options are not supported",
        );
    }
    let model = req.model.clone();
    let input = req.input.map(EmbedInput::into_vec).unwrap_or_default();
    if let Err(error) = validate_embedding_input(&input) {
        return invalid_request_response(error);
    }

    let outcome = match run_embedding(&state, &headers, &model, input).await {
        Ok(o) => o,
        Err(resp) => return resp,
    };

    let body = serde_json::json!({
        "model": model,
        "embeddings": outcome.vectors,
    });
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    apply_embed_headers(&mut resp, &outcome);
    resp
}

/// `/api/embeddings` — LEGACY singular-prompt endpoint. Takes one `prompt`
/// string and returns a single `embedding` vector. An empty prompt sends no
/// input (and yields `[]`).
async fn handle_embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    let model = req.model.clone();
    // Empty prompt → no input → empty result, rather than embedding "".
    let input = if req.prompt.is_empty() {
        Vec::new()
    } else {
        vec![req.prompt.clone()]
    };

    let outcome = match run_embedding(&state, &headers, &model, input).await {
        Ok(o) => o,
        Err(resp) => return resp,
    };

    // Legacy shape: a single `embedding` (the first vector, or `[]` if none).
    let first: Vec<f32> = outcome.vectors.first().cloned().unwrap_or_default();
    let body = serde_json::json!({
        "model": model,
        "embedding": first,
    });
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    apply_embed_headers(&mut resp, &outcome);
    resp
}

// ---------------------------------------------------------------------------
// Pull / verified local import
// ---------------------------------------------------------------------------

/// SEC-04-style name hygiene for `/api/pull`: reject anything that could
/// escape `model_dir` or be mistaken for a flag, matching `worker_llama`'s
/// `resolve_model_path` guard (separators, `..`, leading `-`, empty). We keep
/// the simple `dir.join("<name>.gguf").is_file()` shape the task calls for
/// rather than the full canonicalize-and-confine resolver, but the *name*
/// hygiene is identical so a hostile name never reaches the filesystem.
fn pull_name_is_safe(name: &str) -> bool {
    crate::registry::normalize_model_alias(name).is_ok()
}

fn parse_pull_selection(
    cid: Option<&str>,
    publisher: Option<&str>,
) -> Result<PullSelection, String> {
    let exact_cid = cid
        .map(crate::registry::ModelCid::from_hex)
        .transpose()
        .map_err(|_| "cid must be exactly 64 hexadecimal characters".to_string())?;
    let publisher = publisher
        .map(str::parse::<PeerId>)
        .transpose()
        .map_err(|_| "publisher must be a valid libp2p PeerId".to_string())?;
    Ok(PullSelection {
        exact_cid,
        publisher,
    })
}

/// Resolve/import local source content first, then fall back to the signed
/// peer-to-peer content plane. Both paths publish metadata only after exact
/// size/CID verification and atomic installation.
async fn handle_pull(State(state): State<AppState>, Json(req): Json<PullRequest>) -> Response {
    let stream_mode = req.stream.unwrap_or(true);
    let selection = match parse_pull_selection(req.cid.as_deref(), req.publisher.as_deref()) {
        Ok(selection) => selection,
        Err(error) => return invalid_request_response(&error),
    };
    let name = match req.model.or(req.name) {
        Some(n) => n,
        None => {
            // No name at all — treat like a not-found so clients get a clear
            // signal rather than a silent success.
            return pull_not_found_response("", stream_mode);
        }
    };
    if !pull_name_is_safe(&name) {
        tracing::info!(model = %sanitize_for_log(&name), "rejected unsafe /api/pull name");
        return pull_not_found_response(&name, stream_mode);
    }

    match (
        &state.model_dir,
        &state.artifact_store,
        &state.verified_model_dir,
    ) {
        (Some(dir), Some(artifact_store), Some(verified_model_dir)) => {
            let source_path = dir.join(format!("{name}.gguf"));
            if selection == PullSelection::default() && source_path.is_file() {
                return match state
                    .registry
                    .import_verified_gguf(
                        artifact_store.clone(),
                        dir.clone(),
                        source_path,
                        verified_model_dir.clone(),
                        &name,
                        8192,
                        1,
                        "llama.cpp",
                    )
                    .await
                {
                    Ok(caps) => pull_success_response(stream_mode, Some(&caps.model_cid.to_hex())),
                    Err(error) => {
                        tracing::error!(
                            model = %sanitize_for_log(&name),
                            error = %error,
                            "verified /api/pull import failed"
                        );
                        pull_error_response("verified model import failed", stream_mode)
                    }
                };
            }
        }
        (None, Some(_), Some(_)) => {}
        (None, None, None) => {}
        _ => {
            tracing::error!("incomplete artifact-store configuration for /api/pull");
            return pull_error_response("model store is not configured correctly", stream_mode);
        }
    }

    let Some(content_plane) = state.content_plane else {
        return pull_unavailable_response(
            "model pull is unavailable: no verified content store is configured",
            stream_mode,
        );
    };
    if stream_mode {
        return pull_network_stream(content_plane, name, selection);
    }

    match content_plane.pull_selected(&name, selection, None).await {
        Ok(installed) => pull_success_response(false, Some(&installed.model_cid.to_hex())),
        Err(error) => {
            tracing::warn!(
                model = %sanitize_for_log(&name),
                error = %error,
                "peer-to-peer model pull failed"
            );
            pull_content_error_response(error)
        }
    }
}

fn pull_network_stream(
    content_plane: Arc<crate::content::ContentPlane>,
    name: String,
    selection: PullSelection,
) -> Response {
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(16);
    let task = tokio::spawn(async move {
        content_plane
            .pull_selected(&name, selection, Some(progress_tx))
            .await
    });
    let abort_guard = AbortTaskOnDrop(task.abort_handle());
    let body_stream = stream! {
        let _abort_guard = abort_guard;
        let mut task = task;
        let mut emitted_success = false;
        loop {
            tokio::select! {
                progress = progress_rx.recv() => {
                    if let Some(progress) = progress {
                        emitted_success |= matches!(progress, PullProgress::Success { .. });
                        yield Ok::<Bytes, std::convert::Infallible>(pull_progress_line(progress));
                    }
                }
                result = &mut task => {
                    while let Ok(progress) = progress_rx.try_recv() {
                        emitted_success |= matches!(progress, PullProgress::Success { .. });
                        yield Ok::<Bytes, std::convert::Infallible>(pull_progress_line(progress));
                    }
                    match result {
                        Ok(Ok(installed)) if !emitted_success => {
                            yield Ok::<Bytes, std::convert::Infallible>(json_line(serde_json::json!({
                                "status": "success",
                                "phase": "success",
                                "digest": format!("sha256:{}", installed.model_cid.to_hex()),
                            })));
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            yield Ok::<Bytes, std::convert::Infallible>(json_line(serde_json::json!({
                                "status": "error",
                                "error": error.to_string(),
                            })));
                        }
                        Err(error) => {
                            tracing::error!(error = %error, "content pull task failed");
                            yield Ok::<Bytes, std::convert::Infallible>(json_line(serde_json::json!({
                                "status": "error",
                                "error": "content pull task failed",
                            })));
                        }
                    }
                    break;
                }
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

struct AbortTaskOnDrop(tokio::task::AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn pull_progress_line(progress: PullProgress) -> Bytes {
    let value = match progress {
        PullProgress::Resolving { alias } => {
            serde_json::json!({ "status": "resolving", "phase": "resolving", "model": alias })
        }
        PullProgress::SelectingProvider { cid, providers } => serde_json::json!({
            "status": "selecting provider", "phase": "selecting_provider",
            "digest": format!("sha256:{cid}"), "providers": providers,
        }),
        PullProgress::Downloading {
            cid,
            completed,
            total,
            provider,
        } => serde_json::json!({
            "status": "downloading", "phase": "downloading",
            "digest": format!("sha256:{cid}"), "completed": completed,
            "total": total, "provider": provider,
        }),
        PullProgress::Verifying { cid, total } => serde_json::json!({
            "status": "verifying", "phase": "verifying",
            "digest": format!("sha256:{cid}"), "total": total,
        }),
        PullProgress::Installing { cid } => serde_json::json!({
            "status": "installing", "phase": "installing",
            "digest": format!("sha256:{cid}"),
        }),
        PullProgress::Registering { alias, cid } => serde_json::json!({
            "status": "registering", "phase": "registering", "model": alias,
            "digest": format!("sha256:{cid}"),
        }),
        PullProgress::Success { alias, cid, size } => serde_json::json!({
            "status": "success", "phase": "success", "model": alias,
            "digest": format!("sha256:{cid}"), "total": size,
        }),
    };
    json_line(value)
}

fn json_line(value: serde_json::Value) -> Bytes {
    let mut bytes = serde_json::to_vec(&value)
        .unwrap_or_else(|_| b"{\"status\":\"error\",\"error\":\"serialization failure\"}".to_vec());
    bytes.push(b'\n');
    Bytes::from(bytes)
}

fn pull_content_error_response(error: ContentError) -> Response {
    let status = match &error {
        ContentError::UnknownAlias => StatusCode::NOT_FOUND,
        ContentError::AliasConflict => StatusCode::CONFLICT,
        ContentError::PinMismatch => StatusCode::PRECONDITION_FAILED,
        ContentError::UnsupportedFormat(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ContentError::ModelTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        ContentError::NoProviders => StatusCode::SERVICE_UNAVAILABLE,
        ContentError::Transfer(_) | ContentError::ProvidersExhausted { .. } => {
            StatusCode::BAD_GATEWAY
        }
        ContentError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        ContentError::Verification(_) => StatusCode::UNPROCESSABLE_ENTITY,
        ContentError::Cancelled => StatusCode::REQUEST_TIMEOUT,
        ContentError::Configuration(_) | ContentError::Registration(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (
        status,
        Json(serde_json::json!({ "status": "error", "error": error.to_string() })),
    )
        .into_response()
}

/// Build the success response for `/api/pull` in the requested transport.
fn pull_success_response(stream_mode: bool, digest: Option<&str>) -> Response {
    let terminal = match digest {
        Some(digest) => {
            serde_json::json!({ "status": "success", "digest": format!("sha256:{digest}") })
        }
        None => serde_json::json!({ "status": "success" }),
    };
    if stream_mode {
        ndjson_response(vec![
            serde_json::json!({ "status": "pulling manifest" }),
            terminal,
        ])
    } else {
        (StatusCode::OK, Json(terminal)).into_response()
    }
}

/// Build the not-found response for `/api/pull`. Streaming can't carry a 404
/// mid-NDJSON, so it emits an error status line and ends; non-streaming returns
/// a real 404 with the same message.
fn pull_not_found_response(name: &str, stream_mode: bool) -> Response {
    let msg = format!("model '{name}' not found in the configured source model directory");
    if stream_mode {
        ndjson_response(vec![serde_json::json!({ "status": "error", "error": msg })])
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "status": "error", "error": msg })),
        )
            .into_response()
    }
}

fn pull_error_response(message: &str, stream_mode: bool) -> Response {
    if stream_mode {
        ndjson_response(vec![
            serde_json::json!({ "status": "error", "error": message }),
        ])
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "status": "error", "error": message })),
        )
            .into_response()
    }
}

fn pull_unavailable_response(message: &str, stream_mode: bool) -> Response {
    let body = serde_json::json!({ "status": "error", "error": message });
    if stream_mode {
        ndjson_response_with_status(StatusCode::SERVICE_UNAVAILABLE, vec![body])
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

/// Serialize a sequence of JSON values as an `application/x-ndjson` body — one
/// object per line, newline-terminated — the framing Ollama's pull progress
/// uses. Values are owned (not streamed off a worker), so we build the body
/// eagerly rather than through `async_stream`.
fn ndjson_response(lines: Vec<serde_json::Value>) -> Response {
    ndjson_response_with_status(StatusCode::OK, lines)
}

fn ndjson_response_with_status(status: StatusCode, lines: Vec<serde_json::Value>) -> Response {
    let mut buf = Vec::new();
    for value in &lines {
        if let Ok(mut bytes) = serde_json::to_vec(value) {
            bytes.push(b'\n');
            buf.extend_from_slice(&bytes);
        }
    }
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from(buf))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct HttpTerminal {
    completion: Option<Completion>,
    error: Option<String>,
    output_error: Option<String>,
}

impl HttpTerminal {
    fn observe_output_error(&mut self, error: String) {
        if self.output_error.is_none() {
            self.output_error = Some(error);
        }
    }

    fn observe(&mut self, completion: Completion, error: Option<String>) {
        if self.completion.is_some() {
            self.completion = Some(Completion::Error);
            self.error = Some("worker stream emitted multiple terminal events".to_string());
            return;
        }
        self.completion = Some(completion);
        self.error = error;
    }

    fn done_reason(&self) -> &'static str {
        match self.completion.as_ref() {
            Some(Completion::Stop) => "stop",
            Some(Completion::Length) => "length",
            Some(Completion::Cancelled) => "cancelled",
            Some(Completion::Error) | None => "error",
            Some(_) => "unknown",
        }
    }

    fn failure_reason(&self) -> Option<String> {
        if let Some(error) = &self.output_error {
            return Some(error.clone());
        }
        match self.completion.as_ref() {
            None => Some("worker stream ended without a terminal event".to_string()),
            Some(Completion::Error) => Some(
                self.error
                    .clone()
                    .unwrap_or_else(|| "worker execution failed".to_string()),
            ),
            Some(_) => self.error.clone(),
        }
    }
}

/// Validate the workload-specific interpretation of a committed inference
/// chunk before exposing it as Ollama text. The router/receipt pipeline still
/// sees and commits the original bytes; this check only controls whether those
/// bytes may be represented as textual HTTP output.
fn validated_text_chunk<'a>(
    chunk: &'a OutputChunk,
    next_output_seq: &mut u64,
) -> Result<&'a str, String> {
    if chunk.kind != "token" {
        return Err("worker emitted a non-token chunk for text inference".to_string());
    }
    if chunk.seq != *next_output_seq {
        return Err("worker emitted an out-of-sequence text chunk".to_string());
    }
    let text = std::str::from_utf8(&chunk.data)
        .map_err(|_| "worker emitted a non-UTF-8 text chunk".to_string())?;
    *next_output_seq = next_output_seq
        .checked_add(1)
        .ok_or_else(|| "worker text sequence overflowed".to_string())?;
    Ok(text)
}

#[derive(Debug)]
struct FinishedReceipt {
    verification: ReceiptVerification,
    commitment: Option<[u8; 32]>,
}

fn finalized_receipt_verification(
    initial: ReceiptVerification,
    receipt_present: bool,
) -> ReceiptVerification {
    match initial {
        ReceiptVerification::Pending | ReceiptVerification::Verified if receipt_present => {
            ReceiptVerification::Verified
        }
        ReceiptVerification::Pending | ReceiptVerification::Verified => ReceiptVerification::Failed,
        ReceiptVerification::Local => ReceiptVerification::Local,
        ReceiptVerification::Failed => ReceiptVerification::Failed,
        ReceiptVerification::Unverifiable => ReceiptVerification::Unverifiable,
    }
}

async fn finish_http_receipt(handle: JobHandle, initial: ReceiptVerification) -> FinishedReceipt {
    let receipt = handle.finish().await.ok();
    FinishedReceipt {
        verification: finalized_receipt_verification(initial, receipt.is_some()),
        commitment: receipt
            .as_ref()
            .map(|receipt| receipt.result.output_commitment),
    }
}

fn peer_receipt_failure_reason(verification: ReceiptVerification) -> Option<&'static str> {
    match verification {
        ReceiptVerification::Failed => {
            Some("peer receipt verification failed or receipt is missing")
        }
        ReceiptVerification::Unverifiable => Some("peer returned no verifiable receipt"),
        ReceiptVerification::Pending => Some("peer receipt verification did not complete"),
        ReceiptVerification::Local | ReceiptVerification::Verified => None,
    }
}

fn execution_failure_reason(
    terminal: &HttpTerminal,
    verification: ReceiptVerification,
) -> Option<String> {
    terminal
        .failure_reason()
        .or_else(|| peer_receipt_failure_reason(verification).map(str::to_string))
}

fn apply_result_headers(
    resp: &mut Response,
    routed_via: Option<&str>,
    verification: ReceiptVerification,
    route_explanation: Option<&str>,
) {
    if let Some(routed_via) = routed_via {
        if let Ok(value) = routed_via.parse() {
            resp.headers_mut().insert(HEADER_ROUTED_VIA, value);
        }
    }
    if let Some(verification) = verification.header_value() {
        if let Ok(value) = verification.parse() {
            resp.headers_mut().insert(HEADER_RECEIPT_VERIFIED, value);
        }
    }
    if let Some(route_explanation) = route_explanation {
        if let Ok(value) = route_explanation.parse() {
            resp.headers_mut().insert(HEADER_ROUTE_EXPLANATION, value);
        }
    }
}

fn execution_failure_response(
    reason: String,
    routed_via: Option<&str>,
    verification: ReceiptVerification,
    route_explanation: Option<&str>,
) -> Response {
    let status = if verification == ReceiptVerification::Local {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::BAD_GATEWAY
    };
    let mut response = (
        status,
        Json(serde_json::json!({
            "error": reason,
            "done": true,
            "done_reason": "error",
        })),
    )
        .into_response();
    apply_result_headers(&mut response, routed_via, verification, route_explanation);
    response
}

fn annotate_stream_terminal(
    value: &mut serde_json::Value,
    terminal: &HttpTerminal,
    receipt: &FinishedReceipt,
) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if let Some(verification) = receipt.verification.header_value() {
        map.insert(
            "x_lucid_receipt_verified".to_string(),
            serde_json::Value::String(verification.to_string()),
        );
    }
    if let Some(reason) = execution_failure_reason(terminal, receipt.verification) {
        map.insert(
            "done_reason".to_string(),
            serde_json::Value::String("error".to_string()),
        );
        map.insert("error".to_string(), serde_json::Value::String(reason));
    } else if let Some(commitment) = receipt.commitment.as_ref() {
        map.insert(
            "x_phase_commitment".to_string(),
            serde_json::Value::String(hex32(commitment)),
        );
    }
}

/// HTTP-submitted jobs get a short replay window so a captured signed request
/// cannot be dispatched indefinitely. Five minutes fits comfortably inside
/// the manifest verifier's remote-execution maximum.
fn http_manifest<T>(payload: T) -> ManifestBuilder<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    ManifestBuilder::new(payload).expires_at(chrono::Utc::now() + chrono::Duration::minutes(5))
}

fn parse_role(s: &str) -> Option<PhaseChatRole> {
    match s {
        "system" => Some(PhaseChatRole::System),
        "user" => Some(PhaseChatRole::User),
        "assistant" => Some(PhaseChatRole::Assistant),
        "tool" => Some(PhaseChatRole::Tool),
        _ => None,
    }
}

fn rfc3339_now() -> String {
    // Hand-roll an RFC3339-shaped timestamp so we don't drag in `chrono`
    // or `time` just for one field. Ollama clients accept any RFC3339-ish
    // string; they don't parse it.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339_from_unix_parts(now.as_secs(), now.subsec_nanos())
}

fn rfc3339_from_unix_millis(timestamp_ms: u64) -> String {
    rfc3339_from_unix_parts(
        timestamp_ms / 1_000,
        ((timestamp_ms % 1_000) * 1_000_000) as u32,
    )
}

fn rfc3339_from_unix_parts(secs: u64, nanos: u32) -> String {
    let days_since_epoch = secs / 86_400;
    let time_of_day = secs % 86_400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days_since_epoch as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, month, day, h, m, s, nanos
    )
}

/// Convert "days since 1970-01-01" into a (Y, M, D) tuple. Civil calendar
/// algorithm from Howard Hinnant. We need this because we deliberately don't
/// pull in `chrono`/`time` for the spike.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn receipt_header_value(commitment: &[u8; 32]) -> String {
    base64::engine::general_purpose::STANDARD.encode(commitment)
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_chunk(seq: u64, kind: &str, data: impl Into<Bytes>) -> OutputChunk {
        OutputChunk {
            kind: kind.to_string(),
            data: data.into(),
            seq,
        }
    }

    #[test]
    fn http_manifest_is_short_lived_and_remote_valid() {
        let identity = NodeIdentity::generate();
        let job_spec = JobSpec::Embedding(phase_protocol::EmbeddingJobSpec {
            model_cid: "test-model".to_string(),
            input: vec!["hello".to_string()],
        });
        let manifest = http_manifest(job_spec)
            .sign_with(&identity)
            .expect("sign HTTP manifest");

        manifest
            .verify_for_remote_execution()
            .expect("HTTP manifest meets remote policy");
        let ttl = manifest
            .expires_at
            .expect("HTTP manifest has expiry")
            .signed_duration_since(manifest.created_at);
        assert!(ttl > chrono::Duration::minutes(4));
        assert!(ttl <= chrono::Duration::minutes(5));
    }

    #[test]
    fn registry_metadata_uses_real_capabilities_without_fake_zero_fields() {
        let cid = crate::registry::ModelCid([9u8; 32]);
        let mut caps =
            crate::registry::ModelCapabilities::now("qwen3", cid, "Q4_K_M", 32_768, 2, "llama.cpp");
        caps.advertised_at = 0;

        let listing = serde_json::to_value(tag_model_from_caps(None, &caps))
            .expect("serialize registry-backed tag");
        assert_eq!(listing["name"], "qwen3:latest");
        assert_eq!(listing["model"], "qwen3:latest");
        assert_eq!(listing["digest"], format!("sha256:{}", cid.to_hex()));
        assert_ne!(listing["digest"], format!("sha256:{}", "0".repeat(64)));
        assert_eq!(listing["modified_at"], "1970-01-01T00:00:00.000000000Z");
        assert!(
            listing.get("size").is_none(),
            "unknown size must be omitted"
        );
        assert_eq!(listing["details"]["format"], "gguf");
        assert_eq!(listing["details"]["quantization_level"], "Q4_K_M");

        let shown = serde_json::to_value(show_response_from_caps(&caps))
            .expect("serialize registry-backed show response");
        assert_eq!(shown["model_info"]["phase.model_cid"], cid.to_hex());
        assert_eq!(shown["model_info"]["phase.context_length"], 32_768);
        assert!(shown.get("modelfile").is_none());
        assert!(shown.get("template").is_none());
        assert!(shown.get("parameters").is_none());
    }

    #[test]
    fn show_merges_verified_install_metadata_with_loaded_capabilities() {
        let cid = crate::registry::ModelCid([17u8; 32]);
        let installed = crate::registry::InstalledModel {
            model_id: "qwen3".to_string(),
            model_cid: cid,
            format: "gguf".to_string(),
            size_bytes: 42_000,
            installed_at: 123,
        };
        let caps =
            crate::registry::ModelCapabilities::now("qwen3", cid, "Q4_K_M", 32_768, 2, "llama.cpp");

        let shown = serde_json::to_value(show_response_from_installed_and_caps(
            &installed,
            Some(&caps),
        ))
        .expect("serialize merged show response");
        assert_eq!(shown["model_info"]["phase.model_cid"], cid.to_hex());
        assert_eq!(
            shown["model_info"]["phase.verification"],
            "content_verified"
        );
        assert_eq!(shown["model_info"]["phase.size_bytes"], 42_000);
        assert_eq!(shown["model_info"]["phase.backend"], "llama.cpp");
        assert_eq!(shown["model_info"]["phase.context_length"], 32_768);
        assert_eq!(shown["details"]["format"], "gguf");
        assert_eq!(shown["details"]["quantization_level"], "Q4_K_M");
        assert_eq!(shown["capabilities"], serde_json::json!(["completion"]));
    }

    #[test]
    fn ollama_latest_suffix_routes_to_verified_base_alias() {
        assert_eq!(ollama_routing_model_name("echo:latest"), "echo");
        assert_eq!(ollama_routing_model_name("QWEN3:latest"), "qwen3");
        assert_eq!(ollama_routing_model_name("qwen3:q4"), "qwen3:q4");
    }

    #[tokio::test]
    async fn ps_does_not_invent_runtime_residency_records() {
        let Json(response) = handle_ps().await;
        assert!(response.models.is_empty());
    }

    #[test]
    fn unknown_show_and_unconfigured_pull_are_explicit_errors() {
        assert_eq!(
            unknown_model_response("not-loaded").status(),
            StatusCode::NOT_FOUND
        );

        for streaming in [false, true] {
            let response = pull_unavailable_response("model store not configured", streaming);
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            if streaming {
                assert_eq!(
                    response
                        .headers()
                        .get(header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    Some("application/x-ndjson")
                );
            }
        }
    }

    #[test]
    fn live_peer_verification_stays_pending_until_receipt_arrives() {
        assert_eq!(ReceiptVerification::Pending.header_value(), Some("pending"));
        assert_eq!(
            finalized_receipt_verification(ReceiptVerification::Pending, true),
            ReceiptVerification::Verified
        );
        assert_eq!(
            finalized_receipt_verification(ReceiptVerification::Pending, false),
            ReceiptVerification::Failed
        );
        assert_eq!(
            finalized_receipt_verification(ReceiptVerification::Verified, false),
            ReceiptVerification::Failed,
            "a pre-verified route still must surface its delivered receipt"
        );
    }

    #[test]
    fn terminal_error_and_missing_final_are_http_failures() {
        let missing = HttpTerminal::default();
        assert!(missing
            .failure_reason()
            .expect("missing Final fails")
            .contains("without a terminal"));

        let mut errored = HttpTerminal::default();
        errored.observe(Completion::Error, Some("backend exploded".to_string()));
        assert_eq!(
            execution_failure_reason(&errored, ReceiptVerification::Verified).as_deref(),
            Some("backend exploded")
        );

        let mut duplicate = HttpTerminal::default();
        duplicate.observe(Completion::Stop, None);
        duplicate.observe(Completion::Stop, None);
        assert!(duplicate
            .failure_reason()
            .expect("duplicate Final fails")
            .contains("multiple terminal"));
    }

    #[test]
    fn peer_failure_response_is_non_success_with_truthful_header() {
        for (verification, expected_header) in [
            (ReceiptVerification::Failed, "false"),
            (ReceiptVerification::Unverifiable, "unverifiable"),
        ] {
            let response = execution_failure_response(
                "untrusted peer result".to_string(),
                Some("peer:test"),
                verification,
                Some("bounded route summary"),
            );
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                response
                    .headers()
                    .get(HEADER_RECEIPT_VERIFIED)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_header)
            );
            assert!(!response.headers().contains_key("x-phase-receipt"));
        }

        let verified_error = execution_failure_response(
            "verified worker failure".to_string(),
            Some("peer:test"),
            ReceiptVerification::Verified,
            None,
        );
        assert_eq!(verified_error.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            verified_error
                .headers()
                .get(HEADER_RECEIPT_VERIFIED)
                .and_then(|value| value.to_str().ok()),
            Some("true"),
            "receipt validity and execution success are distinct"
        );
    }

    #[test]
    fn stream_terminal_carries_final_verification_or_error() {
        let mut successful_terminal = HttpTerminal::default();
        successful_terminal.observe(Completion::Stop, None);
        let verified_receipt = FinishedReceipt {
            verification: ReceiptVerification::Verified,
            commitment: Some([7u8; 32]),
        };
        let mut successful = serde_json::json!({ "done": true, "done_reason": "stop" });
        annotate_stream_terminal(&mut successful, &successful_terminal, &verified_receipt);
        assert_eq!(successful["x_lucid_receipt_verified"], "true");
        assert_eq!(successful["x_phase_commitment"], hex32(&[7u8; 32]));
        assert!(successful.get("error").is_none());

        let failed_receipt = FinishedReceipt {
            verification: ReceiptVerification::Failed,
            commitment: None,
        };
        let mut failed = serde_json::json!({ "done": true, "done_reason": "stop" });
        annotate_stream_terminal(&mut failed, &successful_terminal, &failed_receipt);
        assert_eq!(failed["x_lucid_receipt_verified"], "false");
        assert_eq!(failed["done_reason"], "error");
        assert!(failed["error"].as_str().unwrap().contains("receipt"));
        assert!(failed.get("x_phase_commitment").is_none());

        let mut errored_terminal = HttpTerminal::default();
        errored_terminal.observe(Completion::Error, Some("worker failed".to_string()));
        let mut verified_error = serde_json::json!({ "done": true, "done_reason": "error" });
        annotate_stream_terminal(&mut verified_error, &errored_terminal, &verified_receipt);
        assert_eq!(verified_error["x_lucid_receipt_verified"], "true");
        assert_eq!(verified_error["error"], "worker failed");
        assert!(verified_error.get("x_phase_commitment").is_none());
    }

    #[test]
    fn sanitize_strips_crlf_and_ansi() {
        // SEC-10: CRLF (log forging) and ESC/CSI (ANSI abuse) are dropped,
        // yielding a single-line, escape-free string.
        let evil = "/api/\r\nFAKE-LOG-LINE\x1b[31mred\x1b[0m\x00\x7f";
        let s = sanitize_for_log(evil);
        assert!(!s.contains('\r'));
        assert!(!s.contains('\n'));
        assert!(!s.contains('\x1b'));
        assert!(!s.contains('\x00'));
        assert!(!s.contains('\x7f'));
        // Visible content survives.
        assert!(s.contains("/api/"));
        assert!(s.contains("FAKE-LOG-LINE"));
        assert!(s.contains("red"));
    }

    #[test]
    fn sanitize_caps_length() {
        // SEC-10: a 10 KB path is capped at LOG_FIELD_CAP (+ ellipsis).
        let huge = "/".to_string() + &"a".repeat(10_000);
        let s = sanitize_for_log(&huge);
        // chars(): cap content chars plus the trailing ellipsis marker.
        assert!(s.chars().count() <= LOG_FIELD_CAP + 1);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn sanitize_passes_clean_path_through() {
        // SEC-10: a normal path is unchanged.
        let clean = "/api/chat";
        assert_eq!(sanitize_for_log(clean), clean);
    }

    #[test]
    fn embed_input_accepts_single_string() {
        // Ollama's `/api/embed` allows `input` as a bare string; the
        // `untagged` enum should pick `One` and normalize to a 1-element vec.
        let req: EmbedRequest =
            serde_json::from_str(r#"{"model":"m","input":"hello"}"#).expect("parse single");
        let inputs = req.input.expect("input present").into_vec();
        assert_eq!(inputs, vec!["hello".to_string()]);
    }

    #[test]
    fn embed_input_accepts_array_of_strings() {
        // …and as an array; the same enum should pick `Many` and pass it
        // through unchanged, preserving order.
        let req: EmbedRequest =
            serde_json::from_str(r#"{"model":"m","input":["x","y"]}"#).expect("parse array");
        let inputs = req.input.expect("input present").into_vec();
        assert_eq!(inputs, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn embed_input_refuses_unknown_fields() {
        let parsed = serde_json::from_str::<EmbedRequest>(
            r#"{"model":"m","input":"z","future_backend_knob":true}"#,
        );
        assert!(parsed.is_err());
    }

    #[test]
    fn nested_messages_and_legacy_embeddings_refuse_unknown_fields() {
        assert!(serde_json::from_str::<WireMessage>(
            r#"{"role":"user","content":"x","future_field":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<EmbeddingsRequest>(
            r#"{"model":"m","prompt":"x","future_field":true}"#
        )
        .is_err());
    }

    #[test]
    fn text_output_requires_token_kind_contiguous_sequence_and_utf8() {
        let mut next = 0;
        let first = output_chunk(0, "token", Bytes::from_static(b"hello"));
        assert_eq!(validated_text_chunk(&first, &mut next).unwrap(), "hello");
        assert_eq!(next, 1);

        let wrong_kind = output_chunk(1, "embedding", Bytes::from_static(b"[]"));
        assert!(validated_text_chunk(&wrong_kind, &mut next)
            .unwrap_err()
            .contains("non-token"));

        let wrong_seq = output_chunk(2, "token", Bytes::from_static(b"later"));
        assert!(validated_text_chunk(&wrong_seq, &mut next)
            .unwrap_err()
            .contains("out-of-sequence"));

        let non_utf8 = output_chunk(1, "token", Bytes::from_static(&[0xff, 0xfe]));
        assert!(validated_text_chunk(&non_utf8, &mut next)
            .unwrap_err()
            .contains("non-UTF-8"));
        assert_eq!(next, 1, "invalid chunks must not advance the sequence");
    }

    #[test]
    fn embedding_collector_preserves_input_order_and_exact_count() {
        let mut collector = EmbeddingCollector::new(2);
        collector.observe(&output_chunk(
            1,
            "embedding",
            Bytes::from_static(br#"[3.0,4.0]"#),
        ));
        collector.observe(&output_chunk(
            0,
            "embedding",
            Bytes::from_static(br#"[1.0,2.0]"#),
        ));
        assert_eq!(
            collector.finish().unwrap(),
            vec![vec![1.0, 2.0], vec![3.0, 4.0]]
        );

        let missing = EmbeddingCollector::new(1);
        assert!(missing.finish().unwrap_err().contains("fewer"));

        let mut extra = EmbeddingCollector::new(1);
        extra.observe(&output_chunk(
            1,
            "embedding",
            Bytes::from_static(br#"[1.0]"#),
        ));
        assert!(extra.finish().unwrap_err().contains("extra"));

        let mut duplicate = EmbeddingCollector::new(1);
        let vector = output_chunk(0, "embedding", Bytes::from_static(br#"[1.0]"#));
        duplicate.observe(&vector);
        duplicate.observe(&vector);
        assert!(duplicate.finish().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn embedding_collector_rejects_wrong_kind_malformed_and_invalid_vectors() {
        for (chunk, expected) in [
            (
                output_chunk(0, "token", Bytes::from_static(b"text")),
                "non-embedding",
            ),
            (
                output_chunk(0, "embedding", Bytes::from_static(b"not-json")),
                "malformed",
            ),
            (
                output_chunk(0, "embedding", Bytes::from_static(b"[]")),
                "dimension",
            ),
        ] {
            let mut collector = EmbeddingCollector::new(1);
            collector.observe(&chunk);
            assert!(collector.finish().unwrap_err().contains(expected));
        }

        let mut inconsistent = EmbeddingCollector::new(2);
        inconsistent.observe(&output_chunk(
            0,
            "embedding",
            Bytes::from_static(br#"[1.0]"#),
        ));
        inconsistent.observe(&output_chunk(
            1,
            "embedding",
            Bytes::from_static(br#"[1.0,2.0]"#),
        ));
        assert!(inconsistent.finish().unwrap_err().contains("inconsistent"));

        assert!(validate_embedding_values(&[f32::NAN])
            .unwrap_err()
            .contains("non-finite"));
        assert!(validate_embedding_values(&[f32::INFINITY])
            .unwrap_err()
            .contains("non-finite"));
    }

    #[test]
    fn route_explanation_is_bounded_control_safe_and_identifier_redacted() {
        let peer_shaped = "A".repeat(52);
        let raw = format!(
            "local evidence\r\nselected {peer_shaped} \u{1b}[31m{}",
            "x".repeat(400)
        );
        let public = public_route_explanation(&raw);
        assert!(public.is_ascii());
        assert!(public.len() <= MAX_ROUTE_EXPLANATION_BYTES);
        assert!(!public.contains('\r'));
        assert!(!public.contains('\n'));
        assert!(!public.contains('\u{1b}'));
        assert!(!public.contains(&peer_shaped));
        assert!(public.contains("[redacted]"));
        assert!(HeaderValue::from_str(&public).is_ok());
    }

    #[test]
    fn ollama_options_are_bounded_and_signed_into_sampling() {
        let options = serde_json::json!({
            "temperature": 0.7,
            "top_p": 0.9,
            "repeat_penalty": 1.1,
            "seed": 42,
            "stop": ["END"],
            "num_predict": 128,
        });
        let (sampling, max_tokens) = parse_ollama_options(Some(&options)).unwrap();
        assert_eq!(max_tokens, 128);
        assert_eq!(sampling.params["temperature"], "0.7");
        assert_eq!(sampling.params["repetition_penalty"], "1.1");
        assert_eq!(sampling.params["stop"], r#"["END"]"#);
    }

    #[test]
    fn ollama_options_refuse_unknown_unbounded_and_duplicate_controls() {
        for options in [
            serde_json::json!({"future_backend_knob": true}),
            serde_json::json!({"num_predict": 0}),
            serde_json::json!({"num_predict": 8193}),
            serde_json::json!({"repeat_penalty": 1.1, "repetition_penalty": 1.2}),
            serde_json::json!(["not", "an", "object"]),
        ] {
            assert!(
                parse_ollama_options(Some(&options)).is_err(),
                "expected options to be refused: {options}"
            );
        }
    }

    #[test]
    fn request_validators_refuse_ignored_controls_and_unbounded_inputs() {
        let chat: ChatRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "alien", "content": "x"}],
        }))
        .unwrap();
        assert!(validate_chat_request(&chat).is_err());

        let chat_with_image: ChatRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "x", "images": ["abc"]}],
        }))
        .unwrap();
        assert!(validate_chat_request(&chat_with_image).is_err());

        let generate: GenerateRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "prompt": "x", "template": "ignored-before-this-fix"
        }))
        .unwrap();
        assert!(validate_generate_request(&generate).is_err());

        assert!(validate_embedding_input(&vec!["x".to_string(); MAX_EMBED_INPUTS + 1]).is_err());
        assert!(validate_embedding_input(&["x".repeat(MAX_HTTP_INPUT_CHARS + 1)]).is_err());
    }

    #[test]
    fn pull_name_hygiene_accepts_plain_names() {
        // Ordinary model names (the only thing that can resolve to a local
        // GGUF) pass the guard.
        for name in ["qwen3", "llama3.1", "nomic-embed-text", "gpt-oss"] {
            assert!(pull_name_is_safe(name), "expected {name:?} to be allowed");
        }
    }

    #[test]
    fn pull_name_hygiene_rejects_traversal_and_flags() {
        // SEC-04 parity with worker_llama::resolve_model_path: separators,
        // `..`, leading `-`, and empty are all rejected before the filesystem
        // is ever touched.
        for name in [
            "",
            "../etc/passwd",
            "..",
            "foo/bar",
            "foo\\bar",
            "-rf",
            "--flag",
            "a/../b",
        ] {
            assert!(!pull_name_is_safe(name), "expected {name:?} to be rejected");
        }
    }

    #[test]
    fn pull_selection_parses_exact_cid_and_publisher_pins_strictly() {
        let cid = crate::registry::ModelCid([0x7a; 32]);
        let publisher = PeerId::random();
        assert_eq!(
            parse_pull_selection(Some(&cid.to_hex()), Some(&publisher.to_string())).unwrap(),
            PullSelection {
                exact_cid: Some(cid),
                publisher: Some(publisher),
            }
        );
        assert!(parse_pull_selection(Some("sha256:not-canonical"), None).is_err());
        assert!(parse_pull_selection(None, Some("not-a-peer-id")).is_err());
    }
}
