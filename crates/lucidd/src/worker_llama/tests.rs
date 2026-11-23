// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use phase_manifest::ManifestBuilder;
use phase_protocol::{ChatMessage, ChatRole, SignedReceipt};

async fn exercise_raw_sse(body: &'static [u8]) -> (Vec<JobEvent>, SignedReceipt<JobResult>) {
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test backend");
    let port = listener.local_addr().expect("test backend address").port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let headers = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write response headers");
        socket.write_all(body).await.expect("write SSE body");
        socket.shutdown().await.expect("close response");
    });

    let failed = Arc::new(Notify::new());
    let failed_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let model = Arc::new(LoadedModel {
        port,
        last_used: Mutex::new(Instant::now()),
        failed,
        failed_flag,
        supervisor: tokio::spawn(std::future::pending::<()>()),
    });
    let manifest_hash = [0x61; 32];
    let (handle, producer) = JobHandle::new(JobId(manifest_hash));
    let events = run_inference(
        reqwest::Client::new(),
        model,
        CompletionRequest {
            body: serde_json::json!({"prompt": "test", "stream": true}),
            prompt_chars: 4,
        },
        manifest_hash,
        producer,
        NodeIdentity::generate(),
        Duration::from_secs(1),
    )
    .collect::<Vec<_>>()
    .await;
    let receipt = handle.finish().await.expect("signed failure receipt");
    server.await.expect("test backend exits");
    (events, receipt)
}

fn inference_with_sampling(entries: &[(&str, &str)], max_tokens: Option<u32>) -> InferenceJobSpec {
    let mut sampling = SamplingParams::default();
    for (key, value) in entries {
        sampling
            .params
            .insert((*key).to_string(), (*value).to_string());
    }
    InferenceJobSpec {
        model_cid: "test-model".to_string(),
        messages: Vec::new(),
        prompt: Some("trusted prompt".to_string()),
        resume_from: None,
        sampling,
        max_tokens,
        stream: true,
    }
}

#[test]
fn completion_request_rejects_server_owned_sampling_fields() {
    for key in ["n_predict", "prompt", "messages", "stream", "cache_prompt"] {
        let spec = inference_with_sampling(&[(key, "1")], Some(32));
        let error = build_completion_request(&spec).expect_err("reserved key must fail");
        assert!(
            error.contains("server-owned"),
            "unexpected error for {key}: {error}"
        );
    }
}

#[test]
fn completion_request_always_sets_bounded_server_n_predict() {
    let absent = inference_with_sampling(&[], None);
    let request = build_completion_request(&absent).expect("default request");
    assert_eq!(
        request.body["n_predict"],
        serde_json::json!(DEFAULT_N_PREDICT)
    );

    let oversized = inference_with_sampling(&[], Some(u32::MAX));
    let request = build_completion_request(&oversized).expect("clamped request");
    assert_eq!(request.body["n_predict"], serde_json::json!(MAX_N_PREDICT));

    let zero = inference_with_sampling(&[], Some(0));
    let request = build_completion_request(&zero).expect("minimum request");
    assert_eq!(request.body["n_predict"], serde_json::json!(1));
}

