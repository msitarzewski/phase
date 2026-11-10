// SPDX-License-Identifier: AGPL-3.0-or-later

//! Real-process HTTP boundary coverage for the advertised echo-mode API.
//!
//! This deliberately launches the compiled `lucidd` binary and talks to a
//! bound loopback socket. Unit tests in `ollama.rs` cover parsing helpers; this
//! test protects the binary wiring, Axum routes, streaming framing, response
//! headers, malformed-input behavior, and browser preflight contract together.

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

struct TestDaemon {
    child: Child,
    _temp: tempfile::TempDir,
    log_path: PathBuf,
    base_url: String,
}

impl TestDaemon {
    async fn spawn() -> Self {
        let temp = tempfile::tempdir().expect("create daemon temp dir");
        let log_path = temp.path().join("lucidd.log");
        let log = File::create(&log_path).expect("create daemon log");
        let stderr = log.try_clone().expect("clone daemon log handle");
        let port = free_port();
        let base_url = format!("http://127.0.0.1:{port}");

        let child = Command::new(env!("CARGO_BIN_EXE_lucidd"))
            .args([
                "--worker",
                "echo",
                "--no-default-bootstrap",
                "--libp2p-port",
                "0",
                "--identity-path",
            ])
            .arg(temp.path().join("identity.key"))
            .arg("--policy-config")
            .arg(temp.path().join("policy.toml"))
            .env("LUCIDD_HOST", "127.0.0.1")
            .env("LUCIDD_PORT", port.to_string())
            .env("RUST_LOG", "warn")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn compiled lucidd binary");

        let mut daemon = Self {
            child,
            _temp: temp,
            log_path,
            base_url,
        };
        daemon.wait_until_ready().await;
        daemon
    }

