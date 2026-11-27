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
//! - `GET /api/ps` — worker-reported residency, or an empty list without telemetry.
//! - `POST /api/embed` / `POST /api/embeddings` — embedding vectors over the
//!   same router/manifest/receipt pipeline as `/api/chat` (the legacy
//!   `/api/embeddings` is the singular-`prompt` shape Ollama shipped first).
//! - `POST /api/pull` — imports a local GGUF when present, otherwise resolves
//!   signed alias/provider records and performs a resumable verified pull.
//! - Anything else under `/api/*` returns 404 rather than claiming support.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{header, uri::Authority, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use hyper::body::Frame;
use phase_identity::NodeIdentity;
use phase_manifest::ManifestBuilder;
use phase_net::{PeerId, ReachabilityConnection};
use phase_protocol::{
    ChatMessage as PhaseChatMessage, ChatRole as PhaseChatRole, Completion, InferenceJobSpec,
    JobEvent, JobHandle, JobSpec, OutputChunk, SamplingParams,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::content::{ContentError, PullProgress, PullSelection};
use crate::router::{
    find_local_model_by_alias, ReceiptVerification, RedundantCheckResult, RouteDecision, RouteVia,
    Router as LucidRouter, RouterError, SpotCheckResult,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    x_lucid_access: Option<&'static str>,
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

/// A model the worker currently holds in memory. Report optional values
/// only when the worker can measure them or knows its actual eviction deadline.
#[derive(Debug, Serialize)]
pub struct ResidentModel {
    pub name: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_vram: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
struct PsResponse {
    models: Vec<ResidentModel>,
}

/// Obtain a fresh snapshot of local worker residency for each `GET /api/ps`.
pub type ResidentModelsProvider =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Vec<ResidentModel>> + Send + Sync>;

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
    /// Private native-app control; never shared with chat clients.
    pub consumer: Arc<crate::consumer::ConsumerControl>,
    pub discovery: Arc<phase_net::Discovery>,
    pub policy: Arc<crate::policy::PolicyEngine>,
    pub node_config: Arc<crate::node_config::NodeConfigStore>,
    pub cached_probe: Arc<tokio::sync::RwLock<Option<crate::runtime_manager::SystemProbe>>>,
    pub runtime_lock: Arc<tokio::sync::Mutex<()>>,
    pub started_at: tokio::time::Instant,
    /// Optional loopback port for the Tier-1 Trusted-Circle `ShardWorker` stage
    /// bridge (`--shard-stage-bridge-port`). Used only when `cluster_enabled` is
    /// explicitly enabled in `node.toml`.
    pub shard_stage_bridge_port: Option<u16>,
}

impl AppState {
    /// Build the HTTP router with worker-supplied residency telemetry.
    pub fn router_with_resident_models_provider(
        self,
        loopback_bind: bool,
        resident_models_provider: ResidentModelsProvider,
    ) -> Router {
        build_router(self, loopback_bind, Some(resident_models_provider))
    }
}

/// HTTP header that flips this request to local-only mode. Honored on
/// every dispatch path — if the model isn't loaded locally we refuse
/// rather than route to a peer.
pub const HEADER_LOCAL_ONLY: &str = "x-lucid-local-only";

/// HTTP response header advertising where the request was actually
/// served. `local` or `peer`; omitted on Refused.
pub const HEADER_ROUTED_VIA: &str = "x-lucid-routed-via";

/// SEC-05: HTTP response header reporting whether a peer-served job's signed
/// receipt verified and bound to the dispatched job + peer. `true` / `false` /
/// `unverifiable`; omitted on the local path (no peer receipt to assert).
pub const HEADER_RECEIPT_VERIFIED: &str = "x-lucid-receipt-verified";
pub const HEADER_TRUST_LABEL: &str = "x-lucid-trust-label";
pub(crate) const TERMINAL_TRUST_LABEL: &str = "x_lucid_trust_label";

/// Provenance and empirical replay agreement reported by this daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLabel {
    Local,
    RemotePending,
    RemoteReceiptVerified,
    RemoteSpotChecked,
    TrustedCircleSharded,
}

impl TrustLabel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::RemotePending => "remote-pending",
            Self::RemoteReceiptVerified => "remote-receipt-verified",
            Self::RemoteSpotChecked => "remote-spot-checked",
            Self::TrustedCircleSharded => "trusted-circle-sharded",
        }
    }

    pub(crate) fn from_wire(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "remote-pending" => Some(Self::RemotePending),
            "remote-receipt-verified" => Some(Self::RemoteReceiptVerified),
            "remote-spot-checked" => Some(Self::RemoteSpotChecked),
            "trusted-circle-sharded" => Some(Self::TrustedCircleSharded),
            _ => None,
        }
    }
}

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
            header::AUTHORIZATION,
            HeaderName::from_static(HEADER_LOCAL_ONLY),
        ])
        .expose_headers([
            HeaderName::from_static("x-phase-worker"),
            HeaderName::from_static(HEADER_ROUTED_VIA),
            HeaderName::from_static(HEADER_ROUTE_EXPLANATION),
            HeaderName::from_static(HEADER_RECEIPT_VERIFIED),
            HeaderName::from_static(HEADER_TRUST_LABEL),
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

async fn route_job(
    state: &AppState,
    model: &str,
    local_only: bool,
) -> Result<(RouteDecision, String), Box<Response>> {
    let (decision, cid) = state.router.route_with_cid(model, local_only).await;
    if let RouteVia::Refused { reason } = &decision.via {
        tracing::info!(model = %sanitize_for_log(model), "router refused HTTP model request");
        return Err(Box::new(refused_response(reason)));
    }
    let cid = cid.ok_or_else(|| Box::new(refused_response("model CID resolution failed")))?;
    Ok((decision, cid.to_hex()))
}

fn is_loopback_authority(authority: &Authority) -> bool {
    let host = authority.host();
    if !host.eq_ignore_ascii_case("localhost") && !matches!(host, "127.0.0.1" | "[::1]") {
        return false;
    }
    match authority.as_str().strip_prefix(host) {
        Some("") => true,
        Some(suffix) => suffix.starts_with(':') && authority.port_u16().is_some(),
        None => false,
    }
}

async fn require_loopback_host(request: Request, next: Next) -> Response {
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let host_allowed = hosts
        .next()
        .and_then(|host| Authority::try_from(host.as_bytes()).ok())
        .is_some_and(|host| is_loopback_authority(&host));
    if !host_allowed
        || hosts.next().is_some()
        || request
            .uri()
            .authority()
            .is_some_and(|authority| !is_loopback_authority(authority))
    {
        return (StatusCode::FORBIDDEN, "A loopback Host header is required").into_response();
    }
    next.run(request).await
}

pub fn router(state: AppState, loopback_bind: bool) -> Router {
    build_router(state, loopback_bind, None)
}

fn build_router(
    state: AppState,
    loopback_bind: bool,
    resident_models_provider: Option<ResidentModelsProvider>,
) -> Router {
    let app = Router::new()
        .route("/api/chat", post(handle_chat))
        .route("/api/generate", post(handle_generate))
        .route("/api/version", get(handle_version))
        .route("/api/tags", get(handle_tags))
        .route("/api/show", post(handle_show))
        .route(
            "/api/ps",
            get(handle_ps).with_state(resident_models_provider),
        )
        // Embeddings — non-streaming; both the current (`/api/embed`) and the
        // legacy singular-prompt (`/api/embeddings`) request shapes.
        .route("/api/embed", post(handle_embed))
        .route("/api/embeddings", post(handle_embeddings))
        .route("/api/pull", post(handle_pull))
        .route("/api/lucid/status", get(handle_consumer_status))
        .route("/api/lucid/peers", get(handle_control_peers))
        .route("/api/lucid/probe", get(handle_control_probe))
        .route(
            "/api/lucid/probe/refresh",
            post(handle_control_probe_refresh).layer(DefaultBodyLimit::max(1024)),
        )
        .route("/api/lucid/models", get(handle_control_models))
        .route(
            "/api/lucid/provide",
            post(handle_control_provide).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route(
            "/api/lucid/pull",
            post(handle_control_pull).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route(
            "/api/lucid/folders",
            post(handle_control_folders).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route(
            "/api/lucid/settings",
            post(handle_control_settings).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route("/api/lucid/runtime", get(handle_control_runtime))
        .route(
            "/api/lucid/runtime/setup",
            post(handle_control_runtime_setup).layer(DefaultBodyLimit::max(4 * 1024)),
        )
        .route(
            "/api/lucid/enroll",
            post(handle_consumer_enroll).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/lucid/invitations",
            post(handle_create_invitation).layer(DefaultBodyLimit::max(1024)),
        )
        .route("/api/lucid/cluster", get(handle_control_cluster))
        .route(
            "/api/lucid/cluster/plan",
            post(handle_control_cluster_plan).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/lucid/cluster/execute",
            post(handle_control_cluster_execute).layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        // Health check for liveness probes.
        .route("/", get(|| async { "lucidd ready: see /api/version" }))
        // Log unsupported routes while preserving an honest 404 contract.
        .fallback(unknown)
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(loopback_cors())
        .with_state(state);
    if loopback_bind {
        app.layer(middleware::from_fn(require_loopback_host))
    } else {
        app
    }
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
    let mut count = 0usize;
    for ch in input.chars() {
        // Strip C0 controls (< 0x20) and DEL (0x7f). This covers \r, \n,
        // \t, NUL, and the ESC (0x1b) that introduces ANSI sequences.
        if (ch as u32) < 0x20 || ch == '\u{7f}' {
            continue;
        }
        if count >= LOG_FIELD_CAP {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
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

fn public_routed_via(decision: &RouteDecision) -> Option<String> {
    match decision.via {
        RouteVia::Local => Some("local".to_string()),
        RouteVia::TrustedCircleSharded => Some(
            crate::shard_plan::ShardTrustTier::TrustedCircle
                .trust_label()
                .to_string(),
        ),
        RouteVia::Peer { .. } => Some("peer".to_string()),
        RouteVia::Refused { .. } => None,
    }
}

fn public_dispatch_error(error: &RouterError) -> &'static str {
    match error {
        RouterError::Relay(detail) if detail.contains("not authorized") => {
            "Access is not authorized. Join with a valid network invitation."
        }
        RouterError::Relay(_) => "Network model request failed. Check your connection or retry.",
        RouterError::Worker(_) => "Local model execution failed.",
        RouterError::Refused { .. } => "Request refused by operator policy.",
        RouterError::NoLocalWorker => "No eligible model worker is available.",
    }
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

fn authorize_control(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if state.consumer.authorize_bearer(authorization) {
        None
    } else {
        Some(control_response(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": "Local control authorization required."}),
        ))
    }
}

fn control_response(status: StatusCode, value: serde_json::Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn handle_consumer_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state.discovery.reachability_snapshot(),
    )
    .await
    .ok()
    .and_then(|res| res.ok());
    let peer_count = snapshot.as_ref().map(|s| s.connections.len()).unwrap_or(0);
    let active_path = snapshot
        .as_ref()
        .map(|s| format!("{:?}", s.active_path).to_ascii_lowercase())
        .unwrap_or_else(|| "none".to_string());
    let connection = if peer_count > 0 {
        "connected"
    } else if state.started_at.elapsed() < std::time::Duration::from_secs(15) {
        "connecting"
    } else {
        "offline"
    };
    let mut cids = std::collections::HashSet::new();
    for model in state.registry.local_installed_async().await {
        cids.insert(model.model_cid);
    }
    for model in state.registry.local_models_async().await {
        cids.insert(model.model_cid);
    }
    for model in state.registry.cached_network_models().await {
        cids.insert(model.caps.model_cid);
    }
    let node_cfg = state.node_config.snapshot();
    let policy_cfg = state.policy.config();
    let policy_state = state.policy.state();
    let paused_reason = if policy_cfg.manual_pause {
        Some("manual_pause")
    } else if policy_cfg.auto_pause_on_battery && policy_state.on_battery == Some(true) {
        Some("battery")
    } else if let (Some(limit), Some(temp)) = (
        policy_cfg.auto_pause_on_thermal_threshold_c,
        policy_state.temperature_c,
    ) {
        (temp >= limit).then_some("thermal")
    } else {
        None
    };

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "state": connection,
            "peer_count": peer_count,
            "active_path": active_path,
            "model_count": cids.len(),
            "enrollment": state.consumer.enrollment_status(),
            "onboarding_completed": node_cfg.onboarding_completed,
            "providing_enabled": node_cfg.providing_enabled,
            "cluster_enabled": node_cfg.cluster_enabled,
            "providing_models": node_cfg.approved_provide_models,
            "paused_reason": paused_reason,
            "policy": {
                "manual_pause": policy_cfg.manual_pause,
                "auto_pause_on_battery": policy_cfg.auto_pause_on_battery,
                "thermal_limit_c": policy_cfg.auto_pause_on_thermal_threshold_c,
                "max_concurrent_remote_jobs": policy_cfg.max_concurrent_remote_jobs,
            }
        }),
    )
}

#[derive(Serialize)]
struct ControlPeer {
    peer_id: String,
    path: String,
    authorized: bool,
}

fn control_peer_entries(
    state: &AppState,
    connections: Vec<ReachabilityConnection>,
) -> Vec<ControlPeer> {
    let authorized = state.consumer.authorized_circle_peers(&state.policy);
    connections
        .into_iter()
        .map(|connection| ControlPeer {
            peer_id: connection.peer_id.to_string(),
            path: format!("{:?}", connection.path).to_ascii_lowercase(),
            authorized: authorized.contains(&connection.peer_id),
        })
        .collect()
}

async fn handle_control_peers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.discovery.reachability_snapshot(),
    )
    .await
    .ok()
    .and_then(|res| res.ok());

    let (active_path, nat, peers) = match snapshot {
        Some(snap) => (
            format!("{:?}", snap.active_path).to_ascii_lowercase(),
            format!("{:?}", snap.nat).to_ascii_lowercase(),
            control_peer_entries(&state, snap.connections),
        ),
        None => ("none".to_string(), "unknown".to_string(), Vec::new()),
    };

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "peer_count": peers.len(),
            "active_path": active_path,
            "nat": nat,
            "peers": peers,
        }),
    )
}

async fn current_or_fresh_probe(state: &AppState) -> crate::runtime_manager::SystemProbe {
    if let Some(existing) = state.cached_probe.read().await.clone() {
        let cfg = state.node_config.snapshot();
        let mut copy = existing;
        copy.onboarding_completed = cfg.onboarding_completed;
        copy.providing_enabled = cfg.providing_enabled;
        return copy;
    }
    let cfg = state.node_config.snapshot();
    let probe = crate::runtime_manager::probe_system(&cfg).await;
    *state.cached_probe.write().await = Some(probe.clone());
    probe
}

async fn handle_control_probe(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let probe = current_or_fresh_probe(&state).await;
    control_response(StatusCode::OK, serde_json::json!(probe))
}

async fn handle_control_probe_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let cfg = state.node_config.snapshot();
    let probe = crate::runtime_manager::probe_system(&cfg).await;
    *state.cached_probe.write().await = Some(probe.clone());
    control_response(StatusCode::OK, serde_json::json!(probe))
}

