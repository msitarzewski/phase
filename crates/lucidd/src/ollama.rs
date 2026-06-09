// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ollama-compatible HTTP surface — *spike scope only*.
//!
//! Implements just enough of the Ollama native API for a real client (the
//! `ollama` CLI, `curl`, Open WebUI) to stream tokens off our worker:
//!
//! - `POST /api/chat` — full NDJSON streaming, the load-bearing path.
//! - `GET /api/tags` — list a single fake "echo" model so `ollama list` /
//!   client model pickers don't 404.
//! - `GET /api/version` — clients capability-sniff here on startup.
//! - `POST /api/show` — minimal stub so `ollama show echo` doesn't barf.
//! - `POST /api/embed` / `POST /api/embeddings` — embedding vectors over the
//!   same router/manifest/receipt pipeline as `/api/chat` (the legacy
//!   `/api/embeddings` is the singular-`prompt` shape Ollama shipped first).
//! - `POST /api/pull` — v0.1.1 stub: registers an already-present local GGUF
//!   into the M6 registry. No network download; real content-hashed CIDs are
//!   v0.2.
//! - Anything else under `/api/*` returns 404 — not in spike scope.
//!
//! The remaining Ollama surface (ps, copy, delete, etc.) is later LUCID work.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt;
use phase_identity::NodeIdentity;
use phase_manifest::ManifestBuilder;
use phase_protocol::{
    ChatMessage as PhaseChatMessage, ChatRole as PhaseChatRole, InferenceJobSpec, JobEvent,
    JobSpec, SamplingParams,
};
use serde::{Deserialize, Serialize};

use crate::router::{RouteDecision, RouteVia, Router as LucidRouter, RouterError};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
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
    size: u64,
    digest: String,
    details: TagModelDetails,
}

#[derive(Debug, Serialize)]
struct TagModelDetails {
    parent_model: &'static str,
    format: &'static str,
    family: &'static str,
    families: Vec<&'static str>,
    parameter_size: &'static str,
    quantization_level: &'static str,
}

#[derive(Debug, Deserialize)]
struct ShowRequest {
    #[allow(dead_code)]
    model: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShowResponse {
    modelfile: &'static str,
    parameters: &'static str,
    template: &'static str,
    details: TagModelDetails,
    capabilities: Vec<&'static str>,
}

/// `/api/embed` request. Ollama accepts `input` as either a single string or
/// an array of strings; everything else (`options`, `keep_alive`, `truncate`,
/// `dimensions`, …) is accepted-and-ignored for the v0.1.1 surface.
#[derive(Debug, Deserialize)]
struct EmbedRequest {
    model: String,
    #[serde(default)]
    input: Option<EmbedInput>,
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
struct EmbeddingsRequest {
    model: String,
    #[serde(default)]
    prompt: String,
}

/// `/api/pull` request. Ollama clients send the model name under either
/// `model` or `name` depending on version; `stream` defaults to true.
#[derive(Debug, Deserialize)]
struct PullRequest {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
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
    /// M6 registry — needed by /api/pull to advertise a locally-present model.
    pub registry: std::sync::Arc<crate::registry::ModelRegistry>,
    /// Model directory (llama-cpp mode), for /api/pull to confirm a GGUF is present. None in echo mode.
    pub model_dir: Option<std::path::PathBuf>,
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

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/chat", post(handle_chat))
        .route("/api/generate", post(handle_generate))
        .route("/api/version", get(handle_version))
        .route("/api/tags", get(handle_tags))
        .route("/api/show", post(handle_show))
        // Embeddings — non-streaming; both the current (`/api/embed`) and the
        // legacy singular-prompt (`/api/embeddings`) request shapes.
        .route("/api/embed", post(handle_embed))
        .route("/api/embeddings", post(handle_embeddings))
        // v0.1.1 stub: register an already-present local GGUF (no download).
        .route("/api/pull", post(handle_pull))
        // Health check for liveness probes.
        .route("/", get(|| async { "lucidd echo spike: see /api/chat" }))
        // Log everything else so we can see what real clients ask for that
        // we don't (yet) implement. Spike-only — drop before M4.
        .fallback(unknown)
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
    // Pretend to be a current Ollama. Clients capability-sniff this; they
    // don't gate on exact version strings.
    Json(VersionResponse { version: "0.24.0" })
}

async fn handle_tags() -> Json<TagsResponse> {
    Json(TagsResponse {
        models: vec![echo_model_listing()],
    })
}

async fn handle_show(Json(_req): Json<ShowRequest>) -> Json<ShowResponse> {
    Json(ShowResponse {
        modelfile: "# lucidd echo spike — reverses your input\nFROM scratch\n",
        parameters: "",
        template: "{{ .Prompt }}",
        details: echo_model_details(),
        capabilities: vec!["completion"],
    })
}

fn echo_model_listing() -> TagModel {
    TagModel {
        name: "echo:latest".to_string(),
        model: "echo:latest".to_string(),
        modified_at: rfc3339_now(),
        size: 0,
        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        details: echo_model_details(),
    }
}

fn echo_model_details() -> TagModelDetails {
    TagModelDetails {
        parent_model: "",
        format: "phase-echo",
        family: "echo",
        families: vec!["echo"],
        parameter_size: "0B",
        quantization_level: "none",
    }
}

#[derive(Debug, Deserialize)]
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
    let model = req.model.clone();
    let stream_mode = req.stream.unwrap_or(true);
    let prompt = req.prompt.clone().unwrap_or_default();