    async fn wait_until_ready(&mut self) {
        let client = Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .expect("build readiness client");
        let deadline = Instant::now() + Duration::from_secs(10);

        loop {
            if let Some(status) = self.child.try_wait().expect("poll lucidd child") {
                panic!(
                    "lucidd exited before HTTP readiness ({status})\n{}",
                    read_log(&self.log_path)
                );
            }

            if let Ok(response) = client.get(&self.base_url).send().await {
                if response.status() == StatusCode::OK {
                    return;
                }
            }

            assert!(
                Instant::now() < deadline,
                "lucidd did not become HTTP-ready within 10s\n{}",
                read_log(&self.log_path)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral probe");
    listener.local_addr().expect("read probe address").port()
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("<log unavailable: {error}>"))
}

fn assert_local_receipt_headers(response: &reqwest::Response) {
    assert_eq!(
        response
            .headers()
            .get("x-lucid-routed-via")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert!(
        response.headers().contains_key("x-phase-receipt"),
        "non-streaming response should surface the receipt commitment"
    );
    assert!(
        !response.headers().contains_key("x-lucid-receipt-verified"),
        "local work must not claim a peer-receipt verdict"
    );
    assert_public_route_explanation(response);
}

fn assert_public_route_explanation(response: &reqwest::Response) {
    let explanation = response
        .headers()
        .get("x-lucid-route-explanation")
        .and_then(|value| value.to_str().ok())
        .expect("successful routing should expose a public explanation");
    assert!(!explanation.is_empty());
    assert!(explanation.len() <= 192);
    assert!(explanation.is_ascii());
    assert!(explanation.bytes().all(|byte| byte >= 0x20 && byte != 0x7f));
}

fn parse_ndjson(body: &[u8]) -> Vec<Value> {
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("valid NDJSON line"))
        .collect()
}

fn assert_stream_shape(response: &reqwest::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/x-ndjson")
    );
    assert_eq!(
        response
            .headers()
            .get("x-lucid-routed-via")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-phase-worker")
            .and_then(|value| value.to_str().ok()),
        Some("lucidd")
    );
    assert_public_route_explanation(response);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_mode_binary_serves_advertised_http_and_browser_contract() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let daemon = TestDaemon::spawn().await;
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build API client");

        let root = client
            .get(daemon.url("/"))
            .send()
            .await
            .expect("root request");
        assert_eq!(root.status(), StatusCode::OK);
        assert!(root.text().await.expect("root body").contains("lucidd"));

        let version: Value = client
            .get(daemon.url("/api/version"))
            .send()
            .await
            .expect("version request")
            .error_for_status()
            .expect("version success")
            .json()
            .await
            .expect("version JSON");
        assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));

        let tags: Value = client
            .get(daemon.url("/api/tags"))
            .send()
            .await
            .expect("tags request")
            .error_for_status()
            .expect("tags success")
            .json()
            .await
            .expect("tags JSON");
        assert_eq!(tags["models"][0]["name"], "echo:latest");

        let show: Value = client
            .post(daemon.url("/api/show"))
            .json(&json!({"model": "echo"}))
            .send()
            .await
            .expect("show request")
            .error_for_status()
            .expect("show success")
            .json()
            .await
            .expect("show JSON");
        assert_eq!(show["details"]["family"], "echo");
        assert_eq!(show["capabilities"], json!(["completion"]));

        let chat = client
            .post(daemon.url("/api/chat"))
            .header("x-lucid-local-only", "true")
            .json(&json!({
                "model": "echo:latest",
                "messages": [{"role": "user", "content": "abc"}],
                "stream": false
            }))
            .send()
            .await
            .expect("non-streaming chat request");
        assert_eq!(chat.status(), StatusCode::OK);
        assert_local_receipt_headers(&chat);
        let chat_body: Value = chat.json().await.expect("non-streaming chat JSON");
        assert_eq!(chat_body["message"]["content"], "cba");
        assert_eq!(chat_body["done"], true);

        let chat_stream = client
            .post(daemon.url("/api/chat"))
            .json(&json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "stream"}],
                "stream": true
            }))
            .send()
            .await
            .expect("streaming chat request");
        assert_stream_shape(&chat_stream);
        let chat_lines = parse_ndjson(
            &chat_stream
                .bytes()
                .await
                .expect("streaming chat response body"),
        );
        let streamed_chat: String = chat_lines
            .iter()
            .filter(|line| line["done"] == false)
            .filter_map(|line| line["message"]["content"].as_str())
            .collect();
        assert_eq!(streamed_chat, "maerts");
        let chat_final = chat_lines.last().expect("terminal chat frame");
        assert_eq!(chat_final["done"], true);
        assert_eq!(
            chat_final["x_phase_commitment"]
                .as_str()
                .expect("chat commitment")
                .len(),
            64
        );

        let generate = client
            .post(daemon.url("/api/generate"))
            .json(&json!({"model": "echo", "prompt": "phase", "stream": false}))
            .send()
            .await
            .expect("non-streaming generate request");
        assert_eq!(generate.status(), StatusCode::OK);
        assert_local_receipt_headers(&generate);
        let generate_body: Value = generate.json().await.expect("generate JSON");
        assert_eq!(generate_body["response"], "esahp");

        let generate_stream = client
            .post(daemon.url("/api/generate"))
            .json(&json!({"model": "echo", "prompt": "xy", "stream": true}))
            .send()
            .await
            .expect("streaming generate request");
        assert_stream_shape(&generate_stream);
        let generate_lines = parse_ndjson(
            &generate_stream
                .bytes()
                .await
                .expect("streaming generate body"),
        );
        assert_eq!(
            generate_lines
                .iter()
                .filter(|line| line["done"] == false)
                .filter_map(|line| line["response"].as_str())
                .collect::<String>(),
            "yx"
        );
        assert_eq!(generate_lines.last().expect("generate final")["done"], true);

        let embed = client
            .post(daemon.url("/api/embed"))
            .json(&json!({"model": "echo", "input": ["cat", "kitten"]}))
            .send()
            .await
            .expect("embed request");
        assert_eq!(embed.status(), StatusCode::OK);
        assert_local_receipt_headers(&embed);
        let embed_body: Value = embed.json().await.expect("embed JSON");
        assert_eq!(
            embed_body["embeddings"].as_array().expect("vectors").len(),
            2
        );
        assert_eq!(
            embed_body["embeddings"][0]
                .as_array()
                .expect("first vector")
                .len(),
            16
        );

        let legacy_embed = client
            .post(daemon.url("/api/embeddings"))
            .json(&json!({"model": "echo", "prompt": "cat"}))
            .send()
            .await
            .expect("legacy embeddings request");
        assert_eq!(legacy_embed.status(), StatusCode::OK);
        assert_local_receipt_headers(&legacy_embed);
        let legacy_body: Value = legacy_embed.json().await.expect("legacy embeddings JSON");
        assert_eq!(
            legacy_body["embedding"]
                .as_array()
                .expect("legacy vector")
                .len(),
            16
        );

        let pull = client
            .post(daemon.url("/api/pull"))
            .json(&json!({"model": "echo", "stream": false}))
            .send()
            .await
            .expect("non-streaming pull request");
        assert_eq!(pull.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            pull.json::<Value>().await.expect("pull JSON")["status"],
            "error"
        );

        let pull_stream = client
            .post(daemon.url("/api/pull"))
            .json(&json!({"model": "echo", "stream": true}))
            .send()
            .await
            .expect("streaming pull request");
        assert_eq!(pull_stream.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            pull_stream
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/x-ndjson")
        );
        let pull_lines = parse_ndjson(&pull_stream.bytes().await.expect("pull stream body"));
        assert_eq!(pull_lines.last().expect("pull terminal")["status"], "error");

        let malformed = client
            .post(daemon.url("/api/chat"))
            .header("content-type", "application/json")
            .body("{not-json")
            .send()
            .await
            .expect("malformed request");
        assert!(
            malformed.status().is_client_error(),
            "malformed JSON must be rejected at the HTTP boundary"
        );

        for hostile in [
            json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "alien", "content": "x"}]
            }),
            json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x", "images": ["payload"]}]
            }),
            json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x"}],
                "tools": []
            }),
            json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x"}],
                "options": {"future_backend_knob": true}
            }),
        ] {
            let rejected = client
                .post(daemon.url("/api/chat"))
                .json(&hostile)
                .send()
                .await
                .expect("hostile chat request");
            assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        }

        let ignored_generate_control = client
            .post(daemon.url("/api/generate"))
            .json(&json!({
                "model": "echo", "prompt": "x", "stream": false,
                "template": "previously ignored"
            }))
            .send()
            .await
            .expect("unsupported generate control");
        assert_eq!(ignored_generate_control.status(), StatusCode::BAD_REQUEST);

        let unsupported_embedding_control = client
            .post(daemon.url("/api/embed"))
            .json(&json!({"model": "echo", "input": "x", "dimensions": 4}))
            .send()
            .await
            .expect("unsupported embedding control");
        assert_eq!(
            unsupported_embedding_control.status(),
            StatusCode::BAD_REQUEST
        );

        let unknown_wire_field = client
            .post(daemon.url("/api/chat"))
            .json(&json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x"}],
                "not_in_schema": true
            }))
            .send()
            .await
            .expect("unknown wire field");
        assert!(unknown_wire_field.status().is_client_error());

        let unknown_message_field = client
            .post(daemon.url("/api/chat"))
            .json(&json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x", "future_field": true}]
            }))
            .send()
            .await
            .expect("unknown nested message field");
        assert!(unknown_message_field.status().is_client_error());

        let unknown_legacy_embedding_field = client
            .post(daemon.url("/api/embeddings"))
            .json(&json!({
                "model": "echo", "prompt": "x", "future_field": true
            }))
            .send()
            .await
            .expect("unknown legacy embedding field");
        assert!(unknown_legacy_embedding_field.status().is_client_error());

        let oversized = client
            .post(daemon.url("/api/chat"))
            .json(&json!({
                "model": "echo", "stream": false,
                "messages": [{"role": "user", "content": "x".repeat(1024 * 1024 + 1)}]
            }))
            .send()
            .await
            .expect("oversized HTTP body");
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let traversal_pull = client
            .post(daemon.url("/api/pull"))
            .json(&json!({"model": "../escape", "stream": false}))
            .send()
            .await
            .expect("traversal pull");
        assert_eq!(traversal_pull.status(), StatusCode::NOT_FOUND);

        let unknown = client
            .get(daemon.url("/api/not-supported"))
            .send()
            .await
            .expect("unknown endpoint request");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let allowed_origin = "http://localhost:3000";
        let preflight = client
            .request(reqwest::Method::OPTIONS, daemon.url("/api/chat"))
            .header("origin", allowed_origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "content-type,x-lucid-local-only",
            )
            .send()
            .await
            .expect("loopback CORS preflight");
        assert_eq!(preflight.status(), StatusCode::OK);
        assert_eq!(
            preflight
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some(allowed_origin)
        );
        let allowed_methods = preflight
            .headers()
            .get("access-control-allow-methods")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(allowed_methods.contains("POST"));
        let allowed_headers = preflight
            .headers()
            .get("access-control-allow-headers")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(allowed_headers.contains("content-type"));
        assert!(allowed_headers.contains("x-lucid-local-only"));

        let browser_chat = client
            .post(daemon.url("/api/chat"))
            .header("origin", allowed_origin)
            .json(&json!({
                "model": "echo",
                "messages": [{"role": "user", "content": "web"}],
                "stream": false
            }))
            .send()
            .await
            .expect("browser-origin chat request");
        assert_eq!(browser_chat.status(), StatusCode::OK);
        assert_eq!(
            browser_chat
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok()),
            Some(allowed_origin)
        );
        let exposed = browser_chat
            .headers()
            .get("access-control-expose-headers")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(exposed.contains("x-lucid-routed-via"));
        assert!(exposed.contains("x-lucid-route-explanation"));
        assert!(exposed.contains("x-phase-receipt"));

        let external_preflight = client
            .request(reqwest::Method::OPTIONS, daemon.url("/api/chat"))
            .header("origin", "https://evil.example")
            .header("access-control-request-method", "POST")
            .send()
            .await
            .expect("external CORS preflight");
        assert!(
            !external_preflight
                .headers()
                .contains_key("access-control-allow-origin"),
            "non-loopback origins must not receive browser access"
        );
    })
    .await
    .expect("HTTP boundary test exceeded 30 seconds");
}
