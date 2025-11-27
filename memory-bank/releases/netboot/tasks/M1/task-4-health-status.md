# Task 4 — Health & Status Endpoints

**Agent**: Runtime Agent
**Estimated**: 1 day

## 4.1 Implement health endpoint

- [ ] Already basic `/health` in Task 1, enhance it:
  ```rust
  #[derive(Serialize)]
  pub struct HealthResponse {
      pub status: String,  // "healthy" or "degraded"
      pub checks: HealthChecks,
  }

  #[derive(Serialize)]
  pub struct HealthChecks {
      pub artifacts_readable: bool,
      pub dht_connected: bool,  // M3
      pub disk_space_ok: bool,
  }

  pub async fn health(State(state): State<SharedState>) -> Json<HealthResponse> {
      let state = state.read().await;

      let artifacts_readable = state.config.artifacts_dir.exists()
          && state.config.artifacts_dir.is_dir();

      let checks = HealthChecks {
          artifacts_readable,
          dht_connected: false,  // TODO: M3
          disk_space_ok: check_disk_space(&state.config.artifacts_dir),
      };

      let status = if checks.artifacts_readable {
          "healthy"
      } else {
          "degraded"
      }.to_string();

      Json(HealthResponse { status, checks })
  }

  fn check_disk_space(path: &Path) -> bool {
      // Check at least 100MB free
      // Platform-specific implementation
      true  // TODO: implement
  }
  ```

**Dependencies**: M1/Task 1
**Output**: Enhanced health endpoint

---

## 4.2 Implement status endpoint

- [ ] Add `/status` endpoint with detailed metrics:
  ```rust
  #[derive(Serialize)]
  pub struct StatusResponse {
      pub provider: ProviderInfo,
      pub artifacts: ArtifactsInfo,
      pub metrics: MetricsInfo,
  }

  #[derive(Serialize)]
  pub struct ProviderInfo {
      pub version: String,
      pub channel: String,
      pub arch: String,
      pub uptime_secs: u64,
      pub peer_id: String,  // libp2p peer ID
  }

  #[derive(Serialize)]
  pub struct ArtifactsInfo {
      pub available: Vec<ArtifactEntry>,
      pub total_size_bytes: u64,
  }

  #[derive(Serialize)]
  pub struct ArtifactEntry {
      pub name: String,
      pub channel: String,
      pub arch: String,
      pub size_bytes: u64,
      pub hash: String,  // SHA256
  }

  #[derive(Serialize)]
  pub struct MetricsInfo {
      pub requests_total: u64,
      pub bytes_served_total: u64,
      pub requests_by_artifact: HashMap<String, u64>,
      pub active_connections: u64,
  }

  pub async fn status(State(state): State<SharedState>) -> Json<StatusResponse> {
      let state = state.read().await;

      // Scan artifacts directory
      let artifacts = scan_artifacts(&state.config.artifacts_dir).await;

      Json(StatusResponse {
          provider: ProviderInfo {
              version: env!("CARGO_PKG_VERSION").to_string(),
              channel: state.config.channel.clone(),
              arch: state.config.arch.clone(),
              uptime_secs: state.started_at.elapsed().as_secs(),
              peer_id: state.peer_id.clone(),
          },
          artifacts,
          metrics: MetricsInfo {
              requests_total: state.requests_served,
              bytes_served_total: state.bytes_served,
              requests_by_artifact: state.requests_by_artifact.clone(),
              active_connections: 0,  // TODO: track
          },
      })
  }
  ```

**Dependencies**: Task 4.1
**Output**: Status endpoint with metrics

---

## 4.3 Add artifact scanning

- [ ] Implement `scan_artifacts`:
  ```rust
  async fn scan_artifacts(base_dir: &Path) -> ArtifactsInfo {
      let mut available = Vec::new();
      let mut total_size = 0u64;

      // Walk directory structure: base/channel/arch/artifact
      for channel in &["stable", "testing"] {
          for arch in &["arm64", "x86_64"] {
              let dir = base_dir.join(channel).join(arch);
              if !dir.exists() {
                  continue;
              }

              for artifact in &["vmlinuz", "initramfs.img", "rootfs.sqfs"] {
                  let path = dir.join(artifact);
                  if path.exists() {
                      if let Ok(metadata) = tokio::fs::metadata(&path).await {
                          let size = metadata.len();
                          total_size += size;

                          // Compute hash (cached in production)
                          let hash = compute_sha256(&path).await
                              .unwrap_or_else(|_| "unknown".to_string());

                          available.push(ArtifactEntry {
                              name: artifact.to_string(),
                              channel: channel.to_string(),
                              arch: arch.to_string(),
                              size_bytes: size,
                              hash,
                          });
                      }
                  }
              }
          }
      }

      ArtifactsInfo {
          available,
          total_size_bytes: total_size,
      }
  }
  ```

**Dependencies**: Task 4.2
**Output**: Artifact scanning

---

## 4.4 Add routes

- [ ] Update server routes:
  ```rust
  let app = Router::new()
      .route("/", get(handlers::index))
      .route("/health", get(handlers::health))
      .route("/status", get(handlers::status))
      // ... artifact routes
  ```

**Dependencies**: Task 4.3
**Output**: Routes added

---

## 4.5 Test health and status

- [ ] Test health endpoint:
  ```bash
  curl http://localhost:8080/health | jq
  # Expected:
  # {
  #   "status": "healthy",
  #   "checks": {
  #     "artifacts_readable": true,
  #     "dht_connected": false,
  #     "disk_space_ok": true
  #   }
  # }
  ```
- [ ] Test status endpoint:
  ```bash
  curl http://localhost:8080/status | jq
  # Expected: Full status with artifacts list and metrics
  ```
- [ ] Test degraded state:
  ```bash
  # Remove artifacts dir
  mv /tmp/artifacts /tmp/artifacts.bak
  curl http://localhost:8080/health | jq
  # Expected: status: "degraded"
  mv /tmp/artifacts.bak /tmp/artifacts
  ```

**Dependencies**: Task 4.4
**Output**: Endpoints tested

---

## Validation Checklist

- [ ] `/health` returns JSON with status and checks
- [ ] `/health` returns "healthy" when all checks pass
- [ ] `/health` returns "degraded" when artifacts missing
- [ ] `/status` returns provider info
- [ ] `/status` returns artifact list with sizes and hashes
- [ ] `/status` returns request metrics
- [ ] Health check suitable for load balancer (200 = healthy)
