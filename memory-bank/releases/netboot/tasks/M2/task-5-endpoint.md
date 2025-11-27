# Task 5 — Manifest Endpoint

**Agent**: Runtime Agent
**Estimated**: 1 day

## 5.1 Add manifest handler

- [ ] Add to `handlers.rs`:
  ```rust
  pub async fn serve_manifest(
      State(state): State<SharedState>,
      Path((channel, arch)): Path<(String, String)>,
  ) -> Result<Response, StatusCode> {
      let mut state = state.write().await;

      // Get or generate manifest (cache for 5 minutes)
      let cached = state.get_manifest(&channel, &arch, 300).await
          .map_err(|e| {
              tracing::error!("Manifest generation error: {}", e);
              StatusCode::INTERNAL_SERVER_ERROR
          })?;

      Ok(Response::builder()
          .status(StatusCode::OK)
          .header(header::CONTENT_TYPE, "application/json")
          .header(header::CACHE_CONTROL, "max-age=300")
          .header("X-Manifest-Version", &cached.manifest.version)
          .header("X-Manifest-Channel", &channel)
          .header("X-Manifest-Arch", &arch)
          .body(Body::from(cached.json.clone()))
          .unwrap())
  }

  /// Serve manifest for default channel/arch
  pub async fn serve_default_manifest(
      State(state): State<SharedState>,
  ) -> Result<Response, StatusCode> {
      let config = {
          let state = state.read().await;
          (state.config.channel.clone(), state.config.arch.clone())
      };

      serve_manifest(State(state), Path(config)).await
  }
  ```

**Dependencies**: M2/Task 4
**Output**: Manifest handler

---

## 5.2 Add routes

- [ ] Update `server.rs`:
  ```rust
  let app = Router::new()
      .route("/", get(handlers::index))
      .route("/health", get(handlers::health))
      .route("/status", get(handlers::status))
      // Manifest endpoints
      .route("/manifest.json", get(handlers::serve_default_manifest))
      .route("/:channel/:arch/manifest.json", get(handlers::serve_manifest))
      // Artifact endpoints
      .route("/:channel/:arch/:artifact", get(handlers::serve_artifact))
      .route("/kernel", get(handlers::serve_default_kernel))
      .route("/initramfs", get(handlers::serve_default_initramfs))
      .route("/rootfs", get(handlers::serve_default_rootfs))
      .layer(TraceLayer::new_for_http())
      .with_state(self.state);
  ```

**Dependencies**: Task 5.1
**Output**: Routes added

---

## 5.3 Add manifest refresh endpoint

- [ ] For operators to force manifest regeneration:
  ```rust
  pub async fn refresh_manifest(
      State(state): State<SharedState>,
      Path((channel, arch)): Path<(String, String)>,
  ) -> StatusCode {
      let mut state = state.write().await;
      state.invalidate_manifest(&channel, &arch);

      // Regenerate
      match state.get_manifest(&channel, &arch, 0).await {
          Ok(_) => StatusCode::OK,
          Err(e) => {
              tracing::error!("Manifest refresh error: {}", e);
              StatusCode::INTERNAL_SERVER_ERROR
          }
      }
  }
  ```
- [ ] Add route: `.route("/:channel/:arch/manifest/refresh", post(handlers::refresh_manifest))`

**Dependencies**: Task 5.2
**Output**: Refresh endpoint

---

## 5.4 Test manifest endpoint

- [ ] Manual tests:
  ```bash
  # Get default manifest
  curl http://localhost:8080/manifest.json | jq

  # Get specific channel/arch
  curl http://localhost:8080/stable/arm64/manifest.json | jq

  # Check headers
  curl -I http://localhost:8080/manifest.json

  # Force refresh
  curl -X POST http://localhost:8080/stable/arm64/manifest/refresh
  ```
- [ ] Verify:
  - Manifest is valid JSON
  - Contains all artifacts
  - Has signature
  - Cache-Control header present
  - X-Manifest-* headers present

**Dependencies**: Task 5.3
**Output**: Endpoint tests

---

## 5.5 Add ETag support

- [ ] Generate ETag from manifest hash:
  ```rust
  use sha2::{Sha256, Digest};

  fn manifest_etag(json: &str) -> String {
      let mut hasher = Sha256::new();
      hasher.update(json.as_bytes());
      let hash = hasher.finalize();
      format!("\"{}\"", hex::encode(&hash[..8]))  // First 8 bytes
  }

  pub async fn serve_manifest(/* ... */) -> Result<Response, StatusCode> {
      // ... get manifest ...

      let etag = manifest_etag(&cached.json);

      Ok(Response::builder()
          .status(StatusCode::OK)
          .header(header::CONTENT_TYPE, "application/json")
          .header(header::ETAG, &etag)
          // ... other headers
          .body(Body::from(cached.json.clone()))
          .unwrap())
  }
  ```
- [ ] Handle If-None-Match for 304 Not Modified

**Dependencies**: Task 5.4
**Output**: ETag support

---

## Validation Checklist

- [ ] `/manifest.json` returns default channel manifest
- [ ] `/:channel/:arch/manifest.json` returns specific manifest
- [ ] Response is valid JSON
- [ ] Response has correct Content-Type
- [ ] Cache-Control header present
- [ ] ETag header present
- [ ] Refresh endpoint regenerates manifest
- [ ] 404 for invalid channel/arch
