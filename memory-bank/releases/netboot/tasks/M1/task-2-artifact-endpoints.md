# Task 2 — Artifact Endpoints

**Agent**: Runtime Agent
**Estimated**: 3 days

## 2.1 Define artifact structure

- [ ] Expected artifact directory layout:
  ```
  /var/lib/plasm/boot-artifacts/
  ├── stable/
  │   ├── arm64/
  │   │   ├── vmlinuz
  │   │   ├── initramfs.img
  │   │   └── rootfs.sqfs
  │   └── x86_64/
  │       ├── vmlinuz
  │       ├── initramfs.img
  │       └── rootfs.sqfs
  └── testing/
      └── arm64/
          ├── vmlinuz
          └── initramfs.img
  ```
- [ ] Document artifact naming conventions
- [ ] Add artifact path resolution to ProviderState

**Dependencies**: M1/Task 1
**Output**: Artifact directory structure documentation

---

## 2.2 Implement file serving handler

- [ ] Add to `daemon/src/provider/handlers.rs`:
  ```rust
  use axum::{
      extract::{Path, State},
      response::{IntoResponse, Response},
      http::{header, StatusCode},
      body::Body,
  };
  use tokio::fs::File;
  use tokio_util::io::ReaderStream;

  pub async fn serve_artifact(
      State(state): State<SharedState>,
      Path((channel, arch, artifact)): Path<(String, String, String)>,
  ) -> Result<Response, StatusCode> {
      let state = state.read().await;

      // Validate artifact name
      let filename = match artifact.as_str() {
          "kernel" => "vmlinuz",
          "initramfs" => "initramfs.img",
          "rootfs" => "rootfs.sqfs",
          _ => return Err(StatusCode::NOT_FOUND),
      };

      // Build path
      let path = state.config.artifacts_dir
          .join(&channel)
          .join(&arch)
          .join(filename);

      // Check file exists
      if !path.exists() {
          tracing::warn!("Artifact not found: {:?}", path);
          return Err(StatusCode::NOT_FOUND);
      }

      // Open file
      let file = File::open(&path).await
          .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

      let metadata = file.metadata().await
          .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

      // Determine content type
      let content_type = match artifact.as_str() {
          "kernel" => "application/octet-stream",
          "initramfs" => "application/gzip",
          "rootfs" => "application/octet-stream",
          _ => "application/octet-stream",
      };

      // Stream file
      let stream = ReaderStream::new(file);
      let body = Body::from_stream(stream);

      Ok(Response::builder()
          .status(StatusCode::OK)
          .header(header::CONTENT_TYPE, content_type)
          .header(header::CONTENT_LENGTH, metadata.len())
          .header("X-Artifact-Name", &artifact)
          .body(body)
          .unwrap())
  }
  ```

**Dependencies**: Task 2.1
**Output**: File serving handler

---

## 2.3 Add routes to server

- [ ] Update `daemon/src/provider/server.rs`:
  ```rust
  let app = Router::new()
      .route("/", get(handlers::index))
      .route("/health", get(handlers::health))
      .route("/:channel/:arch/:artifact", get(handlers::serve_artifact))
      // Convenience routes for default channel
      .route("/kernel", get(handlers::serve_default_kernel))
      .route("/initramfs", get(handlers::serve_default_initramfs))
      .route("/rootfs", get(handlers::serve_default_rootfs))
      .layer(TraceLayer::new_for_http())
      .with_state(self.state);
  ```
- [ ] Implement default artifact handlers that use configured channel/arch

**Dependencies**: Task 2.2
**Output**: Routes configured

---

## 2.4 Add request logging and metrics

- [ ] Update ProviderState to track:
  ```rust
  pub struct ProviderState {
      // ... existing fields
      pub requests_served: AtomicU64,
      pub bytes_served: AtomicU64,
      pub requests_by_artifact: HashMap<String, AtomicU64>,
  }
  ```
- [ ] Add middleware to track requests:
  ```rust
  async fn track_request(
      State(state): State<SharedState>,
      request: Request,
      next: Next,
  ) -> Response {
      let response = next.run(request).await;

      // Update counters
      let mut state = state.write().await;
      state.requests_served += 1;
      // ... track bytes from Content-Length header

      response
  }
  ```

**Dependencies**: Task 2.3
**Output**: Request tracking

---

## 2.5 Implement hash headers

- [ ] Compute SHA256 hash of artifacts on startup (cache in state)
- [ ] Add `X-Content-SHA256` header to responses:
  ```rust
  .header("X-Content-SHA256", &artifact_hash)
  ```
- [ ] This allows clients to verify without downloading manifest first

**Dependencies**: Task 2.4
**Output**: SHA256 headers on responses

---

## 2.6 Test artifact serving

- [ ] Create test artifact directory:
  ```bash
  mkdir -p /tmp/artifacts/stable/arm64
  dd if=/dev/urandom of=/tmp/artifacts/stable/arm64/vmlinuz bs=1M count=10
  dd if=/dev/urandom of=/tmp/artifacts/stable/arm64/initramfs.img bs=1M count=5
  ```
- [ ] Test requests:
  ```bash
  # Full path
  curl -I http://localhost:8080/stable/arm64/kernel
  curl -I http://localhost:8080/stable/arm64/initramfs

  # Default channel
  curl -I http://localhost:8080/kernel

  # Download and verify size
  curl -o /tmp/test-kernel http://localhost:8080/stable/arm64/kernel
  ls -la /tmp/test-kernel
  ```
- [ ] Verify:
  - Correct Content-Type headers
  - Correct Content-Length
  - X-Content-SHA256 header present
  - File contents match

**Dependencies**: Task 2.5
**Output**: Working artifact endpoints

---

## Validation Checklist

- [ ] `GET /:channel/:arch/kernel` returns kernel file
- [ ] `GET /:channel/:arch/initramfs` returns initramfs file
- [ ] `GET /:channel/:arch/rootfs` returns rootfs file
- [ ] `GET /kernel` returns default channel kernel
- [ ] Content-Type headers correct
- [ ] Content-Length headers correct
- [ ] X-Content-SHA256 headers present
- [ ] 404 for missing artifacts
- [ ] 404 for invalid artifact names
- [ ] Request metrics updated
