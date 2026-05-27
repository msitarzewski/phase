// SPDX-License-Identifier: Apache-2.0

//! HTTP server for distributing Phase artifacts.
//!
//! Two route families are exposed:
//!
//! * **Channel/arch layout** (legacy, byte-identical to the pre-M6 provider):
//!   - `GET /:channel/:arch/:artifact` — fetch a named artifact with full
//!     `Range:` support (HTTP 206 Partial Content, `Content-Range` /
//!     `Accept-Ranges` headers, `X-Artifact-Hash` for integrity).
//!   - `GET /:channel/:arch/manifest.json` — channel/arch manifest, delegated
//!     to a caller-provided [`ManifestProvider`].
//!   - `GET /manifest.json` — default manifest, also via [`ManifestProvider`].
//! * **Content-addressed layout** (new in M6):
//!   - `GET /blobs/:prefix/:filename` — fetch a blob by its SHA-256. The
//!     filename is `<full_hex>.bin`; the prefix is the first two hex chars.
//!     Same Range support as the channel/arch path.
//!
//! Health / status / info endpoints are also exposed:
//! * `GET /` — server info JSON.
//! * `GET /health` — health probe (200 / 503).
//! * `GET /status` — metrics + health.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::artifacts::{ArtifactMeta, ArtifactStore, BlobId};
use crate::config::ArtifactServerConfig;
use crate::metrics::{perform_health_check, ProviderMetrics};

/// Pluggable manifest provider. The daemon side of the artifact-server crate
/// implements this with its `BootManifest`-specific `ManifestGenerator`;
/// future Phase node implementations slot in their own typed manifests the
/// same way.
///
/// Implementors return any `serde_json::Value` — typically a serialized
/// [`phase_manifest::SignedManifest<T>`] or, for the legacy phase-boot wire
/// format, a `BootManifest` value.
pub trait ManifestProvider: Send + Sync + std::fmt::Debug {
    /// Manifest for a given `(channel, arch)` pair. Returning `Err` causes
    /// the server to map to 404 when the message contains
    /// `"No artifacts found"` and 500 otherwise — same behaviour as the
    /// pre-extraction server.
    fn manifest(&self, channel: &str, arch: &str) -> Result<serde_json::Value, anyhow::Error>;

    /// Default channel + arch for the `/manifest.json` route. Returns
    /// `None` to disable the default-manifest endpoint entirely.
    fn defaults(&self) -> Option<(String, String)>;
}