async fn handle_control_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let node_cfg = state.node_config.snapshot();
    let probe = current_or_fresh_probe(&state).await;
    let installed = state.registry.local_installed_async().await;
    let loaded = state.registry.local_models_async().await;
    let remote = state
        .registry
        .discover_network_models()
        .await
        .unwrap_or_default();
    let authorized_issuers: std::collections::HashSet<_> = state
        .consumer
        .authorized_issuers()
        .iter()
        .filter_map(|key| crate::registry::peer_id_from_ed25519_pubkey(key).ok())
        .collect();

    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    for m in &installed {
        let cid_hex = m.model_cid.to_hex();
        let providing = node_cfg.providing_enabled
            && (node_cfg
                .approved_provide_models
                .iter()
                .any(|a| a == &m.model_id || a == &cid_hex));
        let peer_count = remote
            .iter()
            .find(|r| r.caps.model_cid == m.model_cid)
            .map(|r| r.providers.len())
            .unwrap_or(0);
        seen_names.insert(m.model_id.to_ascii_lowercase());
        entries.push(serde_json::json!({
            "id": m.model_id,
            "name": m.model_id,
            "cid": cid_hex,
            "source": "local",
            "size_bytes": m.size_bytes,
            "quantization": serde_json::Value::Null,
            "peer_count": peer_count,
            "on_device": true,
            "available_locally": true,
            "providing": providing,
            "can_request": true,
            "fits_hardware": probe.hardware.recommended_max_model_bytes == 0 || m.size_bytes <= probe.hardware.recommended_max_model_bytes,
            "local_path": serde_json::Value::Null,
        }));
    }

    for caps in &loaded {
        if seen_names.contains(&caps.model_id.to_ascii_lowercase()) {
            continue;
        }
        let cid_hex = caps.model_cid.to_hex();
        let providing = node_cfg.providing_enabled
            && node_cfg
                .approved_provide_models
                .iter()
                .any(|a| a == &caps.model_id || a == &cid_hex);
        seen_names.insert(caps.model_id.to_ascii_lowercase());
        entries.push(serde_json::json!({
            "id": caps.model_id,
            "name": caps.model_id,
            "cid": cid_hex,
            "source": "local",
            "size_bytes": 0,
            "quantization": caps.quantization,
            "peer_count": 0,
            "on_device": true,
            "available_locally": true,
            "providing": providing,
            "can_request": true,
            "fits_hardware": true,
            "local_path": serde_json::Value::Null,
        }));
    }

    for disc in &probe.discovered_models {
        if !seen_names.insert(disc.name.to_ascii_lowercase()) {
            continue;
        }
        let providing = node_cfg.providing_enabled
            && node_cfg
                .approved_provide_models
                .iter()
                .any(|a| a == &disc.name);
        let available_locally = node_cfg
            .approved_local_models
            .iter()
            .any(|a| a == &disc.name)
            || providing;
        entries.push(serde_json::json!({
            "id": disc.name,
            "name": disc.name,
            "cid": serde_json::Value::Null,
            "source": disc.source,
            "size_bytes": disc.size_bytes,
            "quantization": disc.quantization,
            "peer_count": 0,
            "on_device": true,
            "available_locally": available_locally,
            "providing": providing,
            "can_request": false,
            "fits_hardware": disc.fits_hardware,
            "local_path": disc.path,
        }));
    }

    for net in &remote {
        let name = net.caps.model_id.clone();
        if !seen_names.insert(name.to_ascii_lowercase()) {
            continue;
        }
        let can_request = net.providers.iter().any(|p| authorized_issuers.contains(p));
        entries.push(serde_json::json!({
            "id": name,
            "name": net.caps.model_id,
            "cid": net.caps.model_cid.to_hex(),
            "source": "network",
            "size_bytes": 0,
            "quantization": net.caps.quantization,
            "peer_count": net.providers.len(),
            "on_device": false,
            "available_locally": can_request,
            "providing": false,
            "can_request": can_request,
            "fits_hardware": true,
            "local_path": serde_json::Value::Null,
        }));
    }

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "models": entries,
            "model_dirs": node_cfg.model_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "providing_enabled": node_cfg.providing_enabled,
            "onboarding_completed": node_cfg.onboarding_completed,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvideToggleRequest {
    model_name: String,
    enable: bool,
    #[serde(default)]
    confirm_permission: bool,
    #[serde(default)]
    make_available_locally: Option<bool>,
    #[serde(default)]
    local_path: Option<String>,
}