    let local_only = parse_local_only(&headers);

    // Route decision. Refusals short-circuit to 503 without ever
    // touching the worker.
    let decision = state.router.route(&model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(&model), reason = %reason, "router refused /api/generate");
        return refused_response(reason);
    }
    let routed_via = decision.header_value();

    let job_spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: req.model.clone(),
        messages: Vec::new(),
        prompt: Some(prompt),
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: None,
        stream: stream_mode,
    });

    // Sign with the AppState identity. Each call's `created_at` differs by
    // wall-clock so successive jobs get distinct manifest hashes (and
    // therefore distinct JobIds) without needing a per-request UUID.
    let manifest = match ManifestBuilder::new(job_spec).sign_with(&state.client_identity) {
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
        let mut done_reason = "stop";
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) => {
                    if let Ok(s) = std::str::from_utf8(&chunk.data) {
                        acc.push_str(s);
                    }
                }
                JobEvent::Final { result, .. } => {
                    done_reason = match result.completion {
                        phase_protocol::Completion::Stop => "stop",
                        phase_protocol::Completion::Length => "length",
                        phase_protocol::Completion::Cancelled => "cancelled",
                        phase_protocol::Completion::Error => "error",
                        _ => "unknown",
                    };
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }
        let receipt_header = match handle.finish().await {
            Ok(r) => Some(receipt_header_value(&r.result.output_commitment)),
            Err(_) => None,
        };
        let total_duration = started_at.elapsed().as_nanos() as u64;
        let body = serde_json::json!({
            "model": model,
            "created_at": rfc3339_now(),
            "response": acc,
            "done": true,
            "done_reason": done_reason,
            "context": [],
            "total_duration": total_duration,
            "load_duration": 0u64,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0u64,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });
        let mut resp = (StatusCode::OK, Json(body)).into_response();
        if let Some(v) = receipt_header {
            if let Ok(hv) = v.parse() {
                resp.headers_mut().insert("X-Phase-Receipt", hv);
            }
        }
        if let Some(rv) = routed_via.as_deref() {
            if let Ok(hv) = rv.parse() {
                resp.headers_mut().insert(HEADER_ROUTED_VIA, hv);
            }
        }
        // SEC-05: surface peer-receipt verification status.
        if let Some(v) = receipt_verification.header_value() {
            if let Ok(hv) = v.parse() {
                resp.headers_mut().insert(HEADER_RECEIPT_VERIFIED, hv);
            }
        }
        tracing::info!(%job_id, "non-streaming generate complete");
        return resp;
    }

    let model_for_body = model.clone();
    let ndjson = stream! {
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut done_reason = "stop";
        let mut commitment: Option<[u8; 32]> = None;

        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) => {
                    let text = match std::str::from_utf8(&chunk.data) {
                        Ok(s) => s.to_string(),
                        Err(_) => base64::engine::general_purpose::STANDARD.encode(&chunk.data),
                    };
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
                JobEvent::Final { result, .. } => {
                    done_reason = match result.completion {
                        phase_protocol::Completion::Stop => "stop",
                        phase_protocol::Completion::Length => "length",
                        phase_protocol::Completion::Cancelled => "cancelled",
                        phase_protocol::Completion::Error => "error",
                        _ => "unknown",
                    };
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    commitment = Some(result.output_commitment);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }
        let total_duration = started_at.elapsed().as_nanos() as u64;
        if let Some(c) = commitment.as_ref() {
            tracing::info!(
                %job_id,
                commitment = %hex32(c),
                "receipt would be signed here (generate)"
            );
        }
        let mut final_value = serde_json::json!({
            "model": &model_for_body,
            "created_at": rfc3339_now(),
            "response": "",
            "done": true,
            "done_reason": done_reason,
            "context": [],
            "total_duration": total_duration,
            "load_duration": 0,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });
        if let Some(c) = commitment.as_ref() {
            if let Some(map) = final_value.as_object_mut() {
                map.insert(
                    "x_phase_commitment".to_string(),
                    serde_json::Value::String(hex32(c)),
                );
            }
        }
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
    // SEC-05: the relay batch (and its receipt) is fully verified inside the
    // router before streaming starts, so the verdict is known here.
    if let Some(v) = receipt_verification.header_value() {
        builder = builder.header(HEADER_RECEIPT_VERIFIED, v);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response())
}

