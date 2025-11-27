use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use super::artifacts::ArtifactStore;
use super::config::ProviderConfig;
use super::generator::ManifestGenerator;
use super::metrics::{ProviderMetrics, perform_health_check};

/// HTTP provider server for serving boot artifacts
pub struct ProviderServer {
    config: ProviderConfig,
    start_time: Instant,
}

/// Shared application state
#[derive(Clone)]
struct AppState {
    config: ProviderConfig,
    start_time: Instant,
    artifact_store: Arc<ArtifactStore>,
    metrics: Arc<ProviderMetrics>,
    manifest_generator: Arc<ManifestGenerator>,
}

impl ProviderServer {
    /// Create a new provider server with the given configuration
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            start_time: Instant::now(),
        }
    }

    /// Build the axum router with all routes
    fn build_router(&self) -> anyhow::Result<Router> {
        let artifact_store = Arc::new(ArtifactStore::new(self.config.artifacts_dir.clone())?);
        let metrics = Arc::new(ProviderMetrics::new());

        // Create manifest generator (without signing key for now)
        let manifest_generator = Arc::new(ManifestGenerator::new(artifact_store.clone(), None));

        let state = AppState {
            config: self.config.clone(),
            start_time: self.start_time,
            artifact_store,
            metrics,
            manifest_generator,
        };

        Ok(Router::new()
            .route("/", get(info_handler))
            .route("/health", get(health_handler))
            .route("/status", get(status_handler))
            .route("/manifest.json", get(default_manifest_handler))
            .route("/:channel/:arch/manifest.json", get(manifest_handler))
            .route("/:channel/:arch/:artifact", get(artifact_handler))
            .layer(TraceLayer::new_for_http())
            .with_state(Arc::new(state)))
    }

    /// Run the HTTP server
    pub async fn run(self) -> anyhow::Result<()> {
        let bind_addr = self.config.bind_address();
        info!("Starting provider HTTP server on {}", bind_addr);
        info!("Serving artifacts from: {:?}", self.config.artifacts_dir);
        info!("Channel: {}, Architecture: {}", self.config.channel, self.config.arch);

        let router = self.build_router()?;

        let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
        info!("Provider server listening on {}", bind_addr);

        axum::serve(listener, router).await?;

        Ok(())
    }
}

/// Handler for the root "/" endpoint
/// Returns server information as JSON
async fn info_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();

    let info = json!({
        "name": "plasmd-provider",
        "version": env!("CARGO_PKG_VERSION"),
        "channel": state.config.channel,
        "arch": state.config.arch,
        "uptime_seconds": uptime,
    });

    (StatusCode::OK, Json(info))
}

/// Handler for the /health endpoint
/// Returns health check status with 200 (healthy) or 503 (unhealthy)
async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let health = perform_health_check(&state.config.artifacts_dir);

    let status_code = if health.is_healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(health))
}