async fn handle_control_provide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvideToggleRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let model_name = req.model_name.trim().to_string();
    if model_name.is_empty() || model_name.len() > 256 {
        return control_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Invalid model name."}),
        );
    }
    if req.enable && !req.confirm_permission {
        return control_response(
            StatusCode::PRECONDITION_REQUIRED,
            serde_json::json!({"error": "Explicit user permission (confirm_permission: true) is required before providing a model."}),
        );
    }

    let probe = current_or_fresh_probe(&state).await;
    // Match the approved model to a real read-only discovered file when present
    // (covers Ollama blobs, LM Studio nested dirs, and user-added folders).
    let discovered_match = probe.discovered_models.iter().find(|m| {
        m.name == model_name
            || req
                .local_path
                .as_deref()
                .is_some_and(|p| p.trim() == m.path)
    });

    let updated = match state.node_config.update(|cfg| {
        cfg.onboarding_completed = true;
        if req.enable {
            cfg.providing_enabled = true;
            if !cfg.approved_provide_models.iter().any(|m| m == &model_name) {
                cfg.approved_provide_models.push(model_name.clone());
            }
            if !cfg.approved_local_models.iter().any(|m| m == &model_name) {
                cfg.approved_local_models.push(model_name.clone());
            }
            if let Some(found) = discovered_match {
                let path = std::path::PathBuf::from(&found.path);
                if path.is_absolute()
                    && !cfg
                        .approved_model_sources
                        .iter()
                        .any(|s| s.name == model_name)
                {
                    cfg.approved_model_sources
                        .push(crate::node_config::ApprovedModelSource {
                            name: model_name.clone(),
                            path,
                            source: found.source.clone(),
                        });
                }
            }
        } else {
            cfg.approved_provide_models.retain(|m| m != &model_name);
            cfg.approved_model_sources.retain(|s| s.name != model_name);
            if cfg.approved_provide_models.is_empty() {
                cfg.providing_enabled = false;
            }
        }
        if let Some(local) = req.make_available_locally {
            if local {
                if !cfg.approved_local_models.iter().any(|m| m == &model_name) {
                    cfg.approved_local_models.push(model_name.clone());
                }
            } else {
                cfg.approved_local_models.retain(|m| m != &model_name);
            }
        }
        Ok(())
    }) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(error = %err, "failed to persist provide permission");
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "Could not save model permission."}),
            );
        }
    };

    // Sync PolicyEngine allowlist immediately so remote authorization matches
    // user-approved models with zero restart lag.
    let serve_list = updated.approved_provide_models.clone();
    let pause_remote = !updated.providing_enabled;
    if let Err(err) = state.policy.update_config(|pol| {
        pol.serve_models = serve_list;
        pol.manual_pause = pause_remote;
    }) {
        tracing::warn!(error = %err, "failed to sync policy after provide toggle");
    }

    // Hot-import into the content-addressed store when this daemon already has
    // an active store/worker so newly approved Ollama/LM-Studio/local models
    // become verifiable immediately.
    let mut imported_cid: Option<String> = None;
    if req.enable {
        if let (Some(found), Some(store), Some(verified_dir)) = (
            discovered_match,
            state.artifact_store.clone(),
            state.verified_model_dir.clone(),
        ) {
            let src_path = std::path::PathBuf::from(&found.path);
            if let Some(parent) = src_path.parent().map(std::path::Path::to_path_buf) {
                match state
                    .registry
                    .import_verified_gguf_file(
                        store,
                        parent,
                        src_path,
                        verified_dir,
                        &model_name,
                        8192,
                        1,
                        "llama.cpp",
                    )
                    .await
                {
                    Ok(caps) => {
                        imported_cid = Some(caps.model_cid.to_hex());
                    }
                    Err(err) => {
                        tracing::warn!(model = %sanitize_for_log(&model_name), error = %err, "hot-import of approved local model deferred");
                    }
                }
            }
        }
    }

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "providing_enabled": updated.providing_enabled,
            "approved_provide_models": updated.approved_provide_models,
            "imported_cid": imported_cid,
            "runtime_ready": probe.runtime.status == "ready",
            "restart_required": req.enable && state.model_dir.is_none() && probe.runtime.status == "ready",
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlPullRequest {
    model: String,
    #[serde(default)]
    cid: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
}