async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let model = req.model.clone();
    let stream_mode = req.stream.unwrap_or(true);

    let local_only = parse_local_only(&headers);

    // Route decision (M5). Refusals short-circuit to 503 before we
    // build a manifest or touch a worker.
    let decision: RouteDecision = state.router.route(&model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(&model), reason = %reason, "router refused /api/chat");
        return refused_response(reason);
    }
    let routed_via = decision.header_value();

    // Translate wire → JobSpec.
    let messages: Vec<PhaseChatMessage> = req
        .messages
        .iter()
        .map(|m| PhaseChatMessage {
            role: parse_role(&m.role),
            content: m.content.clone(),
            images: m.images.clone(),
        })
        .collect();

    let job_spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: req.model.clone(),
        messages,
        prompt: None,
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: None,
        stream: stream_mode,
    });

    // Real signed manifest. M5 swapped the pseudo-manifest UUID for a
    // canonical Ed25519 signature over the job spec; `created_at` carries
    // enough entropy to keep successive JobIds distinct.
    let manifest = match ManifestBuilder::new(job_spec).sign_with(&state.client_identity) {
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
        let mut done_reason = "stop";
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) => {
                    if let Ok(s) = std::str::from_utf8(&chunk.data) {
                        acc.push_str(s);
                    }
                }
                JobEvent::Final { result, .. } => {
                    done_reason = match result.completion {
                        phase_protocol::Completion::Stop => "stop",
                        phase_protocol::Completion::Length => "length",
                        phase_protocol::Completion::Cancelled => "cancelled",
                        phase_protocol::Completion::Error => "error",
                        _ => "unknown",
                    };
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }

        // Retrieve the receipt (commitment surfaced via response header).
        let receipt_header = match handle.finish().await {
            Ok(r) => Some(receipt_header_value(&r.result.output_commitment)),
            Err(_) => None,
        };

        let total_duration = started_at.elapsed().as_nanos() as u64;
        let body = serde_json::json!({
            "model": model,
            "created_at": rfc3339_now(),
            "message": { "role": "assistant", "content": acc },
            "done": true,
            "done_reason": done_reason,
            "total_duration": total_duration,
            "load_duration": 0u64,
            "prompt_eval_count": prompt_tokens,
            "prompt_eval_duration": 0u64,
            "eval_count": completion_tokens,
            "eval_duration": total_duration,
        });

        let mut resp = (StatusCode::OK, Json(body)).into_response();
        if let Some(v) = receipt_header {
            if let Ok(hv) = v.parse() {
                resp.headers_mut().insert("X-Phase-Receipt", hv);
            }
        }
        if let Some(rv) = routed_via.as_deref() {
            if let Ok(hv) = rv.parse() {
                resp.headers_mut().insert(HEADER_ROUTED_VIA, hv);
            }
        }
        // SEC-05: surface peer-receipt verification status.
        if let Some(v) = receipt_verification.header_value() {
            if let Ok(hv) = v.parse() {
                resp.headers_mut().insert(HEADER_RECEIPT_VERIFIED, hv);
            }
        }
        tracing::info!(%job_id, "non-streaming chat complete");
        return resp;
    }

    // ----- streaming path: NDJSON body driven by the JobStream -----------
    let model_for_body = model.clone();
    let ndjson = stream! {
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut done_reason = "stop";
        let mut commitment: Option<[u8; 32]> = None;

        while let Some(ev) = job_stream.next().await {
            match ev {
                JobEvent::Output(chunk) => {
                    let text = match std::str::from_utf8(&chunk.data) {
                        Ok(s) => s.to_string(),
                        Err(_) => base64::engine::general_purpose::STANDARD.encode(&chunk.data),
                    };
                    let payload = ChatChunkResponse {
                        model: &model_for_body,
                        created_at: rfc3339_now(),
                        message: ChatChunkMessage {
                            role: "assistant",
                            content: &text,
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
                JobEvent::Final { result, .. } => {
                    done_reason = match result.completion {
                        phase_protocol::Completion::Stop => "stop",
                        phase_protocol::Completion::Length => "length",
                        phase_protocol::Completion::Cancelled => "cancelled",
                        phase_protocol::Completion::Error => "error",
                        _ => "unknown",
                    };
                    prompt_tokens = result.metrics.prompt_tokens;
                    completion_tokens = result.metrics.completion_tokens;
                    commitment = Some(result.output_commitment);
                }
                JobEvent::Progress(_) => {}
                _ => {}
            }
        }

        // Trailer-free protocol: Ollama clients don't read trailers, so we
        // bake commitment surfacing into a log line + an in-band annotation
        // on the final NDJSON object. (X-Phase-Receipt is set as a response
        // header only on the non-streaming path; on the streaming path
        // headers are flushed before the first byte of body, so the
        // commitment isn't known yet.)
        let total_duration = started_at.elapsed().as_nanos() as u64;
        if let Some(c) = commitment.as_ref() {
            tracing::info!(
                %job_id,
                commitment = %hex32(c),
                "receipt would be signed here"
            );
        }

        let final_payload = ChatFinalResponse {
            model: &model_for_body,
            created_at: rfc3339_now(),
            message: ChatChunkMessage { role: "assistant", content: "" },
            done: true,
            done_reason,
            total_duration,
            load_duration: 0,
            prompt_eval_count: prompt_tokens,
            prompt_eval_duration: 0,
            eval_count: completion_tokens,
            eval_duration: total_duration,
        };
        let mut final_value = serde_json::to_value(&final_payload).unwrap_or(serde_json::json!({}));
        if let Some(c) = commitment.as_ref() {
            if let Some(map) = final_value.as_object_mut() {
                map.insert(
                    "x_phase_commitment".to_string(),
                    serde_json::Value::String(hex32(c)),
                );
            }
        }
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
    // SEC-05: peer-receipt verdict is known before streaming starts.
    if let Some(v) = receipt_verification.header_value() {
        builder = builder.header(HEADER_RECEIPT_VERIFIED, v);
    }
    builder
        .body(body)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to build streaming response");
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

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
    let decision: RouteDecision = state.router.route(model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        // SEC-10: model is attacker-controlled (request body); sanitize.
        tracing::info!(model = %sanitize_for_log(model), reason = %reason, "router refused /api/embed");
        return Err(refused_response(reason));
    }
    let routed_via = decision.header_value();

    let job_spec = JobSpec::Embedding(phase_protocol::EmbeddingJobSpec {
        model_cid: model.to_string(),
        input,
    });

    let manifest = match ManifestBuilder::new(job_spec).sign_with(&state.client_identity) {
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

    // Collect every "embedding"-kind Output chunk keyed by seq, then sort so
    // the output order matches the input order. Non-embedding chunks (none are
    // expected on this path) are ignored.
    let mut keyed: Vec<(u64, Vec<f32>)> = Vec::new();
    while let Some(ev) = job_stream.next().await {
        match ev {
            JobEvent::Output(chunk) => {
                if chunk.kind != "embedding" {
                    continue;
                }
                match serde_json::from_slice::<Vec<f32>>(&chunk.data) {
                    Ok(vector) => keyed.push((chunk.seq, vector)),
                    Err(e) => {
                        tracing::error!(%job_id, error = %e, seq = chunk.seq, "embedding chunk failed to decode");
                    }
                }
            }
            JobEvent::Progress(_) | JobEvent::Final { .. } => {}
            _ => {}
        }
    }
    keyed.sort_by_key(|(seq, _)| *seq);
    let vectors: Vec<Vec<f32>> = keyed.into_iter().map(|(_, v)| v).collect();

    // Surface the signed receipt (commitment) the same way handle_chat does.
    let receipt = match handle.finish().await {
        Ok(r) => Some(receipt_header_value(&r.result.output_commitment)),
        Err(_) => None,
    };

    tracing::info!(%job_id, count = vectors.len(), "embedding job complete");

    Ok(EmbedOutcome {
        vectors,
        routed_via,
        receipt_verified: receipt_verification.header_value(),
        receipt,
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
    let model = req.model.clone();
    let input = req.input.map(EmbedInput::into_vec).unwrap_or_default();

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
// Pull (v0.1.1 stub)
// ---------------------------------------------------------------------------

/// SEC-04-style name hygiene for `/api/pull`: reject anything that could
/// escape `model_dir` or be mistaken for a flag, matching `worker_llama`'s
/// `resolve_model_path` guard (separators, `..`, leading `-`, empty). We keep
/// the simple `dir.join("<name>.gguf").is_file()` shape the task calls for
/// rather than the full canonicalize-and-confine resolver, but the *name*
/// hygiene is identical so a hostile name never reaches the filesystem.
fn pull_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.starts_with('-')
}

/// `/api/pull` — **v0.1.1 stub**. This does NOT download anything over the
/// network. It registers an *already-present* local GGUF into the M6 registry
/// so the router's "local has model" check resolves for it. Pulling from a
/// network with real content-hashed CIDs (SHA-256 of the weights blob) is
/// v0.2; until then the deterministic `ModelCid::from_model_id` placeholder
/// stands in (see `registry.rs`).
///
/// Behavior by mode:
/// - llama-cpp mode (`model_dir` is `Some`): succeed iff `<name>.gguf` exists
///   in the directory; advertise it (idempotent) and report success. A missing
///   file is an error.
/// - echo mode (`model_dir` is `None`): nothing to pull, so succeed without
///   advertising.
///
/// Respects Ollama's `stream` flag (default true): streaming emits NDJSON
/// status lines (`{"status":"pulling manifest"}` → `{"status":"success"}`);
/// `stream:false` returns a single JSON object. On a not-found in streaming
/// mode we can't change the status code mid-NDJSON, so we emit an error status
/// line and end the stream; non-streaming returns 404.
async fn handle_pull(
    State(state): State<AppState>,
    Json(req): Json<PullRequest>,
) -> Response {
    let stream_mode = req.stream.unwrap_or(true);
    let name = match req.model.or(req.name) {
        Some(n) => n,
        None => {
            // No name at all — treat like a not-found so clients get a clear
            // signal rather than a silent success.
            return pull_not_found_response("", stream_mode);
        }
    };

    // Decide success/failure up front; the body shape then depends on `stream`.
    let ok = match &state.model_dir {
        Some(dir) => {
            if !pull_name_is_safe(&name) {
                // SEC-10: name is attacker-controlled; sanitize before logging.
                tracing::info!(model = %sanitize_for_log(&name), "rejected unsafe /api/pull name");
                false
            } else if dir.join(format!("{name}.gguf")).is_file() {
                // Present locally: register it (idempotent) so the router can
                // serve it. CID is the v0.1 name-derived placeholder.
                let cid = crate::registry::ModelCid::from_model_id(&name);
                let caps = crate::registry::ModelCapabilities::now(
                    &name,
                    cid,
                    "unknown",
                    8192,
                    1,
                    "llama.cpp",
                );
                if let Err(e) = state.registry.advertise_loaded(caps).await {
                    tracing::warn!(model = %sanitize_for_log(&name), error = %e, "advertise_loaded failed during /api/pull");
                    // Advertisement is best-effort; the GGUF is still present.
                    // Report success so the client proceeds — the next request
                    // re-resolves locally regardless.
                }
                true
            } else {
                false
            }
        }
        // Echo mode: nothing to pull. Treat as success without advertising.
        None => true,
    };

    if ok {
        pull_success_response(stream_mode)
    } else {
        pull_not_found_response(&name, stream_mode)
    }
}

/// Build the success response for `/api/pull` in the requested transport.
fn pull_success_response(stream_mode: bool) -> Response {
    if stream_mode {
        ndjson_response(vec![
            serde_json::json!({ "status": "pulling manifest" }),
            serde_json::json!({ "status": "success" }),
        ])
    } else {
        (StatusCode::OK, Json(serde_json::json!({ "status": "success" }))).into_response()
    }
}

/// Build the not-found response for `/api/pull`. Streaming can't carry a 404
/// mid-NDJSON, so it emits an error status line and ends; non-streaming returns
/// a real 404 with the same message.
fn pull_not_found_response(name: &str, stream_mode: bool) -> Response {
    let msg = format!(
        "model '{name}' not found in model_dir; v0.1 /api/pull only registers an already-present local GGUF"
    );
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

/// Serialize a sequence of JSON values as an `application/x-ndjson` body — one
/// object per line, newline-terminated — the framing Ollama's pull progress
/// uses. Values are owned (not streamed off a worker), so we build the body
/// eagerly rather than through `async_stream`.
fn ndjson_response(lines: Vec<serde_json::Value>) -> Response {
    let mut buf = Vec::new();
    for value in &lines {
        if let Ok(mut bytes) = serde_json::to_vec(value) {
            bytes.push(b'\n');
            buf.extend_from_slice(&bytes);
        }
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from(buf))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "response build failure").into_response()
        })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_role(s: &str) -> PhaseChatRole {
    match s {
        "system" => PhaseChatRole::System,
        "assistant" => PhaseChatRole::Assistant,
        "tool" => PhaseChatRole::Tool,
        _ => PhaseChatRole::User,
    }
}

fn rfc3339_now() -> String {
    // Hand-roll an RFC3339-shaped timestamp so we don't drag in `chrono`
    // or `time` just for one field. Ollama clients accept any RFC3339-ish
    // string; they don't parse it.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();

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
        let req: EmbedRequest = serde_json::from_str(r#"{"model":"m","input":["x","y"]}"#)
            .expect("parse array");
        let inputs = req.input.expect("input present").into_vec();
        assert_eq!(inputs, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn embed_input_ignores_unknown_fields() {
        // Extra fields Ollama may send (options, truncate, dimensions, …) are
        // accepted-and-ignored, not a deserialization error.
        let req: EmbedRequest = serde_json::from_str(
            r#"{"model":"m","input":"z","truncate":true,"options":{"x":1}}"#,
        )
        .expect("parse with extras");
        assert_eq!(req.model, "m");
        assert_eq!(req.input.expect("input").into_vec(), vec!["z".to_string()]);
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
            assert!(
                !pull_name_is_safe(name),
                "expected {name:?} to be rejected"
            );
        }
    }
}