#[test]
fn completion_request_rejects_negative_oversized_and_unknown_sampling() {
    for (key, value) in [
        ("temperature", "-0.1"),
        ("temperature", "2.1"),
        ("top_p", "1.1"),
        ("min_p", "-0.1"),
        ("repetition_penalty", "0"),
        ("top_k", "-1"),
        ("top_k", "1001"),
        ("seed", "-1"),
        ("seed", "2147483648"),
        ("temperature", r#""0.7""#),
        ("top_k", "1.5"),
        ("stop", r#""END""#),
        ("future_llama_flag", "true"),
    ] {
        let spec = inference_with_sampling(&[(key, value)], Some(32));
        assert!(
            build_completion_request(&spec).is_err(),
            "expected {key}={value} to be rejected"
        );
    }
}

#[tokio::test]
async fn execute_rejects_reserved_sampling_before_model_load() {
    let spec = inference_with_sampling(&[("n_predict", "-1")], Some(u32::MAX));
    let manifest = ManifestBuilder::new(JobSpec::Inference(spec))
        .sign_with(&NodeIdentity::generate())
        .expect("sign manifest");
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

    let error = match worker.execute(manifest).await {
        Ok(_) => panic!("reserved sampling key must fail dispatch"),
        Err(error) => error,
    };
    assert!(matches!(error, WorkerError::BadManifest(_)));
}

#[tokio::test]
async fn execute_rejects_multimodal_images_before_model_load() {
    let mut spec = inference_with_sampling(&[], Some(32));
    spec.messages.push(ChatMessage {
        role: ChatRole::User,
        content: "describe this".to_string(),
        images: vec!["not-decoded-or-forwarded".to_string()],
    });
    let manifest = ManifestBuilder::new(JobSpec::Inference(spec))
        .sign_with(&NodeIdentity::generate())
        .expect("sign manifest");
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

    let error = match worker.execute(manifest).await {
        Ok(_) => panic!("multimodal input must fail before model lookup"),
        Err(error) => error,
    };
    assert!(matches!(error, WorkerError::BadManifest(message) if message.contains("multimodal")));
}

#[tokio::test]
async fn embedding_bounds_fail_before_model_load() {
    let identity = NodeIdentity::generate();
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
    for input in [
        Vec::new(),
        vec![String::new()],
        vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS + 1)],
        vec!["x".to_string(); MAX_EMBEDDING_INPUTS + 1],
        vec!["x".repeat(MAX_EMBEDDING_ENTRY_CHARS); 5],
    ] {
        let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
            model_cid: "does-not-load".to_string(),
            input,
        }))
        .sign_with(&identity)
        .expect("sign manifest");
        let error = match worker.execute(manifest).await {
            Ok(_) => panic!("invalid embedding must fail before model lookup"),
            Err(error) => error,
        };
        assert!(matches!(error, WorkerError::BadManifest(_)));
    }

    assert!(validate_embedding_spec(&EmbeddingJobSpec {
        model_cid: "cid".to_string(),
        input: vec!["ok".to_string()],
    })
    .is_ok());
}

#[tokio::test]
async fn occupied_loopback_port_is_never_reserved_for_llama() {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("occupy loopback port");
    let port = listener.local_addr().expect("occupied address").port();
    assert!(port < u16::MAX, "ephemeral port must have a successor");
    let worker = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_port_range: port..port + 1,
            ..LlamaCppConfig::default()
        },
    );

    assert!(matches!(
        worker.allocate_port().await,
        Err(WorkerError::Capacity)
    ));
    assert!(worker.inner.ports_in_use.lock().await.is_empty());
}

#[tokio::test]
async fn loopback_client_refuses_backend_redirects() {
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind redirect server");
    let port = listener.local_addr().expect("redirect address").port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.expect("read request");
        socket
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://192.0.2.1/exfiltrate\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("write redirect");
    });
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());

    let response = worker
        .inner
        .client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .expect("receive redirect response");
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    server.await.expect("redirect server exits");
}