async fn handle_control_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ControlPullRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let Some(plane) = state.content_plane.clone() else {
        return control_response(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error": "Content store is not enabled on this node."}),
        );
    };
    let selection = match parse_pull_selection(req.cid.as_deref(), req.publisher.as_deref()) {
        Ok(sel) => sel,
        Err(err) => {
            return control_response(StatusCode::BAD_REQUEST, serde_json::json!({"error": err}));
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<PullProgress>(64);
    let drain = tokio::spawn(async move {
        let mut last = None;
        while let Some(progress) = rx.recv().await {
            last = Some(progress);
        }
        last
    });
    let result = plane.pull_selected(&req.model, selection, Some(tx)).await;
    let _ = drain.await;
    match result {
        Ok(installed) => control_response(
            StatusCode::OK,
            serde_json::json!({
                "status": "installed",
                "model_id": installed.model_id,
                "cid": installed.model_cid.to_hex(),
                "size_bytes": installed.size_bytes,
            }),
        ),
        Err(err) => control_response(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({"error": format!("Model download failed: {err}")}),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderActionRequest {
    action: String,
    path: String,
}

async fn handle_control_folders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FolderActionRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let path = std::path::PathBuf::from(req.path.trim());
    let updated = match req.action.as_str() {
        "add" => {
            let valid = match crate::node_config::validate_model_dir(&path) {
                Ok(p) => p,
                Err(err) => {
                    return control_response(
                        StatusCode::BAD_REQUEST,
                        serde_json::json!({"error": err.to_string()}),
                    );
                }
            };
            state.node_config.update(|cfg| {
                if !cfg.model_dirs.contains(&valid) {
                    cfg.model_dirs.push(valid);
                }
                Ok(())
            })
        }
        "remove" => state.node_config.update(|cfg| {
            cfg.model_dirs.retain(|p| p != &path);
            Ok(())
        }),
        _ => {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "action must be 'add' or 'remove'"}),
            );
        }
    };

    let cfg = match updated {
        Ok(c) => c,
        Err(err) => {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };

    let probe = crate::runtime_manager::probe_system(&cfg).await;
    *state.cached_probe.write().await = Some(probe.clone());
    control_response(
        StatusCode::OK,
        serde_json::json!({
            "model_dirs": cfg.model_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "probe": probe,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsUpdateRequest {
    #[serde(default)]
    onboarding_completed: Option<bool>,
    #[serde(default)]
    manual_pause: Option<bool>,
    #[serde(default)]
    auto_pause_on_battery: Option<bool>,
    #[serde(default)]
    thermal_limit_c: Option<Option<f32>>,
    #[serde(default)]
    serve_model_content: Option<bool>,
    #[serde(default)]
    cluster_enabled: Option<bool>,
}

async fn handle_control_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SettingsUpdateRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    if let Some(Some(t)) = req.thermal_limit_c {
        if !(40.0..=105.0).contains(&t) {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "Thermal limit must be between 40°C and 105°C."}),
            );
        }
    }

    let node_cfg = match state.node_config.update(|cfg| {
        if let Some(done) = req.onboarding_completed {
            cfg.onboarding_completed = done;
        }
        if let Some(serve_content) = req.serve_model_content {
            cfg.serve_model_content = serve_content;
        }
        if let Some(cluster) = req.cluster_enabled {
            cfg.cluster_enabled = cluster;
        }
        Ok(())
    }) {
        Ok(cfg) => cfg,
        Err(err) => {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };

    let pol_cfg = match state.policy.update_config(|pol| {
        if let Some(pause) = req.manual_pause {
            pol.manual_pause = pause;
        }
        if let Some(bat) = req.auto_pause_on_battery {
            pol.auto_pause_on_battery = bat;
        }
        if let Some(thermal) = req.thermal_limit_c {
            pol.auto_pause_on_thermal_threshold_c = thermal;
        }
    }) {
        Ok(cfg) => cfg,
        Err(err) => {
            return control_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "onboarding_completed": node_cfg.onboarding_completed,
            "serve_model_content": node_cfg.serve_model_content,
            "cluster_enabled": node_cfg.cluster_enabled,
            "policy": {
                "manual_pause": pol_cfg.manual_pause,
                "auto_pause_on_battery": pol_cfg.auto_pause_on_battery,
                "thermal_limit_c": pol_cfg.auto_pause_on_thermal_threshold_c,
            }
        }),
    )
}

async fn handle_control_runtime(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let probe = current_or_fresh_probe(&state).await;
    control_response(
        StatusCode::OK,
        serde_json::json!({
            "hardware": probe.hardware,
            "runtime": probe.runtime,
        }),
    )
}

async fn handle_control_cluster(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let node_cfg = state.node_config.snapshot();
    let probe = current_or_fresh_probe(&state).await;
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state.discovery.reachability_snapshot(),
    )
    .await
    .ok()
    .and_then(|res| res.ok());

    let connected_peers = snapshot
        .map(|snap| control_peer_entries(&state, snap.connections))
        .unwrap_or_default();

    let discovered_clone = probe.discovered_models.clone();
    let inspectable_models: Vec<serde_json::Value> = tokio::task::spawn_blocking(move || {
        discovered_clone
            .into_iter()
            .map(|m| {
                let gguf =
                    crate::shard_plan::inspect_gguf_shard_metadata(std::path::Path::new(&m.path))
                        .ok();
                serde_json::json!({
                    "name": m.name,
                    "path": m.path,
                    "size_bytes": m.size_bytes,
                    "source": m.source,
                    "fits_local": m.fits_hardware,
                    "architecture": gguf.as_ref().map(|g| g.architecture.clone()),
                    "total_layers": gguf.as_ref().map(|g| g.block_count),
                })
            })
            .collect()
    })
    .await
    .unwrap_or_default();

    let local_memory = probe
        .hardware
        .gpu_vram_bytes
        .unwrap_or(probe.hardware.recommended_max_model_bytes);

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "cluster_enabled": node_cfg.cluster_enabled,
            "shard_stage_bridge_configured": state.shard_stage_bridge_port.is_some(),
            "trust_tier": "trusted-circle",
            "trust_label": crate::shard_plan::ShardTrustTier::TrustedCircle.trust_label(),
            "untrusted_sharding_allowed": false,
            "local_node": {
                "peer_id": state.discovery.local_peer_id().to_string(),
                "verifying_key_hex": hex::encode(state.client_identity.peer_id_bytes()),
                "available_memory_bytes": local_memory,
                "backend": probe.hardware.gpu_backend,
                "summary": probe.hardware.summary,
            },
            "connected_peers": connected_peers,
            "models": inspectable_models,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterPlanRequest {
    model_name: String,
    #[serde(default)]
    model_path: Option<String>,
    #[serde(default)]
    model_cid_hex: Option<String>,
    #[serde(default)]
    total_layers: Option<u32>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    trust_tier: Option<crate::shard_plan::ShardTrustTier>,
    peers: Vec<crate::shard_plan::PeerShardCapacity>,
}

async fn handle_control_cluster_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ClusterPlanRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    let tier = req
        .trust_tier
        .unwrap_or(crate::shard_plan::ShardTrustTier::TrustedCircle);
    if tier != crate::shard_plan::ShardTrustTier::TrustedCircle {
        return control_response(
            StatusCode::FORBIDDEN,
            serde_json::json!({
                "error": "Untrusted sharded execution is disabled pending partial-compute verification ADR approval."
            }),
        );
    }

    let alias = req.model_name.trim();
    if alias.is_empty() || alias.len() > 160 {
        return control_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Invalid model_name."}),
        );
    }

    let cid_hex = req
        .model_cid_hex
        .clone()
        .unwrap_or_else(|| crate::registry::ModelCid::development_name_hash(alias).to_hex());

    // Prefer exact GGUF header metadata when an approved local model path is provided.
    let spec = if let Some(path_raw) = req.model_path.as_deref() {
        let probe = current_or_fresh_probe(&state).await;
        let allowed = probe
            .discovered_models
            .iter()
            .any(|m| m.path == path_raw.trim());
        if !allowed {
            return control_response(
                StatusCode::FORBIDDEN,
                serde_json::json!({"error": "model_path must match a read-only discovered local model."}),
            );
        }
        match crate::shard_plan::inspect_gguf_shard_metadata(std::path::Path::new(path_raw.trim()))
        {
            Ok(meta) => crate::shard_plan::shard_spec_from_gguf(cid_hex, alias.to_string(), &meta),
            Err(err) => {
                return control_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": format!("Could not read GGUF header: {err}")}),
                );
            }
        }
    } else {
        let total_layers = req.total_layers.unwrap_or(80);
        let size_bytes = req.size_bytes.unwrap_or(42 * 1024 * 1024 * 1024);
        let synthetic_meta = crate::shard_plan::GgufShardMetadata {
            architecture: "llama".to_string(),
            block_count: total_layers,
            embedding_length: None,
            file_size_bytes: size_bytes,
        };
        crate::shard_plan::shard_spec_from_gguf(cid_hex, alias.to_string(), &synthetic_meta)
    };

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);

    let signed = match crate::shard_plan::plan_pipeline_shards(
        &state.client_identity,
        &spec,
        &req.peers,
        tier,
        now_ms,
        30 * 60 * 1000,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };
    if let Err(err) = crate::shard_plan::verify_shard_plan(&signed, now_ms) {
        return control_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": format!("Plan self-verification failed: {err}")}),
        );
    }

    control_response(
        StatusCode::OK,
        serde_json::json!({
            "verified": true,
            "required_memory_bytes": spec.total_required_bytes(),
            "signed_plan": signed,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterExecuteRequest {
    #[serde(default)]
    job_id: Option<String>,
    signed_plan: crate::shard_plan::SignedShardPartitionPlan,
    input_base64: String,
    #[serde(default)]
    per_stage_timeout_ms: Option<u64>,
}

async fn handle_control_cluster_execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ClusterExecuteRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    if !state.node_config.snapshot().cluster_enabled {
        return control_response(
            StatusCode::FORBIDDEN,
            serde_json::json!({
                "error": "Trusted-circle cluster mode is disabled on this node. Enable cluster_enabled in /api/lucid/settings first."
            }),
        );
    }
    if req.signed_plan.plan.trust_tier != crate::shard_plan::ShardTrustTier::TrustedCircle {
        return control_response(
            StatusCode::FORBIDDEN,
            serde_json::json!({
                "error": "Untrusted sharded execution is disabled pending partial-compute verification ADR approval."
            }),
        );
    }

    let job_id = match req.job_id {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty()
                || trimmed.len() > 128
                || !trimmed
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return control_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "Invalid job_id."}),
                );
            }
            trimmed.to_string()
        }
        None => uuid::Uuid::new_v4().to_string(),
    };

    let input_bytes =
        match base64::engine::general_purpose::STANDARD.decode(req.input_base64.trim()) {
            Ok(bytes) => bytes,
            Err(_) => {
                return control_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "input_base64 must be valid standard base64."}),
                );
            }
        };
    if input_bytes.is_empty() || input_bytes.len() > phase_net::SHARD_STAGE_MAX_PAYLOAD_BYTES {
        return control_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "error": format!(
                    "Decoded input size {} outside 1..={} bytes.",
                    input_bytes.len(),
                    phase_net::SHARD_STAGE_MAX_PAYLOAD_BYTES
                )
            }),
        );
    }

    let timeout = std::time::Duration::from_millis(
        req.per_stage_timeout_ms
            .unwrap_or(30_000)
            .clamp(500, 60_000),
    );
    let local_bridge = match state.shard_stage_bridge_port {
        Some(port) => match crate::shard_worker::LoopbackStageBridgeExecutor::new(port, timeout) {
            Ok(exec) => Some(exec),
            Err(err) => {
                return control_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({"error": format!("Invalid local stage bridge config: {err}")}),
                );
            }
        },
        None => None,
    };
    let local_exec_ref: Option<&dyn crate::shard_worker::ShardStageExecutor> = local_bridge
        .as_ref()
        .map(|b| b as &dyn crate::shard_worker::ShardStageExecutor);

    match crate::shard_worker::execute_sharded_pipeline(
        &state.discovery,
        &state.client_identity,
        &req.signed_plan,
        &job_id,
        &input_bytes,
        local_exec_ref,
        timeout,
    )
    .await
    {
        Ok(output) => {
            let trust_label = TrustLabel::from_wire(&output.verification.trust_label);
            let mut response = control_response(
                StatusCode::OK,
                serde_json::json!({
                "verified": true,
                "trust_label": output.verification.trust_label,
                "job_id": job_id,
                "output_base64": base64::engine::general_purpose::STANDARD.encode(&output.output_payload),
                "verification": output.verification,
                "receipts": output.receipts,
                }),
            );
            if trust_label == Some(TrustLabel::TrustedCircleSharded) {
                set_trust_label(&mut response, TrustLabel::TrustedCircleSharded);
            }
            response
        }
        Err(err) => control_response(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({
                "verified": false,
                "error": err.to_string(),
            }),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSetupRequest {
    /// `"pin_detected"` or `"download_pinned"`
    action: String,
    #[serde(default)]
    confirm_permission: bool,
}

async fn handle_control_runtime_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RuntimeSetupRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    if !req.confirm_permission {
        return control_response(
            StatusCode::PRECONDITION_REQUIRED,
            serde_json::json!({"error": "Explicit user permission (confirm_permission: true) is required before installing or pinning an inference runtime."}),
        );
    }

    let _guard = state.runtime_lock.lock().await;
    let probe = current_or_fresh_probe(&state).await;

    let result = match req.action.as_str() {
        "pin_detected" => {
            let Some(detected) = probe.runtime.detected_path.as_deref() else {
                return control_response(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"error": "No existing llama-server binary was detected on this machine."}),
                );
            };
            crate::runtime_manager::pin_existing_runtime(std::path::Path::new(detected))
                .await
                .map(|(path, sha)| (path, sha, "pinned".to_string()))
        }
        "download_pinned" => {
            crate::runtime_manager::download_and_install_pinned_runtime(&probe.hardware, None)
                .await
                .map(|(path, sha, asset)| (path, sha, format!("downloaded {}", asset.tag)))
        }
        _ => {
            return control_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": "action must be 'pin_detected' or 'download_pinned'"}),
            );
        }
    };

    let (binary_path, sha256, detail) = match result {
        Ok(ok) => ok,
        Err(err) => {
            tracing::warn!(error = %err, "runtime setup failed");
            return control_response(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };

    let updated = match state.node_config.update(|cfg| {
        cfg.llama_server_binary = Some(binary_path.clone());
        cfg.llama_server_sha256 = Some(sha256.clone());
        Ok(())
    }) {
        Ok(cfg) => cfg,
        Err(err) => {
            return control_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": err.to_string()}),
            );
        }
    };

    let refreshed = crate::runtime_manager::probe_system(&updated).await;
    *state.cached_probe.write().await = Some(refreshed.clone());
    control_response(
        StatusCode::OK,
        serde_json::json!({
            "detail": detail,
            "runtime": refreshed.runtime,
        }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollRequest {
    invitation: String,
}

async fn handle_consumer_enroll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EnrollRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    match state.consumer.enroll(&request.invitation).await {
        Ok(status) => control_response(StatusCode::ACCEPTED, serde_json::json!(status)),
        Err(_) => control_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Invitation is invalid, expired, or enrollment is unavailable."}),
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInvitationRequest {
    #[serde(default = "default_invitation_ttl")]
    ttl_seconds: u64,
}

fn default_invitation_ttl() -> u64 {
    15 * 60
}

async fn handle_create_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateInvitationRequest>,
) -> Response {
    if let Some(response) = authorize_control(&state, &headers) {
        return response;
    }
    if state.registry.local_models_async().await.is_empty()
        && state.shard_stage_bridge_port.is_none()
    {
        return control_response(
            StatusCode::CONFLICT,
            serde_json::json!({"error": "This node is not serving models or shard stages."}),
        );
    }
    match state.consumer.create_invitation(request.ttl_seconds).await {
        Ok(invitation) => control_response(StatusCode::CREATED, serde_json::json!(invitation)),
        Err(_) => control_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Could not create invitation. Check its lifetime and pending invitation limit."}),
        ),
    }
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
    let mut models = installed
        .iter()
        .map(tag_model_from_installed)
        .collect::<Vec<_>>();
    for caps in &loaded_only {
        models.push(tag_model_from_caps_async(state.verified_model_dir.as_deref(), caps).await);
    }
    let mut known = installed
        .iter()
        .map(|model| model.model_cid)
        .collect::<std::collections::HashSet<_>>();
    known.extend(loaded_only.iter().map(|caps| caps.model_cid));
    match state.registry.discover_network_models().await {
        Ok(remote) => {
            let authorized = state
                .consumer
                .authorized_issuers()
                .iter()
                .filter_map(|key| crate::registry::peer_id_from_ed25519_pubkey(key).ok())
                .collect::<std::collections::HashSet<_>>();
            for entry in remote {
                if !known.insert(entry.caps.model_cid) {
                    continue;
                }
                let access = if entry.providers.iter().any(|peer| authorized.contains(peer)) {
                    "authorized"
                } else {
                    "access_not_confirmed"
                };
                models.push(tag_model_from_network(&entry, access));
            }
        }
        Err(error) => tracing::debug!(%error, "network model catalog unavailable"),
    }
    models.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.model.cmp(&b.model)));
    Json(TagsResponse { models })
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
        Ok(None) => {
            let requested = ollama_routing_model_name(&requested);
            let cid = crate::registry::ModelCid::from_hex(&requested).ok();
            let candidates = state
                .registry
                .discover_network_models()
                .await
                .unwrap_or_default();
            let matches = candidates
                .iter()
                .filter(|model| {
                    cid == Some(model.caps.model_cid) || model.caps.model_id == requested
                })
                .collect::<Vec<_>>();
            return match matches.as_slice() {
                [model] => Json(show_response_from_caps(&model.caps)).into_response(),
                [] => unknown_model_response(&requested),
                _ => (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "Model name is ambiguous. Choose its exact catalog identifier."
                    })),
                )
                    .into_response(),
            };
        }
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
        x_lucid_access: None,
    }
}

