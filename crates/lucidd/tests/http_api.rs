// SPDX-License-Identifier: AGPL-3.0-or-later

//! Real-process HTTP boundary coverage for the advertised echo-mode API.
//!
//! This deliberately launches the compiled `lucidd` binary and talks to a
//! bound loopback socket. Unit tests in `ollama.rs` cover parsing helpers; this
//! test protects the binary wiring, Axum routes, streaming framing, response
//! headers, malformed-input behavior, and browser preflight contract together.

use std::collections::HashSet;
use std::fs::File;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
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
        Self::spawn_with_bootstrap(&[], None).await
    }

    async fn spawn_with_bootstrap(bootstrap_peers: &[String], policy: Option<&str>) -> Self {
        let temp = tempfile::tempdir().expect("create daemon temp dir");
        let log_path = temp.path().join("lucidd.log");
        let log = File::create(&log_path).expect("create daemon log");
        let stderr = log.try_clone().expect("clone daemon log handle");
        let policy_path = temp.path().join("policy.toml");
        if let Some(policy) = policy {
            std::fs::write(&policy_path, policy).expect("write test policy");
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_lucidd"));
        if !bootstrap_peers.is_empty() {
            command.arg("--no-bootstrap-relays");
            for address in bootstrap_peers {
                command.arg("--bootstrap-peer").arg(address);
            }
        }
        command
            .args([
                "--worker",
                "echo",
                "--no-default-bootstrap",
                "--outbound-only",
                "--libp2p-port",
                "0",
                "--identity-path",
            ])
            .arg(temp.path().join("identity.key"))
            .arg("--policy-config")
            .arg(&policy_path)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        let (child, base_url) = spawn_on_disjoint_port(&mut command);

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
            .no_proxy()
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

#[tokio::test]
async fn peer_and_cluster_status_distinguish_explicit_authorization_from_open_mode() {
    use phase_identity::NodeIdentity;
    use phase_net::{Discovery, DiscoveryConfig};

    let authorized_identity = NodeIdentity::generate();
    let allowed_key = hex::encode_upper(authorized_identity.peer_id_bytes());
    let policy =
        format!("allow_unauthenticated_jobs = true\nauthorized_submitters = [\"{allowed_key}\"]\n");
    let mut bootstrap = Vec::new();
    let mut peers = Vec::new();
    for identity in [authorized_identity, NodeIdentity::generate()] {
        let peer = Discovery::new(DiscoveryConfig {
            identity: Some(identity),
            ..DiscoveryConfig::default()
        })
        .unwrap();
        peer.listen("/ip4/127.0.0.1/tcp/0").await.unwrap();
        let address = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(address) = peer.listen_addrs().await.unwrap().into_iter().next() {
                    break address;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("loopback peer listening");
        bootstrap.push(format!("{address}/p2p/{}", peer.local_peer_id()));
        peers.push(peer);
    }
    let authorized_peer = peers[0].local_peer_id().to_string();
    let other_peer = peers[1].local_peer_id().to_string();
    let daemon = TestDaemon::spawn_with_bootstrap(&bootstrap, Some(&policy)).await;
    let token = std::fs::read_to_string(daemon._temp.path().join("identity.key.control-token"))
        .expect("read local control token");
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for path in ["/api/lucid/peers", "/api/lucid/cluster"] {
        let response = client.get(daemon.url(path)).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    for (path, field) in [
        ("/api/lucid/peers", "peers"),
        ("/api/lucid/cluster", "connected_peers"),
    ] {
        let status = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let status: Value = client
                    .get(daemon.url(path))
                    .bearer_auth(token.trim())
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let rows = status[field].as_array().expect("peer list");
                if [&authorized_peer, &other_peer].into_iter().all(|id| {
                    rows.iter()
                        .any(|row| row["peer_id"].as_str() == Some(id.as_str()))
                }) {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("both loopback peers connected");
        let rows = status[field].as_array().unwrap();
        for (id, authorized) in [(&authorized_peer, true), (&other_peer, false)] {
            let peer = rows
                .iter()
                .find(|row| row["peer_id"].as_str() == Some(id.as_str()))
                .unwrap();
            assert_eq!(peer["authorized"], authorized, "{path}");
            assert!(peer["path"].is_string());
            assert_eq!(
                peer.as_object().unwrap().len(),
                3,
                "only peer ID, path and authorization"
            );
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_on_disjoint_port(command: &mut Command) -> (Child, String) {
    static RESERVED: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    // Serialize probe-to-spawn so a concurrent child cannot inherit a probe
    // listener. Use a range below ephemeral ports and separate from llama_worker.
    let mut reserved = RESERVED.get_or_init(Mutex::default).lock().unwrap();
    for _ in 0..128 {
        let ephemeral = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral probe");
        let port = 12_000 + ephemeral.local_addr().expect("read probe address").port() % 6_000;
        if reserved.contains(&port) {
            continue;
        }
        let Ok(probe) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else {
            continue;
        };
        reserved.insert(port);
        drop(probe);
        drop(ephemeral);
        let child = command
            .env("LUCIDD_HOST", "127.0.0.1")
            .env("LUCIDD_PORT", port.to_string())
            .spawn()
            .expect("spawn compiled lucidd binary");
        return (child, format!("http://127.0.0.1:{port}"));
    }
    panic!("could not allocate a disjoint daemon port");
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("<log unavailable: {error}>"))
}

#[tokio::test]
async fn native_control_requires_private_bearer_and_never_echoes_invitation_secrets() {
    let daemon = TestDaemon::spawn().await;
    let client = Client::builder().no_proxy().build().unwrap();
    let path = daemon._temp.path().join("identity.key.control-token");
    let token = std::fs::read_to_string(path).expect("daemon creates private control token");
    let token = token.trim();
    assert_eq!(token.len(), 64);
    for endpoint in [
        "/api/lucid/status",
        "/api/lucid/enroll",
        "/api/lucid/invitations",
    ] {
        let request = if endpoint.ends_with("status") {
            client.get(daemon.url(endpoint))
        } else {
            client
                .post(daemon.url(endpoint))
                .json(&if endpoint.ends_with("enroll") {
                    json!({"invitation":"private-seed-must-not-leak"})
                } else {
                    json!({})
                })
        };
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = response.text().await.unwrap();
        assert!(!body.contains(token));
        assert!(!body.contains("private-seed-must-not-leak"));
    }
    let response = client
        .get(daemon.url("/api/lucid/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status: Value = response.json().await.unwrap();
    assert!(status["enrollment"]["state"].is_string());
    assert!(status.get("peer_id").is_none());
    assert!(status.get("addresses").is_none());
    let response = client
        .post(daemon.url("/api/lucid/enroll"))
        .bearer_auth(token)
        .json(&json!({"invitation":"private-seed-must-not-leak"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!response
        .text()
        .await
        .unwrap()
        .contains("private-seed-must-not-leak"));
    let response = client
        .post(daemon.url("/api/lucid/enroll"))
        .bearer_auth(token)
        .json(&json!({"invitation":"x".repeat(16 * 1024)}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response = client
        .post(daemon.url("/api/lucid/invitations"))
        .bearer_auth(token)
        .json(&json!({"ttl_seconds":300}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let invite: Value = response.json().await.unwrap();
    assert!(invite["invitation"]
        .as_str()
        .unwrap()
        .starts_with("lucid://join/"));
}

#[test]
fn outbound_only_refuses_infrastructure_before_creating_identity() {
    let temp = tempfile::tempdir().unwrap();
    let identity = temp.path().join("must-not-exist.key");
    let output = Command::new(env!("CARGO_BIN_EXE_lucidd"))
        .args([
            "--mode",
            "infrastructure",
            "--outbound-only",
            "--external-address",
            "/ip4/127.0.0.1/tcp/4001",
            "--identity-path",
        ])
        .arg(&identity)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!identity.exists());
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
    assert_eq!(response.headers()["x-lucid-trust-label"], "local");
    assert_eq!(response.headers()["trailer"], "x-lucid-trust-label");
    assert_public_route_explanation(response);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn echo_mode_binary_serves_advertised_http_and_browser_contract() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let daemon = TestDaemon::spawn().await;
        let client = Client::builder()
            .no_proxy()
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

        for host in [
            "127.0.0.1:11434",
            "localhost:11434",
            "[::1]:11434",
            "localhost",
        ] {
            let response = client
                .get(daemon.url("/api/version"))
                .header(reqwest::header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{host}");
        }
        for host in [
            "evil.example:11434",
            "localhost:",
            "localhost:65536",
            "127.0.0.2",
        ] {
            let response = client
                .get(daemon.url("/api/version"))
                .header(reqwest::header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
        }

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
        assert_eq!(chat.headers()["x-lucid-trust-label"], "local");
        assert_local_receipt_headers(&chat);
        let chat_body: Value = chat.json().await.expect("non-streaming chat JSON");
        assert_eq!(chat_body["message"]["content"], "cba");
        assert_eq!(chat_body["done"], true);

        let report = lucidd::bench::run_benchmark(lucidd::bench::BenchConfig {
            api_base: daemon.base_url.clone(),
            model: "echo:latest".into(),
            total_requests: 1,
            concurrency: 1,
            max_tokens: 8,
            timeout_secs: 5,
        })
        .await
        .unwrap();
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.local_commitments, 1);
        assert_eq!(report.receipt_verified_count, 0);
        assert_eq!(report.eval_count_source, "self_reported");
        assert_eq!(report.trust_label_distribution.get("local"), Some(&1));

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
        assert_eq!(chat_final["x_lucid_trust_label"], "local");
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
        assert_eq!(generate.headers()["x-lucid-trust-label"], "local");
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
        let generate_final = generate_lines.last().expect("generate final");
        assert_eq!(generate_final["done"], true);
        assert_eq!(generate_final["x_lucid_trust_label"], "local");

        // Inspect the wire because reqwest consumes trailers without exposing them.
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let address = daemon.base_url.strip_prefix("http://").unwrap();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let payload = r#"{"model":"echo","prompt":"trailers","stream":true}"#;
        let request = format!(
            "POST /api/generate HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nTE: trailers\r\nConnection: close, TE\r\n\r\n{payload}",
            payload.len(),
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw_response = Vec::new();
        stream.read_to_end(&mut raw_response).await.unwrap();
        let raw_response = std::str::from_utf8(&raw_response).unwrap();
        assert!(raw_response.ends_with("0\r\nx-lucid-trust-label: local\r\n\r\n"), "{raw_response}");

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

#[tokio::test]
async fn permission_first_control_api_enforces_auth_consent_and_folder_sandbox() {
    let temp = tempfile::tempdir().expect("create isolated node temp dir");
    let log_path = temp.path().join("lucidd.log");
    let log = File::create(&log_path).expect("create daemon log");
    let stderr = log.try_clone().expect("clone daemon log");
    let token_path = temp.path().join("control.token");
    let node_cfg_path = temp.path().join("node.toml");

    let mut command = Command::new(env!("CARGO_BIN_EXE_lucidd"));
    command
        .args([
            "--mode",
            "consume-only",
            "--no-default-bootstrap",
            "--outbound-only",
            "--libp2p-port",
            "0",
        ])
        .arg("--identity-path")
        .arg(temp.path().join("identity.key"))
        .arg("--policy-config")
        .arg(temp.path().join("policy.toml"))
        .arg("--node-config")
        .arg(&node_cfg_path)
        .arg("--control-token-file")
        .arg(&token_path)
        .arg("--artifact-dir")
        .arg(temp.path().join("artifacts"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    let (child, base_url) = spawn_on_disjoint_port(&mut command);
    let mut daemon = TestDaemon {
        child,
        _temp: temp,
        log_path,
        base_url: base_url.clone(),
    };

    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    daemon.wait_until_ready().await;

    let token = std::fs::read_to_string(&token_path)
        .expect("read auto-created private control token")
        .trim()
        .to_string();
    assert!(!token.is_empty());

    // 1. All new control endpoints reject unauthenticated callers.
    for path in [
        "/api/lucid/probe",
        "/api/lucid/peers",
        "/api/lucid/models",
        "/api/lucid/runtime",
    ] {
        let unauth = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            unauth.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must require bearer token"
        );
    }

    // 2. Read-only probe succeeds with bearer token and starts opt-in/off.
    let probe: Value = client
        .get(format!("{base_url}/api/lucid/probe"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probe["onboarding_completed"], json!(false));
    assert_eq!(probe["providing_enabled"], json!(false));
    assert!(probe["hardware"]["cpu_cores"].as_u64().unwrap_or(0) >= 1);

    // 3. Providing fails closed (428) without explicit confirm_permission: true.
    let denied_provide = client
        .post(format!("{base_url}/api/lucid/provide"))
        .bearer_auth(&token)
        .json(&json!({
            "model_name": "test-model",
            "enable": true,
            "confirm_permission": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied_provide.status(), StatusCode::PRECONDITION_REQUIRED);

    // 4. Runtime setup fails closed (428) without explicit confirm_permission: true.
    let denied_runtime = client
        .post(format!("{base_url}/api/lucid/runtime/setup"))
        .bearer_auth(&token)
        .json(&json!({
            "action": "pin_detected",
            "confirm_permission": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied_runtime.status(), StatusCode::PRECONDITION_REQUIRED);

    // 5. Folder sandbox refuses sensitive system directories (/etc).
    let blocked_folder = client
        .post(format!("{base_url}/api/lucid/folders"))
        .bearer_auth(&token)
        .json(&json!({ "action": "add", "path": "/etc" }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_folder.status(), StatusCode::BAD_REQUEST);

    // 6. Valid model folder with a real GGUF header is discovered read-only.
    let model_dir = daemon._temp.path().join("my-models");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(
        model_dir.join("tiny-q4_k_m.gguf"),
        b"GGUF\x03\x00\x00\x00synthetic-header-bytes",
    )
    .unwrap();
    let add_folder = client
        .post(format!("{base_url}/api/lucid/folders"))
        .bearer_auth(&token)
        .json(&json!({
            "action": "add",
            "path": model_dir.display().to_string()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(add_folder.status(), StatusCode::OK);
    let add_body: Value = add_folder.json().await.unwrap();
    let discovered = add_body["probe"]["discovered_models"]
        .as_array()
        .expect("discovered_models array");
    assert!(
        discovered.iter().any(|m| m["name"] == "tiny-q4_k_m"),
        "expected tiny-q4_k_m in discovered models: {discovered:?}"
    );

    // 7. Granting explicit permission records node.toml and imports verified GGUF.
    let grant = client
        .post(format!("{base_url}/api/lucid/provide"))
        .bearer_auth(&token)
        .json(&json!({
            "model_name": "tiny-q4_k_m",
            "enable": true,
            "confirm_permission": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(grant.status(), StatusCode::OK);
    let grant_body: Value = grant.json().await.unwrap();
    assert_eq!(grant_body["providing_enabled"], json!(true));
    assert!(
        grant_body["imported_cid"]
            .as_str()
            .is_some_and(|c| c.len() == 64),
        "expected hot-imported 64-hex CID, got {grant_body}"
    );

    // 8. Settings endpoint enforces thermal bounds and updates policy cleanly.
    let bad_settings = client
        .post(format!("{base_url}/api/lucid/settings"))
        .bearer_auth(&token)
        .json(&json!({ "thermal_limit_c": 200.0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_settings.status(), StatusCode::BAD_REQUEST);

    let ok_settings = client
        .post(format!("{base_url}/api/lucid/settings"))
        .bearer_auth(&token)
        .json(&json!({
            "manual_pause": true,
            "auto_pause_on_battery": true,
            "thermal_limit_c": 72.0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_settings.status(), StatusCode::OK);

    // 9. Trusted-Circle cluster endpoint enforces auth, fail-closed untrusted
    // sharding, and returns coordinator-signed Tier-1 partition plans.
    let unauth_cluster = client
        .get(format!("{base_url}/api/lucid/cluster"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_cluster.status(), StatusCode::UNAUTHORIZED);

    let cluster_status: Value = client
        .get(format!("{base_url}/api/lucid/cluster"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cluster_status["cluster_enabled"], json!(false));
    assert_eq!(cluster_status["untrusted_sharding_allowed"], json!(false));
    assert_eq!(
        cluster_status["trust_label"],
        json!("trusted-circle-sharded")
    );
    let local_vk = cluster_status["local_node"]["verifying_key_hex"]
        .as_str()
        .expect("local verifying_key_hex");
    let gib: u64 = 1024 * 1024 * 1024;

    let forbidden_untrusted = client
        .post(format!("{base_url}/api/lucid/cluster/plan"))
        .bearer_auth(&token)
        .json(&json!({
            "model_name": "llama3.3:70b-q4_k_m",
            "total_layers": 80,
            "size_bytes": 42 * gib,
            "trust_tier": "untrusted-spot-checked",
            "peers": [
                { "peer_id": "peer-a", "verifying_key_hex": local_vk, "available_memory_bytes": 64 * gib, "backend": "metal" },
                { "peer_id": "peer-b", "verifying_key_hex": local_vk, "available_memory_bytes": 24 * gib, "backend": "cuda" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_untrusted.status(), StatusCode::FORBIDDEN);

    let trusted_plan_resp = client
        .post(format!("{base_url}/api/lucid/cluster/plan"))
        .bearer_auth(&token)
        .json(&json!({
            "model_name": "llama3.3:70b-q4_k_m",
            "total_layers": 80,
            "size_bytes": 42 * gib,
            "peers": [
                { "peer_id": "peer-a", "verifying_key_hex": local_vk, "available_memory_bytes": 64 * gib, "backend": "metal" },
                { "peer_id": "peer-b", "verifying_key_hex": local_vk, "available_memory_bytes": 24 * gib, "backend": "cuda" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(trusted_plan_resp.status(), StatusCode::OK);
    let trusted_plan: Value = trusted_plan_resp.json().await.unwrap();
    assert_eq!(trusted_plan["verified"], json!(true));
    assert_eq!(
        trusted_plan["signed_plan"]["plan"]["stages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let cluster_disabled = client
        .post(format!("{base_url}/api/lucid/cluster/execute"))
        .bearer_auth(&token)
        .json(&json!({
            "signed_plan": trusted_plan["signed_plan"],
            "input_base64": "AQ==",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cluster_disabled.status(), StatusCode::FORBIDDEN);
    assert!(!cluster_disabled
        .headers()
        .contains_key("x-lucid-trust-label"));
}