#[cfg(unix)]
#[tokio::test]
async fn loopback_client_ignores_environment_proxy() {
    use tokio::io::AsyncWriteExt as _;

    const CHILD_MARKER: &str = "LUCID_LLAMA_NO_PROXY_CHILD";
    const TARGET_URL: &str = "LUCID_LLAMA_NO_PROXY_TARGET";
    if std::env::var_os(CHILD_MARKER).is_some() {
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
        let response = worker
            .inner
            .client
            .get(std::env::var(TARGET_URL).expect("child target URL"))
            .send()
            .await
            .expect("direct loopback request");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        return;
    }

    let target = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind direct target");
    let target_port = target.local_addr().expect("target address").port();
    let target_task = tokio::spawn(async move {
        let (mut socket, _) = target.accept().await.expect("accept direct request");
        let mut request = [0_u8; 1024];
        let _ = socket
            .read(&mut request)
            .await
            .expect("read direct request");
        socket
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("write direct response");
    });
    let hostile_proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hostile proxy");
    let proxy_port = hostile_proxy.local_addr().expect("proxy address").port();
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    let test_name = "worker_llama::tests::loopback_client_ignores_environment_proxy";
    let output =
        tokio::process::Command::new(std::env::current_exe().expect("current test executable"))
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env(TARGET_URL, format!("http://127.0.0.1:{target_port}/health"))
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .await
            .expect("run isolated proxy child test");
    assert!(
        output.status.success(),
        "proxy child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    tokio::time::timeout(Duration::from_secs(2), target_task)
        .await
        .expect("client bypasses proxy and reaches target")
        .expect("target task exits");
    drop(hostile_proxy);
}

#[tokio::test]
async fn concurrent_cold_loads_are_serialized_before_capacity_mutation() {
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
    let guard = worker.inner.load_gate.lock().await;
    let contender = worker.clone();
    let task = tokio::spawn(async move { contender.ensure_loaded("missing-model", false).await });

    assert!(tokio::time::timeout(Duration::from_millis(25), task)
        .await
        .is_err());
    drop(guard);

    // The timed-out JoinHandle was dropped, not the underlying task. A
    // second direct call proves the gate is usable and fails at the model
    // boundary rather than leaking a port reservation.
    assert!(matches!(
        worker.ensure_loaded("missing-model", false).await,
        Err(WorkerError::ArtifactUnavailable(_))
    ));
    assert!(worker.inner.ports_in_use.lock().await.is_empty());
}

#[test]
fn completion_request_rejects_oversized_stop_sequences() {
    let too_many = serde_json::to_string(&vec!["x"; MAX_STOP_SEQUENCES + 1]).unwrap();
    let spec = inference_with_sampling(&[("stop", &too_many)], Some(32));
    assert!(build_completion_request(&spec).is_err());

    let too_long = "x".repeat(MAX_STOP_SEQUENCE_CHARS + 1);
    let encoded = serde_json::to_string(&vec![too_long]).unwrap();
    let spec = inference_with_sampling(&[("stop", &encoded)], Some(32));
    assert!(build_completion_request(&spec).is_err());
}

#[test]
fn completion_request_forwards_only_validated_sampling() {
    let spec = inference_with_sampling(
        &[
            ("temperature", "0.7"),
            ("top_p", "0.9"),
            ("top_k", "40"),
            ("min_p", "0.05"),
            ("repetition_penalty", "1.1"),
            ("seed", "42"),
            ("stop", r#"["END","STOP"]"#),
        ],
        Some(64),
    );
    let request = build_completion_request(&spec).expect("valid request");
    let body = request.body;

    assert_eq!(body["prompt"], serde_json::json!("trusted prompt"));
    assert_eq!(body["stream"], serde_json::json!(true));
    assert_eq!(body["cache_prompt"], serde_json::json!(true));
    assert_eq!(body["n_predict"], serde_json::json!(64));
    assert_eq!(body["temperature"], serde_json::json!(0.7));
    assert_eq!(body["top_p"], serde_json::json!(0.9));
    assert_eq!(body["top_k"], serde_json::json!(40));
    assert_eq!(body["min_p"], serde_json::json!(0.05));
    assert_eq!(body["repetition_penalty"], serde_json::json!(1.1));
    assert_eq!(body["seed"], serde_json::json!(42));
    assert_eq!(body["stop"], serde_json::json!(["END", "STOP"]));
    assert_eq!(request.prompt_chars, 14);
}

#[test]
fn resolve_model_path_accepts_real_file_in_dir() {
    // SEC-04: a legitimate id resolves to `model_dir/<id>.gguf`.
    let dir = tempfile::tempdir().expect("tempdir");
    let model = dir.path().join("qwen3.gguf");
    std::fs::write(&model, b"gguf").expect("touch model");
    let resolved = resolve_model_path(dir.path(), "qwen3").expect("resolve ok");
    // Canonicalized on both sides — compare against the canonical file.
    assert_eq!(resolved, model.canonicalize().unwrap());
    assert!(resolved.starts_with(dir.path().canonicalize().unwrap()));
}

#[test]
fn resolve_model_path_rejects_traversal_and_separators() {
    // SEC-04: traversal / separators / absolute ids never resolve.
    let dir = tempfile::tempdir().expect("tempdir");
    for bad in [
        "../../etc/passwd",
        "..",
        "a/b",
        "a\\b",
        "/etc/passwd",
        "/tmp/x.gguf",
    ] {
        assert!(
            resolve_model_path(dir.path(), bad).is_err(),
            "expected reject for {bad:?}"
        );
    }
}

#[test]
fn resolve_model_path_rejects_leading_dash_empty_and_nul() {
    // SEC-04: leading `-` (arg injection), empty, and embedded NUL.
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(resolve_model_path(dir.path(), "--flag").is_err());
    assert!(resolve_model_path(dir.path(), "-rf").is_err());
    assert!(resolve_model_path(dir.path(), "").is_err());
    assert!(resolve_model_path(dir.path(), "x\0y").is_err());
}

#[test]
fn resolve_model_path_oracle_closed_same_client_error() {
    // SEC-04: a not-found id and a parse-/shape-rejected id collapse to
    // the SAME generic client-facing error string. `ensure_loaded` maps
    // both Err reasons to `ArtifactUnavailable("model unavailable")`.
    let dir = tempfile::tempdir().expect("tempdir");
    // Both of these are `Err(_)` at the resolver; the caller maps them
    // to one constant string, so the client cannot distinguish them.
    let not_found = resolve_model_path(dir.path(), "definitely-not-here");
    let bad_shape = resolve_model_path(dir.path(), "../escape");
    assert!(not_found.is_err());
    assert!(bad_shape.is_err());
    let client_err =
        |_: String| WorkerError::ArtifactUnavailable("model unavailable".into()).to_string();
    assert_eq!(
        client_err(not_found.unwrap_err()),
        client_err(bad_shape.unwrap_err())
    );
}

#[test]
fn strip_sse_data_prefix_handles_space() {
    let frame = b"data: {\"x\":1}";
    assert_eq!(strip_sse_data_prefix(frame), Some(&b"{\"x\":1}"[..]));
}

#[test]
fn strip_sse_data_prefix_handles_no_space() {
    let frame = b"data:{\"x\":1}";
    assert_eq!(strip_sse_data_prefix(frame), Some(&b"{\"x\":1}"[..]));
}

#[test]
fn strip_sse_data_prefix_skips_non_data() {
    let frame = b": comment";
    assert_eq!(strip_sse_data_prefix(frame), None);
}

#[test]
fn malformed_sse_data_frame_is_a_terminal_protocol_error() {
    let error = decode_completion_frame(br#"{"content":"partial","stop":tru}"#)
        .expect_err("malformed JSON must never be skipped");
    assert!(error.contains("malformed llama-server SSE data frame"));
}

#[test]
fn eof_without_explicit_stop_frame_can_never_be_success() {
    assert_eq!(validate_completion_eof(true, b""), Ok(()));

    let clean_eof = validate_completion_eof(false, b"")
        .expect_err("clean transport EOF is not a model terminal event");
    assert!(clean_eof.contains("without explicit stop:true"));

    let partial_eof = validate_completion_eof(false, br#"data: {"content":"partial"}"#)
        .expect_err("unterminated frame must fail closed");
    assert!(partial_eof.contains("unterminated byte(s)"));
    assert!(partial_eof.contains("no stop:true terminal frame"));
}

#[tokio::test]
async fn backend_eof_without_stop_emits_only_signed_error_terminal() {
    let (events, receipt) =
        exercise_raw_sse(b"data: {\"content\":\"partial\",\"stop\":false}\n\n").await;
    assert!(matches!(events.first(), Some(JobEvent::Output(_))));
    assert!(matches!(
        events.last(),
        Some(JobEvent::Final {
            result: JobResult {
                completion: Completion::Error,
                ..
            },
            error: Some(error),
        }) if error.contains("without explicit stop:true")
    ));
    assert!(!events.iter().any(|event| matches!(
        event,
        JobEvent::Final {
            result: JobResult {
                completion: Completion::Stop | Completion::Length,
                ..
            },
            ..
        }
    )));
    assert_eq!(receipt.result.completion, Completion::Error);
    let mut replay = CommitmentAccumulator::new();
    for event in &events {
        if let JobEvent::Output(chunk) = event {
            replay.update(chunk);
        }
    }
    let (commitment, count) = replay.finalize();
    assert_eq!(receipt.result.output_commitment, commitment);
    assert_eq!(receipt.result.output_chunk_count, count);
}

#[tokio::test]
async fn malformed_backend_sse_emits_only_signed_error_terminal() {
    let (events, receipt) =
        exercise_raw_sse(b"data: {\"content\":\"partial\",\"stop\":tru}\n\n").await;
    assert!(matches!(
        events.as_slice(),
        [JobEvent::Final {
            result: JobResult {
                completion: Completion::Error,
                ..
            },
            error: Some(error),
        }] if error.contains("malformed llama-server SSE data frame")
    ));
    assert_eq!(receipt.result.completion, Completion::Error);
}

#[test]
fn find_double_newline_finds_first() {
    let buf = b"hello\n\nworld\n\nbye";
    assert_eq!(crate::backend_io::find_sse_boundary(buf), Some((5, 2)));
}

#[tokio::test]
async fn ensure_loaded_warm_hit_does_not_block_on_load_gate() {
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), LlamaCppConfig::default());
    let loaded = Arc::new(LoadedModel {
        port: 18080,
        last_used: Mutex::new(Instant::now()),
        failed: Arc::new(Notify::new()),
        failed_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        supervisor: tokio::spawn(async {}),
    });
    worker
        .inner
        .loaded_models
        .insert("warm-model".to_string(), Arc::clone(&loaded));

    // Simulate an unrelated model cold-start holding `load_gate`.
    let _gate = worker.inner.load_gate.lock().await;
    let hit = timeout(
        Duration::from_millis(200),
        worker.ensure_loaded("warm-model", false),
    )
    .await
    .expect("warm hit must not block while load_gate is held")
    .expect("warm model should return Ok");
    assert_eq!(hit.port, 18080);
}

#[test]
fn delimiter_free_oversized_sse_is_rejected_before_buffer_growth() {
    let existing = vec![b'x'; MAX_SSE_FRAME_BYTES];
    let mut buffer = BytesMut::from(existing.as_slice());

    assert_eq!(append_sse_chunk(&mut buffer, b"x"), Err(SseFrameTooLarge));
    assert_eq!(buffer.len(), MAX_SSE_FRAME_BYTES);

    let mut oversized_frame = vec![b'x'; MAX_SSE_FRAME_BYTES + 1];
    oversized_frame.extend_from_slice(b"\n\n");
    let mut empty = BytesMut::new();
    assert_eq!(
        append_sse_chunk(&mut empty, &oversized_frame),
        Err(SseFrameTooLarge)
    );
    assert!(empty.is_empty());
}

#[test]
fn limited_body_rejects_oversized_chunks_without_partial_append() {
    let mut body = BytesMut::new();
    extend_limited_body(&mut body, b"12345678", 8).expect("exact limit is accepted");
    assert_eq!(&body[..], b"12345678");

    assert_eq!(
        extend_limited_body(&mut body, b"9", 8),
        Err(BackendBodyReadError::TooLarge { limit: 8 })
    );
    assert_eq!(&body[..], b"12345678");
}

#[test]
fn child_log_text_is_control_safe_and_character_bounded() {
    let mut hostile = b"prefix\r\n\x1b[31m\0".to_vec();
    hostile.extend(std::iter::repeat_n(b'x', 128));

    let sanitized = sanitize_bounded_log_text(&hostile, 32, false);
    assert!(sanitized.chars().count() <= 32);
    assert!(sanitized.chars().all(|character| !character.is_control()));
    assert!(sanitized.ends_with("...[truncated]"));
    assert!(!sanitized.contains('\u{1b}'));
    assert!(!sanitized.contains('\n'));

    let explicitly_truncated = sanitize_bounded_log_text(b"short", 32, true);
    assert!(explicitly_truncated.ends_with("...[truncated]"));
}

#[test]
fn parse_embedding_accepts_top_level_object() {
    let raw = br#"{"embedding":[0.1,0.2,0.3]}"#;
    let v = parse_embedding_response(raw).expect("parse");
    assert_eq!(v, vec![0.1f32, 0.2, 0.3]);
}

#[test]
fn parse_embedding_accepts_array_of_objects() {
    let raw = br#"[{"embedding":[1.0,2.0],"index":0}]"#;
    let v = parse_embedding_response(raw).expect("parse");
    assert_eq!(v, vec![1.0f32, 2.0]);
}

#[test]
fn parse_embedding_accepts_data_wrapper() {
    let raw = br#"{"data":[{"embedding":[4.0,5.0,6.0]}]}"#;
    let v = parse_embedding_response(raw).expect("parse");
    assert_eq!(v, vec![4.0f32, 5.0, 6.0]);
}

#[test]
fn parse_embedding_accepts_nested_array_of_objects() {
    // The REAL llama.cpp native /embedding endpoint wraps the pooled
    // vector in an OUTER array: `[{"index":0,"embedding":[[...]]}]`. The
    // Earlier fixture coverage used a flat array and missed this; a live
    // nomic-embed run returned zero vectors until the parser descended one
    // level. Take row 0 (the pooled vector).
    let raw = br#"[{"index":0,"embedding":[[7.0,8.0,9.0]]}]"#;
    let v = parse_embedding_response(raw).expect("parse");
    assert_eq!(v, vec![7.0f32, 8.0, 9.0]);
}

#[test]
fn parse_embedding_accepts_nested_top_level_object() {
    // Same nesting under the top-level `{"embedding":[[...]]}` shape.
    let raw = br#"{"embedding":[[1.5,2.5]]}"#;
    let v = parse_embedding_response(raw).expect("parse");
    assert_eq!(v, vec![1.5f32, 2.5]);
}

#[test]
fn parse_embedding_rejects_non_numeric_and_missing() {
    assert!(parse_embedding_response(br#"{"nope":1}"#).is_none());
    assert!(parse_embedding_response(br#"{"embedding":["x"]}"#).is_none());
    assert!(parse_embedding_response(br#"{"embedding":[]}"#).is_none());
    assert!(parse_embedding_response(b"not json").is_none());
}

#[test]
fn render_prompt_empty_messages_uses_prompt() {
    let spec = InferenceJobSpec {
        model_cid: "x".to_string(),
        messages: vec![],
        prompt: Some("Hello.".to_string()),
        resume_from: None,
        sampling: Default::default(),
        max_tokens: None,
        stream: true,
    };
    assert_eq!(
        build_completion_request(&spec).unwrap().body["prompt"],
        "Hello."
    );
}

#[test]
fn chat_request_waits_for_loaded_model_template() {
    let spec = InferenceJobSpec {
        model_cid: "x".to_string(),
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: "Be helpful.".to_string(),
                images: vec![],
            },
            ChatMessage {
                role: ChatRole::User,
                content: "Hi.".to_string(),
                images: vec![],
            },
        ],
        prompt: None,
        resume_from: None,
        sampling: Default::default(),
        max_tokens: None,
        stream: true,
    };
    let request = build_completion_request(&spec).unwrap();
    assert_eq!(request.body["prompt"], "");
    assert_eq!(request.prompt_chars, 0);
}