/// The model registry tracks availability. Only the worker snapshot can
/// establish residency for this endpoint.
async fn handle_ps(State(provider): State<Option<ResidentModelsProvider>>) -> Json<PsResponse> {
    let models = match provider {
        Some(provider) => provider().await,
        None => Vec::new(),
    };
    Json(PsResponse { models })
}

async fn tag_model_from_caps_async(
    verified_model_dir: Option<&std::path::Path>,
    caps: &crate::registry::ModelCapabilities,
) -> TagModel {
    let size = match verified_model_dir {
        Some(dir) => tokio::fs::metadata(dir.join(format!("{}.gguf", caps.model_cid.to_hex())))
            .await
            .ok()
            .map(|metadata| metadata.len()),
        None => None,
    };
    tag_model_from_caps_with_size(size, caps)
}

fn tag_model_from_caps_with_size(
    size: Option<u64>,
    caps: &crate::registry::ModelCapabilities,
) -> TagModel {
    let name = ollama_model_name(&caps.model_id);
    TagModel {
        name: name.clone(),
        model: name,
        modified_at: rfc3339_from_unix_millis(caps.advertised_at),
        size,
        digest: format!("sha256:{}", caps.model_cid.to_hex()),
        details: model_details_from_caps(caps),
        x_lucid_access: None,
    }
}

