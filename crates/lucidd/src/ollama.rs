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
//! - Anything else under `/api/*` returns 404 — not in spike scope.
//!
//! The full Ollama surface (generate, embed, pull, ps, etc.) is LUCID M4.

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
}

/// HTTP header that flips this request to local-only mode. Honored on
/// every dispatch path — if the model isn't loaded locally we refuse
/// rather than route to a peer.
pub const HEADER_LOCAL_ONLY: &str = "x-lucid-local-only";

/// HTTP response header advertising where the request was actually
/// served. `local` or `peer:<short>`; omitted on Refused.
pub const HEADER_ROUTED_VIA: &str = "x-lucid-routed-via";

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
        // Health check for liveness probes.
        .route("/", get(|| async { "lucidd echo spike: see /api/chat" }))
        // Log everything else so we can see what real clients ask for that
        // we don't (yet) implement. Spike-only — drop before M4.
        .fallback(unknown)
        .with_state(state)
}

async fn unknown(req: axum::http::Request<Body>) -> impl IntoResponse {
    tracing::warn!(method = %req.method(), uri = %req.uri(), "unimplemented endpoint");
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
        tracing::info!(model = %model, reason = %reason, "router refused /api/generate");
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

    let (handle, mut job_stream) = match state.router.execute(&decision, manifest).await {
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
        tracing::info!(model = %model, reason = %reason, "router refused /api/chat");
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

    let (handle, mut job_stream) = match state.router.execute(&decision, manifest).await {
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
    builder
        .body(body)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to build streaming response");
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