/// Handle returned by [`ArtifactServer::serve_on`]: the bound socket address
/// plus an aborted-on-drop task for the running server.
#[derive(Debug)]
pub struct ServerHandle {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// The address the server actually bound to (useful when the caller
    /// passed port `0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop the server.
    pub fn abort(self) {
        self.task.abort();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// HTTP server for Phase artifacts.
///
/// The server is layout-agnostic: callers can deposit blobs via
/// [`ArtifactServer::add_blob`] (content-addressed) and/or via
/// [`ArtifactServer::add_channel_artifact`] (legacy boot layout) and the
/// HTTP layer will serve both.
#[derive(Debug)]
pub struct ArtifactServer {
    config: ArtifactServerConfig,
    start_time: Instant,
    artifact_store: Arc<ArtifactStore>,
    metrics: Arc<ProviderMetrics>,
    manifest_provider: Option<Arc<dyn ManifestProvider>>,
    info_name: String,
    info_version: String,
}

impl ArtifactServer {
    /// Construct a server with the given configuration. Creates the
    /// artifacts directory if it does not already exist.
    pub fn new(config: ArtifactServerConfig) -> anyhow::Result<Self> {
        let artifact_store = Arc::new(ArtifactStore::new(config.artifacts_dir.clone())?);
        Ok(Self {
            config,
            start_time: Instant::now(),
            artifact_store,
            metrics: Arc::new(ProviderMetrics::new()),
            manifest_provider: None,
            info_name: "phase-artifact-server".to_string(),
            info_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Override the `name` field returned by the `/` info endpoint.
    pub fn with_info_name(mut self, name: impl Into<String>) -> Self {
        self.info_name = name.into();
        self
    }

    /// Override the `version` field returned by the `/` info endpoint.
    pub fn with_info_version(mut self, version: impl Into<String>) -> Self {
        self.info_version = version.into();
        self
    }

    /// Install a [`ManifestProvider`]. Without one, the manifest routes
    /// respond 404.
    pub fn with_manifest_provider(mut self, provider: Arc<dyn ManifestProvider>) -> Self {
        self.manifest_provider = Some(provider);
        self
    }

    /// Bound port from the config.
    pub fn config(&self) -> &ArtifactServerConfig {
        &self.config
    }

    /// Reference to the underlying artifact store. Useful when the caller
    /// needs to materialise artifacts on disk before/after the server is
    /// constructed.
    pub fn store(&self) -> &Arc<ArtifactStore> {
        &self.artifact_store
    }

    /// Reference to the metrics counter snapshot.
    pub fn metrics(&self) -> &Arc<ProviderMetrics> {
        &self.metrics
    }

    /// Write `content` as a content-addressed blob. Returns its [`BlobId`]
    /// (the SHA-256 hex of the content).
    pub async fn add_blob(&self, content: &[u8]) -> anyhow::Result<BlobId> {
        let store = Arc::clone(&self.artifact_store);
        let bytes = content.to_vec();
        tokio::task::spawn_blocking(move || store.add_blob(&bytes)).await?
    }

    /// Write `content` at the legacy channel/arch path.
    pub async fn add_channel_artifact(
        &self,
        channel: &str,
        arch: &str,
        filename: &str,
        content: &[u8],
    ) -> anyhow::Result<ArtifactMeta> {
        let store = Arc::clone(&self.artifact_store);
        let channel = channel.to_string();
        let arch = arch.to_string();
        let filename = filename.to_string();
        let bytes = content.to_vec();
        tokio::task::spawn_blocking(move || {
            store.add_channel_artifact(&channel, &arch, &filename, &bytes)
        })
        .await?
    }

    /// Build the axum router. Public so callers that already own a
    /// `tokio::net::TcpListener` can drive `axum::serve` themselves.
    pub fn build_router(&self) -> Router {
        let state = AppState {
            info_name: self.info_name.clone(),
            info_version: self.info_version.clone(),
            start_time: self.start_time,
            artifact_store: Arc::clone(&self.artifact_store),
            metrics: Arc::clone(&self.metrics),
            manifest_provider: self.manifest_provider.clone(),
            artifacts_dir: self.config.artifacts_dir.clone(),
            default_channel: self
                .manifest_provider
                .as_ref()
                .and_then(|p| p.defaults())
                .map(|(c, _)| c),
            default_arch: self
                .manifest_provider
                .as_ref()
                .and_then(|p| p.defaults())
                .map(|(_, a)| a),
        };

        Router::new()
            .route("/", get(info_handler))
            .route("/health", get(health_handler))
            .route("/status", get(status_handler))
            .route("/manifest.json", get(default_manifest_handler))
            .route("/blobs/:prefix/:filename", get(blob_handler))
            .route("/:channel/:arch/manifest.json", get(manifest_handler))
            .route("/:channel/:arch/:artifact", get(artifact_handler))
            .layer(TraceLayer::new_for_http())
            .with_state(Arc::new(state))
    }

    /// Bind to the supplied socket address and run the server until the
    /// returned [`ServerHandle`] is dropped or aborted. The returned
    /// `local_addr()` reflects the bound port (useful when `addr.port() == 0`).
    pub async fn serve_on(self, addr: SocketAddr) -> anyhow::Result<ServerHandle> {
        let router = self.build_router();
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let actual = listener.local_addr()?;
        info!("phase-artifact-server listening on {}", actual);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(ServerHandle { addr: actual, task })
    }

    /// Run the server using the bind address + port from
    /// [`ArtifactServerConfig`]. Blocks until the listener errors or the
    /// task is cancelled.
    pub async fn run(self) -> anyhow::Result<()> {
        let bind_addr = self.config.bind_address();
        info!("Starting artifact HTTP server on {}", bind_addr);
        info!("Serving artifacts from: {:?}", self.config.artifacts_dir);

        let router = self.build_router();

        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        info!("Provider server listening on {}", bind_addr);

        axum::serve(listener, router).await?;
        Ok(())
    }
}

/// Shared application state for axum handlers.
#[derive(Clone)]
struct AppState {
    info_name: String,
    info_version: String,
    start_time: Instant,
    artifact_store: Arc<ArtifactStore>,
    metrics: Arc<ProviderMetrics>,
    manifest_provider: Option<Arc<dyn ManifestProvider>>,
    artifacts_dir: std::path::PathBuf,
    default_channel: Option<String>,
    default_arch: Option<String>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("info_name", &self.info_name)
            .field("info_version", &self.info_version)
            .field("artifacts_dir", &self.artifacts_dir)
            .field("default_channel", &self.default_channel)
            .field("default_arch", &self.default_arch)
            .finish()
    }
}

async fn info_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let info = json!({
        "name": state.info_name,
        "version": state.info_version,
        "channel": state.default_channel.clone().unwrap_or_default(),
        "arch": state.default_arch.clone().unwrap_or_default(),
        "uptime_seconds": uptime,
    });
    (StatusCode::OK, Json(info))
}

async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let health = perform_health_check(&state.artifacts_dir);
    let status_code = if health.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status_code, Json(health))
}

async fn status_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let metrics = state.metrics.snapshot();
    let health = perform_health_check(&state.artifacts_dir);

