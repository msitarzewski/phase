// SPDX-License-Identifier: AGPL-3.0-or-later

//! Test-only protocol fixture for `worker_llama` integration tests.
//!
//! This file is included as a module of `tests/llama_worker.rs`; it is not
//! declared as a Cargo binary target. The integration-test executable
//! re-enters a single ignored harness entrypoint when a worker subprocess is
//! needed, which keeps this fixture out of `cargo install` and release bins.
//!
//! Mirrors just enough of the llama-server surface that `LlamaCppWorker`
//! can drive it end-to-end:
//!
//! - Receives the worker's parsed `--port` and `--host` through the temporary
//!   launcher; other llama-server arguments are intentionally ignored there.
//! - Serves `GET /health` → `{"status":"ok"}` (or 503 for a configurable
//!   warmup period).
//! - Serves `POST /completion` returning SSE frames the worker can decode.
//!
//! Behaviour knobs (env vars, picked up at fixture spawn time):
//!
//! | env var | effect |
//! | --- | --- |
//! | `FAKE_LLAMA_TOKENS=a,b,c` | Tokens to emit (default `Hello,, ,world`). |
//! | `FAKE_LLAMA_DELAY_MS=20` | Inter-token delay. |
//! | `FAKE_LLAMA_WARMUP_MS=0` | Return 503 from `/health` for this long. |
//! | `FAKE_LLAMA_HANG_AFTER=N` | Stop emitting after N tokens, hold socket. |
//! | `FAKE_LLAMA_CRASH_AFTER_MS=N` | `std::process::exit(2)` after N ms. |
//! | `FAKE_LLAMA_FAIL_HEALTH=1` | Always return 500 from `/health`. |
//!
//! The worker only ever cares about the SSE chunk shape and the `/health`
//! semantics, so the fixture stays small.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct Config {
    tokens: Vec<String>,
    delay: Duration,
    warmup: Duration,
    hang_after: Option<usize>,
    fail_health: bool,
    boot_at: std::time::Instant,
}

pub(crate) fn run() -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build fixture runtime");
    runtime.block_on(run_server())
}

async fn run_server() -> ! {
    let port: u16 = std::env::var("PHASE_FAKE_LLAMA_PORT")
        .expect("fixture wrapper must set PHASE_FAKE_LLAMA_PORT")
        .parse()
        .expect("fixture port must be a u16");
    let host = std::env::var("PHASE_FAKE_LLAMA_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

    let cfg = Config {
        tokens: std::env::var("FAKE_LLAMA_TOKENS")
            .ok()
            .map(|s| s.split(',').map(|t| t.to_string()).collect())
            .unwrap_or_else(|| {
                vec![
                    "Hello".to_string(),
                    ",".to_string(),
                    " ".to_string(),
                    "world".to_string(),
                    "!".to_string(),
                ]
            }),
        delay: Duration::from_millis(
            std::env::var("FAKE_LLAMA_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20),
        ),
        warmup: Duration::from_millis(
            std::env::var("FAKE_LLAMA_WARMUP_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        ),
        hang_after: std::env::var("FAKE_LLAMA_HANG_AFTER")
            .ok()
            .and_then(|s| s.parse().ok()),
        fail_health: std::env::var("FAKE_LLAMA_FAIL_HEALTH").is_ok(),
        boot_at: std::time::Instant::now(),
    };

    // Optional self-destruct used to simulate a crash mid-stream.
    if let Some(ms) = std::env::var("FAKE_LLAMA_CRASH_AFTER_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            // Non-zero exit so the supervisor sees `code != 0` and treats
            // the child as crashed rather than cleanly exited.
            std::process::exit(2);
        });
    }

    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/completion", post(handle_completion))
        .route("/v1/chat/completions", post(handle_completion))
        .route("/embedding", post(handle_embedding))
        .with_state(Arc::new(cfg));

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("valid host:port from args");
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake-llama-server bind {addr} failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("fake-llama-server listening on {addr}");
    if let Err(error) = axum::serve(listener, app).await {
        eprintln!("fake-llama-server stopped: {error}");
        std::process::exit(1);
    }
    std::process::exit(0)
}

async fn handle_health(State(cfg): State<Arc<Config>>) -> Response {
    if cfg.fail_health {
        return (StatusCode::INTERNAL_SERVER_ERROR, "fail").into_response();
    }
    if cfg.boot_at.elapsed() < cfg.warmup {
        return (StatusCode::SERVICE_UNAVAILABLE, "loading model").into_response();
    }
    Json(serde_json::json!({"status": "ok"})).into_response()
}

#[derive(Deserialize)]
struct CompletionRequest {
    #[serde(default)]
    #[allow(dead_code)]
    prompt: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    stream: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    n_predict: Option<i32>,
    #[serde(default)]
    #[allow(dead_code)]
    messages: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct CompletionChunk<'a> {
    content: &'a str,
    stop: bool,
}

#[derive(Serialize)]
struct FinalCompletionChunk<'a> {
    content: &'a str,
    stop: bool,
    stop_type: &'a str,
    tokens_predicted: usize,
    tokens_evaluated: usize,
}

#[derive(Deserialize)]
struct EmbeddingRequest {
    #[serde(default)]
    #[allow(dead_code)]
    content: Option<String>,
}

/// Canned `/embedding` handler. The worker only cares about extracting a
/// `Vec<f32>` from one of the accepted response shapes, so we return the
/// top-level `{"embedding": [...]}` form with a fixed short vector. That's
/// enough to exercise `run_embedding` end-to-end (chunk emission, ordering,
/// commitment, receipt) without a real model.
async fn handle_embedding(
    State(_cfg): State<Arc<Config>>,
    Json(_req): Json<EmbeddingRequest>,
) -> Response {
    Json(serde_json::json!({ "embedding": [0.1f32, 0.2, 0.3, 0.4] })).into_response()
}

async fn handle_completion(
    State(cfg): State<Arc<Config>>,
    Json(_req): Json<CompletionRequest>,
) -> Response {
    let cfg = cfg.clone();
    let stream = async_stream::stream! {
        for (idx, tok) in cfg.tokens.iter().enumerate() {
            // Hang test: stop emitting but keep socket open. The worker's
            // hang detector should notice and abort.
            if let Some(hang) = cfg.hang_after {
                if idx >= hang {
                    // Sleep "forever" so the worker has to time out.
                    tokio::time::sleep(Duration::from_secs(600)).await;
                    return;
                }
            }
            let chunk = CompletionChunk { content: tok, stop: false };
            let mut frame = b"data: ".to_vec();
            frame.extend_from_slice(&serde_json::to_vec(&chunk).unwrap());
            frame.extend_from_slice(b"\n\n");
            yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
            tokio::time::sleep(cfg.delay).await;
        }
        let final_chunk = FinalCompletionChunk {
            content: "",
            stop: true,
            stop_type: "eos",
            tokens_predicted: cfg.tokens.len(),
            tokens_evaluated: 1,
        };
        let mut frame = b"data: ".to_vec();
        frame.extend_from_slice(&serde_json::to_vec(&final_chunk).unwrap());
        frame.extend_from_slice(b"\n\n");
        yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}
