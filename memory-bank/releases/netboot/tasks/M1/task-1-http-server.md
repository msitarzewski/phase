# Task 1 — HTTP Server Integration

**Agent**: Runtime Agent
**Estimated**: 3 days

## 1.1 Add dependencies

- [ ] Update `daemon/Cargo.toml`:
  ```toml
  [dependencies]
  axum = "0.7"
  tower = "0.4"
  tower-http = { version = "0.5", features = ["fs", "trace"] }
  ```
- [ ] Verify compatible with existing tokio version
- [ ] Run `cargo build` to ensure no conflicts

**Dependencies**: None
**Output**: Updated Cargo.toml, successful build

---

## 1.2 Create provider module structure

- [ ] Create `daemon/src/provider/mod.rs`:
  ```rust
  pub mod server;
  pub mod handlers;
  pub mod state;

  pub use server::ProviderServer;
  pub use state::ProviderState;
  ```
- [ ] Create `daemon/src/provider/state.rs`:
  ```rust
  use std::path::PathBuf;
  use std::sync::Arc;
  use tokio::sync::RwLock;

  #[derive(Debug, Clone)]
  pub struct ProviderConfig {
      pub port: u16,
      pub artifacts_dir: PathBuf,
      pub channel: String,
      pub arch: String,
  }

  #[derive(Debug)]
  pub struct ProviderState {
      pub config: ProviderConfig,
      pub started_at: std::time::Instant,
      pub requests_served: u64,
      pub bytes_served: u64,
  }

  pub type SharedState = Arc<RwLock<ProviderState>>;
  ```
- [ ] Export from `daemon/src/lib.rs`:
  ```rust
  pub mod provider;
  ```

**Dependencies**: Task 1.1
**Output**: Provider module structure

---

## 1.3 Implement basic HTTP server

- [ ] Create `daemon/src/provider/server.rs`:
  ```rust
  use axum::{Router, routing::get};
  use std::net::SocketAddr;
  use tower_http::trace::TraceLayer;

  use super::handlers;
  use super::state::SharedState;

  pub struct ProviderServer {
      state: SharedState,
      port: u16,
  }

  impl ProviderServer {
      pub fn new(state: SharedState, port: u16) -> Self {
          Self { state, port }
      }

      pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
          let app = Router::new()
              .route("/", get(handlers::index))
              .route("/health", get(handlers::health))
              .layer(TraceLayer::new_for_http())
              .with_state(self.state);

          let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
          tracing::info!("Provider HTTP server listening on {}", addr);

          let listener = tokio::net::TcpListener::bind(addr).await?;
          axum::serve(listener, app).await?;

          Ok(())
      }
  }
  ```

**Dependencies**: Task 1.2
**Output**: Basic HTTP server that starts and responds

---

## 1.4 Create basic handlers

- [ ] Create `daemon/src/provider/handlers.rs`:
  ```rust
  use axum::{
      extract::State,
      response::Json,
      http::StatusCode,
  };
  use serde::Serialize;

  use super::state::SharedState;

  #[derive(Serialize)]
  pub struct IndexResponse {
      pub name: String,
      pub version: String,
      pub channel: String,
      pub arch: String,
      pub uptime_secs: u64,
  }

  pub async fn index(State(state): State<SharedState>) -> Json<IndexResponse> {
      let state = state.read().await;
      Json(IndexResponse {
          name: "plasmd".to_string(),
          version: env!("CARGO_PKG_VERSION").to_string(),
          channel: state.config.channel.clone(),
          arch: state.config.arch.clone(),
          uptime_secs: state.started_at.elapsed().as_secs(),
      })
  }

  pub async fn health() -> StatusCode {
      StatusCode::OK
  }
  ```

**Dependencies**: Task 1.3
**Output**: Working index and health endpoints

---

## 1.5 Integrate with plasmd main

- [ ] Update `daemon/src/main.rs` to optionally start provider:
  ```rust
  use plasm::provider::{ProviderServer, ProviderState, ProviderConfig};

  // In daemon mode, start provider if configured
  if let Some(provider_config) = config.provider {
      let state = Arc::new(RwLock::new(ProviderState {
          config: provider_config.clone(),
          started_at: std::time::Instant::now(),
          requests_served: 0,
          bytes_served: 0,
      }));

      let server = ProviderServer::new(state, provider_config.port);
      tokio::spawn(async move {
          if let Err(e) = server.run().await {
              tracing::error!("Provider server error: {}", e);
          }
      });
  }
  ```
- [ ] Add provider config to main Config struct

**Dependencies**: Task 1.4
**Output**: plasmd starts HTTP server when configured

---

## 1.6 Test basic server

- [ ] Manual test:
  ```bash
  # Start plasmd with provider
  cargo run -- daemon --provider-port 8080 --artifacts /tmp/test

  # Test endpoints
  curl http://localhost:8080/
  curl http://localhost:8080/health
  ```
- [ ] Verify:
  - Server starts without errors
  - Index returns JSON with correct fields
  - Health returns 200 OK
  - Server logs requests

**Dependencies**: Task 1.5
**Output**: Working HTTP server in plasmd

---

## Validation Checklist

- [ ] `cargo build` succeeds with new dependencies
- [ ] `cargo test` passes (no regressions)
- [ ] HTTP server starts on configured port
- [ ] `/` returns provider info JSON
- [ ] `/health` returns 200 OK
- [ ] Server logs requests with tracing
- [ ] Server shuts down cleanly with plasmd