fn tag_model_from_network(model: &crate::registry::NetworkModel, access: &'static str) -> TagModel {
    let mut tag = tag_model_from_caps_with_size(model.size_bytes, &model.caps);
    // Open WebUI uses `model` as the dispatch ID and `name` for display.
    // Bind selection to exact bytes even when publishers reuse a human alias.
    tag.model = model.caps.model_cid.to_hex();
    if let Some(format) = &model.format {
        tag.details.format = Some(format.clone());
    }
    tag.x_lucid_access = Some(access);
    tag
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
            if let Some(caps) = find_local_model_by_alias(&local_models, &normalized) {
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
    if request.keep_alive.is_some() {
        return Err("keep_alive is not supported; contributor policy controls model residency");
    }
    if request.format.is_some() {
        return Err("structured format is not supported");
    }
    if request.tools.is_some() {
        return Err("tools are not supported");
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
    let (decision, model_cid) = match route_job(&state, &model, local_only).await {
        Ok(route) => route,
        Err(response) => return *response,
    };
    let routed_via = public_routed_via(&decision);
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP generate route selected"
    );
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
            return (StatusCode::INTERNAL_SERVER_ERROR, "manifest signing failed").into_response();
        }
    };

    let ((handle, mut job_stream, receipt_verification), spot_check) = match state
        .router
        .execute_with_spot_check(&decision, manifest)
        .await
    {
        Ok(t) => t,
        Err(RouterError::Refused { reason }) => return refused_response(&reason),
        Err(e) => {
            tracing::error!(error = %e, "router dispatch failed (/api/generate)");
            return (StatusCode::INTERNAL_SERVER_ERROR, public_dispatch_error(&e)).into_response();
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
        if let Some(label) =
            completed_trust_label(routed_via.as_deref(), &finished_receipt, spot_check).await
        {
            set_trust_label(&mut resp, label);
        }
        tracing::info!(%job_id, "non-streaming generate complete");
        return resp;
    }

    let model_for_body = model.clone();
    let routed_via_for_stream = routed_via.clone();
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
                                yield Ok::<Frame<Bytes>, std::io::Error>(Frame::data(Bytes::from(bytes)));
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
        let trust_label = stream_terminal_trust_label(
            routed_via_for_stream.as_deref(),
            &mut final_value,
            &terminal,
            &finished_receipt,
            spot_check,
        )
        .await;
        if let Ok(mut bytes) = serde_json::to_vec(&final_value) {
            bytes.push(b'\n');
            yield Ok(Frame::data(Bytes::from(bytes)));
            if let Some(label) = trust_label {
                yield Ok(trust_label_trailer(label));
            }
        }
    };
    inference_stream_response(
        ndjson,
        routed_via.as_deref(),
        &route_explanation,
        receipt_verification,
    )
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
    let (decision, model_cid) = match route_job(&state, &model, local_only).await {
        Ok(route) => route,
        Err(response) => return *response,
    };
    let routed_via = public_routed_via(&decision);
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP chat route selected"
    );
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
            return (StatusCode::INTERNAL_SERVER_ERROR, "manifest signing failed").into_response();
        }
    };

    let ((handle, mut job_stream, receipt_verification), spot_check) = match state
        .router
        .execute_with_spot_check(&decision, manifest)
        .await
    {
        Ok(t) => t,
        Err(RouterError::Refused { reason }) => return refused_response(&reason),
        Err(e) => {
            tracing::error!(error = %e, "router dispatch failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, public_dispatch_error(&e)).into_response();
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
        if let Some(label) =
            completed_trust_label(routed_via.as_deref(), &finished_receipt, spot_check).await
        {
            set_trust_label(&mut resp, label);
        }
        tracing::info!(%job_id, "non-streaming chat complete");
        return resp;
    }

    // ----- streaming path: NDJSON body driven by the JobStream -----------
    let model_for_body = model.clone();
    let routed_via_for_stream = routed_via.clone();
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
                                    yield Ok::<Frame<Bytes>, std::io::Error>(Frame::data(Bytes::from(bytes)));
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
        let trust_label = stream_terminal_trust_label(
            routed_via_for_stream.as_deref(),
            &mut final_value,
            &terminal,
            &finished_receipt,
            spot_check,
        )
        .await;
        if let Ok(mut bytes) = serde_json::to_vec(&final_value) {
            bytes.push(b'\n');
            yield Ok(Frame::data(Bytes::from(bytes)));
            if let Some(label) = trust_label {
                yield Ok(trust_label_trailer(label));
            }
        }
    };

    inference_stream_response(
        ndjson,
        routed_via.as_deref(),
        &route_explanation,
        receipt_verification,
    )
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
    /// `X-Lucid-Routed-Via` value (`local` / `peer`), if any.
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
) -> Result<EmbedOutcome, Box<Response>> {
    let local_only = parse_local_only(headers);

    // Route decision. Refusals short-circuit to 503 before we build a
    // manifest or touch a worker — identical to handle_chat.
    let (decision, model_cid) = route_job(state, model, local_only).await?;
    let routed_via = public_routed_via(&decision);
    let route_explanation = public_route_explanation(&decision.explanation);
    tracing::info!(
        routed_via = routed_via.as_deref().unwrap_or("unknown"),
        route_explanation = %route_explanation,
        "HTTP embedding route selected"
    );
    let expected_output_count = input.len();
    let job_spec = JobSpec::Embedding(phase_protocol::EmbeddingJobSpec { model_cid, input });

    let manifest = match http_manifest(job_spec).sign_with(&state.client_identity) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "manifest signing failed (/api/embed)");
            return Err(Box::new(
                (StatusCode::INTERNAL_SERVER_ERROR, "manifest signing failed").into_response(),
            ));
        }
    };

    let (handle, mut job_stream, receipt_verification) =
        match state.router.execute(&decision, manifest).await {
            Ok(t) => t,
            Err(RouterError::Refused { reason }) => {
                return Err(Box::new(refused_response(&reason)))
            }
            Err(e) => {
                tracing::error!(error = %e, "router dispatch failed (/api/embed)");
                return Err(Box::new(
                    (StatusCode::INTERNAL_SERVER_ERROR, public_dispatch_error(&e)).into_response(),
                ));
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
        return Err(Box::new(execution_failure_response(
            reason,
            routed_via.as_deref(),
            finished_receipt.verification,
            Some(&route_explanation),
        )));
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
        Err(resp) => return *resp,
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
        Err(resp) => return *resp,
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
                            tracing::warn!(error = %error, "peer-to-peer model pull failed");
                            yield Ok::<Bytes, std::convert::Infallible>(json_line(serde_json::json!({
                                "status": "error",
                                "error": public_content_error(&error),
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
            provider: _,
        } => serde_json::json!({
            "status": "downloading", "phase": "downloading",
            "digest": format!("sha256:{cid}"), "completed": completed,
            "total": total,
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

fn public_content_error(error: &ContentError) -> &'static str {
    match error {
        ContentError::UnknownAlias => "Model alias is unknown or has no signed mapping.",
        ContentError::AliasConflict => "Signed model records conflict. Select an exact model CID.",
        ContentError::PinMismatch => "No signed model record matches the requested selection.",
        ContentError::UnsupportedFormat(_) => "The model format is not supported.",
        ContentError::ModelTooLarge { .. } => "The model exceeds the local pull limit.",
        ContentError::NoProviders => "No authenticated provider is available for this model.",
        ContentError::Transfer(_) | ContentError::ProvidersExhausted { .. } => {
            "Model transfer failed. Check your connection or retry."
        }
        ContentError::DeadlineExceeded => "Model transfer exceeded its deadline.",
        ContentError::Verification(_) => "Model content verification or installation failed.",
        ContentError::Cancelled => "Model transfer was cancelled.",
        ContentError::Configuration(_) | ContentError::Registration(_) => {
            "The local model store could not complete the request."
        }
    }
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
        Json(serde_json::json!({ "status": "error", "error": public_content_error(&error) })),
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
    if terminal.output_error.is_none() && terminal.error.is_some() {
        tracing::warn!(error = ?terminal.error, "model execution terminal failed");
        return Some("model execution failed".to_string());
    }
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
        let routed_via = if routed_via.starts_with("peer:") {
            "peer"
        } else {
            routed_via
        };
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

async fn completed_trust_label(
    routed_via: Option<&str>,
    receipt: &FinishedReceipt,
    spot_check: Option<SpotCheckResult>,
) -> Option<TrustLabel> {
    match receipt.verification {
        ReceiptVerification::Local
            if routed_via == Some(TrustLabel::TrustedCircleSharded.as_str()) =>
        {
            Some(TrustLabel::TrustedCircleSharded)
        }
        ReceiptVerification::Local => Some(TrustLabel::Local),
        ReceiptVerification::Verified => {
            // Keep a slow validator from delaying completion indefinitely.
            if let Some(check) = spot_check {
                if matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), check).await,
                    Ok(Ok(RedundantCheckResult::Agreement { commitment }))
                        if receipt.commitment == Some(commitment)
                ) {
                    return Some(TrustLabel::RemoteSpotChecked);
                }
            }
            Some(TrustLabel::RemoteReceiptVerified)
        }
        _ => None,
    }
}

fn set_trust_label(response: &mut Response, label: TrustLabel) {
    response
        .headers_mut()
        .insert(HEADER_TRUST_LABEL, HeaderValue::from_static(label.as_str()));
}

fn trust_label_trailer(label: TrustLabel) -> Frame<Bytes> {
    let mut trailers = HeaderMap::new();
    trailers.insert(HEADER_TRUST_LABEL, HeaderValue::from_static(label.as_str()));
    Frame::trailers(trailers)
}

struct InferenceBody<S>(Pin<Box<S>>);

impl<S: Stream<Item = Result<Frame<Bytes>, std::io::Error>>> axum::body::HttpBody
    for InferenceBody<S>
{
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.get_mut().0.as_mut().poll_next(cx)
    }
}

fn inference_stream_response(
    stream: impl Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send + 'static,
    routed_via: Option<&str>,
    route_explanation: &str,
    verification: ReceiptVerification,
) -> Response {
    let mut response = Response::new(Body::new(InferenceBody(Box::pin(stream))));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert("x-phase-worker", HeaderValue::from_static("lucidd"));
    headers.insert(
        header::TRAILER,
        HeaderValue::from_static(HEADER_TRUST_LABEL),
    );
    let label = if routed_via == Some(TrustLabel::TrustedCircleSharded.as_str()) {
        TrustLabel::TrustedCircleSharded
    } else if verification == ReceiptVerification::Local {
        TrustLabel::Local
    } else {
        TrustLabel::RemotePending
    };
    set_trust_label(&mut response, label);
    apply_result_headers(
        &mut response,
        routed_via,
        verification,
        Some(route_explanation),
    );
    response
}

async fn stream_terminal_trust_label(
    routed_via: Option<&str>,
    value: &mut serde_json::Value,
    terminal: &HttpTerminal,
    receipt: &FinishedReceipt,
    spot_check: Option<SpotCheckResult>,
) -> Option<TrustLabel> {
    if !annotate_stream_terminal(value, terminal, receipt) {
        return None;
    }
    let label = completed_trust_label(routed_via, receipt, spot_check).await?;
    value[TERMINAL_TRUST_LABEL] = serde_json::Value::String(label.as_str().to_string());
    Some(label)
}

fn annotate_stream_terminal(
    value: &mut serde_json::Value,
    terminal: &HttpTerminal,
    receipt: &FinishedReceipt,
) -> bool {
    let Some(map) = value.as_object_mut() else {
        return false;
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
        return false;
    }
    if let Some(commitment) = receipt.commitment.as_ref() {
        map.insert(
            "x_phase_commitment".to_string(),
            serde_json::Value::String(hex32(commitment)),
        );
    }
    true
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
mod tests;