/// Handler for the /status endpoint
/// Returns detailed server status including metrics
async fn status_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let metrics = state.metrics.snapshot();
    let health = perform_health_check(&state.config.artifacts_dir);

    let status = json!({
        "name": "plasmd-provider",
        "version": env!("CARGO_PKG_VERSION"),
        "channel": state.config.channel,
        "arch": state.config.arch,
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

/// Parse HTTP Range header, returns (start, end) inclusive
/// Supports formats like "bytes=0-1023" and "bytes=1024-"
fn parse_range(header: &str, file_size: u64) -> Option<(u64, u64)> {
    let header = header.strip_prefix("bytes=")?;
    let parts: Vec<&str> = header.split('-').collect();

    match parts.as_slice() {
        [start, ""] => {
            // "bytes=1024-" means from 1024 to end
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

/// Handler for artifact requests: GET /:channel/:arch/:artifact
/// Serves boot artifacts with proper headers and streaming
/// Supports HTTP Range requests for resumable downloads
async fn artifact_handler(
    State(state): State<Arc<AppState>>,
    Path((channel, arch, artifact)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, &'static str)> {
    info!("Artifact request: {}/{}/{}", channel, arch, artifact);

    // Increment request counter
    state.metrics.increment_requests();

    // Get artifact metadata
    let artifact_meta = match state.artifact_store.get_artifact(&channel, &arch, &artifact) {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            warn!("Artifact not found: {}/{}/{}", channel, arch, artifact);
            return Err((StatusCode::NOT_FOUND, "Artifact not found"));
        }
        Err(e) => {
            warn!("Error getting artifact {}/{}/{}: {}", channel, arch, artifact, e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"));
        }
    };

    let file_size = artifact_meta.size_bytes;

    // Check for Range header
    let range_header = headers.get(header::RANGE);

    // Determine content type
    let content_type = match artifact.as_str() {
        name if name.starts_with("kernel") || name == "vmlinuz" || name == "bzImage" => {
            "application/octet-stream"
        }
        name if name.starts_with("initramfs") || name.starts_with("initrd") => {
            "application/octet-stream"
        }
        name if name.ends_with(".img") || name.ends_with(".squashfs") => {
            "application/octet-stream"
        }
        _ => "application/octet-stream",
    };

    // Handle range request
    if let Some(range_value) = range_header {
        let range_str = match range_value.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Err((StatusCode::BAD_REQUEST, "Invalid Range header"));
            }
        };

        match parse_range(range_str, file_size) {
            Some((start, end)) => {
                info!("Range request: bytes {}-{}/{}", start, end, file_size);

                // Open file and seek to start position
                let mut file = match File::open(&artifact_meta.path).await {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("Error opening artifact file {:?}: {}", artifact_meta.path, e);
                        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to open artifact"));
                    }
                };

                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
                    warn!("Error seeking to position {}: {}", start, e);
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to seek in artifact"));
                }

                // Read only the requested range
                let content_length = (end - start + 1) as usize;
                let mut buffer = vec![0u8; content_length];
                if let Err(e) = file.read_exact(&mut buffer).await {
                    warn!("Error reading range: {}", e);
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to read artifact"));
                }

                // Track bytes served (only the range)
                state.metrics.add_bytes_served(content_length as u64);

                // Build 206 Partial Content response
                use axum::http::header::{HeaderName, HeaderValue};
                let mut response_headers = HeaderMap::new();
                response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
                response_headers.insert(
                    header::CONTENT_LENGTH,
                    HeaderValue::from_str(&content_length.to_string()).unwrap()
                );
                response_headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, file_size)).unwrap()
                );
                response_headers.insert(
                    header::ACCEPT_RANGES,
                    HeaderValue::from_static("bytes")
                );
                response_headers.insert(
                    HeaderName::from_static("x-artifact-hash"),
                    HeaderValue::from_str(&artifact_meta.hash).unwrap()
                );

                let mut response = Response::new(Body::from(buffer));
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                *response.headers_mut() = response_headers;
                Ok(response)
            }
            None => {
                warn!("Invalid range request: {}", range_str);
                // Return 416 Range Not Satisfiable
                use axum::http::header::HeaderValue;
                let mut response_headers = HeaderMap::new();
                response_headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", file_size)).unwrap()
                );
                response_headers.insert(
                    header::ACCEPT_RANGES,
                    HeaderValue::from_static("bytes")
                );

                let mut response = Response::new(Body::empty());
                *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                *response.headers_mut() = response_headers;
                Ok(response)
            }
        }
    } else {
        // No Range header - return full file as before
        let file = match File::open(&artifact_meta.path).await {
            Ok(f) => f,
            Err(e) => {
                warn!("Error opening artifact file {:?}: {}", artifact_meta.path, e);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "Failed to open artifact"));
            }
        };

        // Create streaming response
        let stream = ReaderStream::new(file);
        let body = Body::from_stream(stream);

        // Track bytes served
        state.metrics.add_bytes_served(artifact_meta.size_bytes);

        // Build response with headers
        use axum::http::header::{HeaderName, HeaderValue};
        let mut response_headers = HeaderMap::new();
        response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        response_headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&artifact_meta.size_bytes.to_string()).unwrap()
        );
        response_headers.insert(
            header::ACCEPT_RANGES,
            HeaderValue::from_static("bytes")
        );
        response_headers.insert(
            HeaderName::from_static("x-artifact-hash"),
            HeaderValue::from_str(&artifact_meta.hash).unwrap()
        );

        let mut response = Response::new(body);
        *response.headers_mut() = response_headers;
        Ok(response)
    }
}