    let status = json!({
        "name": state.info_name,
        "version": state.info_version,
        "channel": state.default_channel.clone().unwrap_or_default(),
        "arch": state.default_arch.clone().unwrap_or_default(),
        "uptime_seconds": uptime,
        "health": {
            "status": health.status,
            "artifacts_readable": health.checks.artifacts_readable,
            "disk_space_ok": health.checks.disk_space_ok,
        },
        "metrics": {
            "requests_total": metrics.requests_total,
            "bytes_served_total": metrics.bytes_served_total,
        },
    });

    (StatusCode::OK, Json(status))
}

/// Parse HTTP Range header, returns (start, end) inclusive. Supports
/// `"bytes=0-1023"` and `"bytes=1024-"`.
fn parse_range(header: &str, file_size: u64) -> Option<(u64, u64)> {
    let header = header.strip_prefix("bytes=")?;
    let parts: Vec<&str> = header.split('-').collect();

    match parts.as_slice() {
        [start, ""] => {
            let start: u64 = start.parse().ok()?;
            if start < file_size {
                Some((start, file_size - 1))
            } else {
                None
            }
        }
        [start, end] => {
            let start: u64 = start.parse().ok()?;
            let end: u64 = end.parse().ok()?;
            if start <= end && end < file_size {
                Some((start, end))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Stream an [`ArtifactMeta`] back as an HTTP response. Honours `Range:` and
/// preserves the byte-identical headers from the pre-extraction server.
async fn serve_artifact_with_range(
    meta: ArtifactMeta,
    headers: HeaderMap,
    metrics: &ProviderMetrics,
    content_type: &'static str,
) -> Result<Response, (StatusCode, &'static str)> {
    let file_size = meta.size_bytes;
    let range_header = headers.get(header::RANGE);

    if let Some(range_value) = range_header {
        let range_str = match range_value.to_str() {
            Ok(s) => s,
            Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid Range header")),
        };

        match parse_range(range_str, file_size) {
            Some((start, end)) => {
                info!("Range request: bytes {}-{}/{}", start, end, file_size);

                let mut file = match File::open(&meta.path).await {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("Error opening artifact file {:?}: {}", meta.path, e);
                        return Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to open artifact",
                        ));
                    }
                };

                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                    warn!("Error seeking to position {}: {}", start, e);
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to seek in artifact",
                    ));
                }

                let content_length = (end - start + 1) as usize;
                let mut buffer = vec![0u8; content_length];
                if let Err(e) = file.read_exact(&mut buffer).await {
                    warn!("Error reading range: {}", e);
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to read artifact",
                    ));
                }

                metrics.add_bytes_served(content_length as u64);

                use axum::http::header::{HeaderName, HeaderValue};
                let mut response_headers = HeaderMap::new();
                response_headers
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
                response_headers.insert(
                    header::CONTENT_LENGTH,
                    HeaderValue::from_str(&content_length.to_string()).unwrap(),
                );
                response_headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, file_size))
                        .unwrap(),
                );
                response_headers
                    .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                response_headers.insert(
                    HeaderName::from_static("x-artifact-hash"),
                    HeaderValue::from_str(&meta.hash).unwrap(),
                );

                let mut response = Response::new(Body::from(buffer));
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                *response.headers_mut() = response_headers;
                Ok(response)
            }
            None => {
                warn!("Invalid range request: {}", range_str);
                use axum::http::header::HeaderValue;
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", file_size)).unwrap(),
                );
                response_headers
                    .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));

                let mut response = Response::new(Body::empty());
                *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                *response.headers_mut() = response_headers;
                Ok(response)
            }
        }
    } else {
        let file = match File::open(&meta.path).await {
            Ok(f) => f,
            Err(e) => {
                warn!("Error opening artifact file {:?}: {}", meta.path, e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to open artifact",
                ));
            }
        };

        let stream = ReaderStream::new(file);
        let body = Body::from_stream(stream);

        metrics.add_bytes_served(meta.size_bytes);

        use axum::http::header::{HeaderName, HeaderValue};
        let mut response_headers = HeaderMap::new();
        response_headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        response_headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&meta.size_bytes.to_string()).unwrap(),
        );
        response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response_headers.insert(
            HeaderName::from_static("x-artifact-hash"),
            HeaderValue::from_str(&meta.hash).unwrap(),
        );

        let mut response = Response::new(body);
        *response.headers_mut() = response_headers;
        Ok(response)
    }
}

/// `GET /:channel/:arch/:artifact`.
async fn artifact_handler(
    State(state): State<Arc<AppState>>,
    Path((channel, arch, artifact)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    info!("Artifact request: {}/{}/{}", channel, arch, artifact);
    state.metrics.increment_requests();

    let meta = match state.artifact_store.get_artifact(&channel, &arch, &artifact) {
        Ok(Some(m)) => m,
        Ok(None) => {
            warn!("Artifact not found: {}/{}/{}", channel, arch, artifact);
            return Err((StatusCode::NOT_FOUND, "Artifact not found"));
        }
        Err(e) => {
            warn!("Error getting artifact {}/{}/{}: {}", channel, arch, artifact, e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"));
        }
    };

    serve_artifact_with_range(meta, headers, &state.metrics, "application/octet-stream").await
}

/// `GET /blobs/:prefix/:filename`. The filename must be `<hex>.bin` where
/// `<hex>` is the full 64-char SHA-256 hex; `<prefix>` must match the first
/// two chars of `<hex>`. Mismatches return 404 (never serve a path that
/// doesn't satisfy the content-address invariant).
async fn blob_handler(
    State(state): State<Arc<AppState>>,
    Path((prefix, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    info!("Blob request: /blobs/{}/{}", prefix, filename);
    state.metrics.increment_requests();

    let hex = match filename.strip_suffix(".bin") {
        Some(h) => h,
        None => return Err((StatusCode::NOT_FOUND, "Blob not found")),
    };

    let blob_id = match BlobId::from_hex(hex) {
        Some(id) => id,
        None => return Err((StatusCode::NOT_FOUND, "Blob not found")),
    };

    if blob_id.prefix() != prefix {
        return Err((StatusCode::NOT_FOUND, "Blob not found"));
    }

    let meta = match state.artifact_store.get_blob(&blob_id) {
        Ok(Some(m)) => m,
        Ok(None) => return Err((StatusCode::NOT_FOUND, "Blob not found")),
        Err(e) => {
            warn!("Error reading blob {}: {}", blob_id, e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"));
        }
    };

    serve_artifact_with_range(meta, headers, &state.metrics, "application/octet-stream").await
}

/// `GET /manifest.json`.
async fn default_manifest_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let provider = state
        .manifest_provider
        .as_ref()
        .ok_or((StatusCode::NOT_FOUND, "Manifest provider not configured"))?;
    let (channel, arch) = match provider.defaults() {
        Some(d) => d,
        None => return Err((StatusCode::NOT_FOUND, "No default channel/arch configured")),
    };

    info!("Default manifest request for {}/{}", channel, arch);
    let manifest = provider.manifest(&channel, &arch).map_err(|e| {
        warn!("Error generating manifest for {}/{}: {}", channel, arch, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to generate manifest",
        )
    })?;
    Ok((StatusCode::OK, Json(manifest)))
}

/// `GET /:channel/:arch/manifest.json`.
async fn manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((channel, arch)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    info!("Manifest request for {}/{}", channel, arch);

    let provider = state
        .manifest_provider
        .as_ref()
        .ok_or((StatusCode::NOT_FOUND, "Manifest provider not configured"))?;

    let manifest = provider.manifest(&channel, &arch).map_err(|e| {
        warn!("Error generating manifest for {}/{}: {}", channel, arch, e);
        if e.to_string().contains("No artifacts found") {
            (
                StatusCode::NOT_FOUND,
                "No artifacts found for channel/arch",
            )
        } else {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to generate manifest",
            )
        }
    })?;
    Ok((StatusCode::OK, Json(manifest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = ArtifactServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 8080,
            artifacts_dir: temp.path().to_path_buf(),
        };
        let server = ArtifactServer::new(config).unwrap();
        assert_eq!(server.config.port, 8080);
    }

    #[test]
    fn test_parse_range_full_range() {
        let result = parse_range("bytes=0-1023", 10000);
        assert_eq!(result, Some((0, 1023)));
    }

    #[test]
    fn test_parse_range_from_offset() {
        let result = parse_range("bytes=1024-", 10000);
        assert_eq!(result, Some((1024, 9999)));
    }

    #[test]
    fn test_parse_range_single_byte() {
        let result = parse_range("bytes=100-100", 10000);
        assert_eq!(result, Some((100, 100)));
    }

    #[test]
    fn test_parse_range_last_byte() {
        let result = parse_range("bytes=9999-9999", 10000);
        assert_eq!(result, Some((9999, 9999)));
    }

    #[test]
    fn test_parse_range_invalid_beyond_size() {
        let result = parse_range("bytes=10000-10100", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_start_after_end() {
        let result = parse_range("bytes=1000-100", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_format() {
        let result = parse_range("0-1023", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_format_multiple_dashes() {
        let result = parse_range("bytes=0-500-1000", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_start_equals_size() {
        let result = parse_range("bytes=10000-", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_empty_values() {
        let result = parse_range("bytes=-", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_non_numeric() {
        let result = parse_range("bytes=abc-def", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_zero_length_file() {
        let result = parse_range("bytes=0-0", 0);
        assert_eq!(result, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blob_endpoint_range_request() {
        // End-to-end Range fetch over the /blobs/<prefix>/<hex>.bin route.
        let temp = tempfile::TempDir::new().unwrap();
        let config = ArtifactServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 0,
            artifacts_dir: temp.path().to_path_buf(),
        };
        let server = ArtifactServer::new(config).unwrap();

        // Build a 200-byte payload, deposit it as a blob.
        let mut payload = Vec::with_capacity(200);
        for i in 0u16..200 {
            payload.push((i & 0xFF) as u8);
        }
        let id = server.add_blob(&payload).await.unwrap();

        let handle = server
            .serve_on(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = handle.local_addr().port();

        let url = format!(
            "http://127.0.0.1:{}/blobs/{}/{}.bin",
            port,
            id.prefix(),
            id.as_str()
        );

        // Range bytes=10-50.
        let client = reqwest::Client::new();
        let mut got = None;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(r) = client.get(&url).header("Range", "bytes=10-50").send().await {
                if r.status().as_u16() == 206 {
                    got = Some(r);
                    break;
                }
            }
        }
        let resp = got.expect("blob server did not respond 206");
        assert_eq!(resp.status().as_u16(), 206);
        let cr = resp
            .headers()
            .get("content-range")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(cr, "bytes 10-50/200");
        let body = resp.bytes().await.unwrap();
        assert_eq!(body.len(), 41);
        assert_eq!(&body[..], &payload[10..=50]);

        handle.abort();
    }
}