/// Handler for default manifest: GET /manifest.json
/// Returns manifest for default channel/arch from config
async fn default_manifest_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let channel = &state.config.channel;
    let arch = &state.config.arch;

    info!("Default manifest request for {}/{}", channel, arch);

    // Generate manifest
    let manifest = state
        .manifest_generator
        .generate_signed(channel, arch)
        .map_err(|e| {
            warn!("Error generating manifest for {}/{}: {}", channel, arch, e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate manifest")
        })?;

    Ok((StatusCode::OK, Json(manifest)))
}

/// Handler for channel/arch-specific manifest: GET /:channel/:arch/manifest.json
/// Returns manifest for the specified channel and architecture
async fn manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((channel, arch)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    info!("Manifest request for {}/{}", channel, arch);

    // Generate manifest
    let manifest = state
        .manifest_generator
        .generate_signed(&channel, &arch)
        .map_err(|e| {
            warn!("Error generating manifest for {}/{}: {}", channel, arch, e);
            if e.to_string().contains("No artifacts found") {
                (StatusCode::NOT_FOUND, "No artifacts found for channel/arch")
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate manifest")
            }
        })?;

    Ok((StatusCode::OK, Json(manifest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let config = ProviderConfig::default();
        let server = ProviderServer::new(config);
        assert_eq!(server.config.port, 8080);
    }

    #[test]
    fn test_parse_range_full_range() {
        // Test "bytes=0-1023" format
        let result = parse_range("bytes=0-1023", 10000);
        assert_eq!(result, Some((0, 1023)));
    }

    #[test]
    fn test_parse_range_from_offset() {
        // Test "bytes=1024-" format (from offset to end)
        let result = parse_range("bytes=1024-", 10000);
        assert_eq!(result, Some((1024, 9999)));
    }

    #[test]
    fn test_parse_range_single_byte() {
        // Test requesting a single byte
        let result = parse_range("bytes=100-100", 10000);
        assert_eq!(result, Some((100, 100)));
    }

    #[test]
    fn test_parse_range_last_byte() {
        // Test requesting the last byte
        let result = parse_range("bytes=9999-9999", 10000);
        assert_eq!(result, Some((9999, 9999)));
    }

    #[test]
    fn test_parse_range_invalid_beyond_size() {
        // Test range beyond file size
        let result = parse_range("bytes=10000-10100", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_start_after_end() {
        // Test invalid range where start > end
        let result = parse_range("bytes=1000-100", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_format() {
        // Test invalid format (missing bytes= prefix)
        let result = parse_range("0-1023", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_invalid_format_multiple_dashes() {
        // Test invalid format with multiple dashes
        let result = parse_range("bytes=0-500-1000", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_start_equals_size() {
        // Test start position equals file size (invalid)
        let result = parse_range("bytes=10000-", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_empty_values() {
        // Test invalid empty range values
        let result = parse_range("bytes=-", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_non_numeric() {
        // Test non-numeric values
        let result = parse_range("bytes=abc-def", 10000);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_range_zero_length_file() {
        // Test with zero-length file
        let result = parse_range("bytes=0-0", 0);
        assert_eq!(result, None);
    }

    // TODO: Fix this test - axum response API changed
    // #[tokio::test]
    // async fn test_info_handler() {
    //     let state = Arc::new(AppState {
    //         config: ProviderConfig::default(),
    //         start_time: Instant::now(),
    //     });
    //
    //     let response = info_handler(State(state)).await;
    //     let (status, Json(data)) = response.into_response().into_parts();
    //
    //     assert_eq!(status, StatusCode::OK);
    //     assert_eq!(data["name"], "plasmd-provider");
    //     assert_eq!(data["channel"], "stable");
    // }
}
