// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::convert::Infallible;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::sync::Arc;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use axum::body::Body;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use axum::extract::State;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use axum::http::{header, Response, StatusCode};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use axum::routing::{get, post};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use axum::{Json, Router};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use futures::StreamExt;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use phase_manifest::ManifestBuilder;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use phase_protocol::{EmbeddingJobSpec, JobSpec};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use tokio::net::TcpListener;

use super::*;

fn inference(model_cid: &str) -> InferenceJobSpec {
    InferenceJobSpec {
        model_cid: model_cid.to_string(),
        messages: Vec::new(),
        prompt: Some("hello".into()),
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: Some(8),
        stream: true,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn signed_job(spec: JobSpec) -> SignedManifest<JobSpec> {
    ManifestBuilder::new(spec)
        .sign_with(&NodeIdentity::generate())
        .expect("sign test manifest")
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    let mut permissions = std::fs::symlink_metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn create_immutable_bundle(root: &Path) -> PathBuf {
    let bundle = root.join("bundle");
    std::fs::create_dir(&bundle).unwrap();
    std::fs::write(
        bundle.join("config.json"),
        br#"{"max_position_embeddings":4096}"#,
    )
    .unwrap();
    std::fs::write(bundle.join("tokenizer_config.json"), b"{}").unwrap();
    std::fs::write(bundle.join("tokenizer.json"), b"{}").unwrap();
    std::fs::write(bundle.join("model.safetensors"), b"fixture").unwrap();
    let nested = bundle.join("weights");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("model-00002.safetensors"), b"second").unwrap();
    for file in [
        bundle.join("config.json"),
        bundle.join("tokenizer_config.json"),
        bundle.join("tokenizer.json"),
        bundle.join("model.safetensors"),
        nested.join("model-00002.safetensors"),
    ] {
        set_mode(&file, 0o444);
    }
    set_mode(&nested, 0o555);
    set_mode(&bundle, 0o555);
    bundle
}

#[cfg(unix)]
fn thaw_for_cleanup(path: &Path) {
    let Ok(metadata) = path.symlink_metadata() else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        set_mode(path, 0o755);
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                thaw_for_cleanup(&entry.path());
            }
        }
    } else if metadata.is_file() {
        set_mode(path, 0o644);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn create_runtime_and_bundle(root: &Path, port: u16) -> (PathBuf, PathBuf, MlxConfig) {
    let runtime = root.join("pinned-mlx-lm-server");
    std::fs::write(
        &runtime,
        b"#!/bin/sh\ntrap 'exit 0' INT TERM\nwhile :; do sleep 1; done\n",
    )
    .expect("write fixture runtime");
    set_mode(&runtime, 0o555);

    let bundle = create_immutable_bundle(root);
    let bundle_metadata = inspect_mlx_bundle(&bundle).unwrap();
    let mut config = MlxConfig::new(
        runtime.clone(),
        bundle.clone(),
        bundle_metadata.model_cid,
        port,
    );
    config.model_load_timeout = Duration::from_secs(2);
    config.request_start_timeout = Duration::from_secs(1);
    config.per_request_idle_timeout = Duration::from_millis(75);
    config.request_deadline = Duration::from_secs(1);
    config.shutdown_timeout = Duration::from_millis(100);
    config.max_start_retries = 0;
    config.restart_backoff = Duration::from_millis(1);
    (runtime, bundle, config)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
async fn attach_fixture_server(worker: &MlxWorker, port: u16) {
    let failed = Arc::new(AtomicBool::new(false));
    let failed_task = failed.clone();
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
    let supervisor = tokio::spawn(async move {
        if let Some(request) = shutdown_rx.recv().await {
            failed_task.store(true, Ordering::Release);
            let _ = request.acknowledgement.send(());
        }
    });
    *worker.inner.server.lock().await = Some(Arc::new(MlxServer {
        port,
        failed,
        shutdown_tx,
        supervisor,
    }));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone)]
enum FixtureMode {
    Success,
    Hang,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone)]
struct FixtureState {
    mode: FixtureMode,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
async fn fixture_health() -> StatusCode {
    StatusCode::OK
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
async fn fixture_completion(
    State(state): State<FixtureState>,
    Json(body): Json<serde_json::Value>,
) -> Response<Body> {
    state.bodies.lock().await.push(body.clone());
    if body.get("stream") == Some(&serde_json::Value::Bool(false)) {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"choices":[{"text":"x"}]}"#))
            .unwrap();
    }
    match state.mode {
        FixtureMode::Success => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(concat!(
                "data: {\"choices\":[{\"text\":\"hello\",\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            )))
            .unwrap(),
        FixtureMode::Hang => {
            let pending = futures::stream::pending::<Result<Bytes, Infallible>>();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(pending))
                .unwrap()
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
async fn start_fixture(
    mode: FixtureMode,
) -> (
    u16,
    Arc<Mutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let state = FixtureState {
        mode,
        bodies: bodies.clone(),
    };
    let app = Router::new()
        .route("/health", get(fixture_health))
        .route("/v1/completions", post(fixture_completion))
        .route("/v1/chat/completions", post(fixture_completion))
        .with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, bodies, task)
}

#[cfg(unix)]
#[test]
fn canonical_bundle_root_is_deterministic_and_exposes_hashed_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let first_root = directory.path().join("first");
    let second_root = directory.path().join("second");
    std::fs::create_dir(&first_root).unwrap();
    std::fs::create_dir(&second_root).unwrap();
    let first = create_immutable_bundle(&first_root);
    let second = create_immutable_bundle(&second_root);

    let first_metadata = inspect_mlx_bundle(&first).unwrap();
    let second_metadata = inspect_mlx_bundle(&second).unwrap();
    assert_eq!(first_metadata, second_metadata);
    assert_eq!(first_metadata.file_count, 5);
    assert_eq!(first_metadata.context_length, Some(4096));
    assert!(first_metadata.total_bytes > 0);
    assert_eq!(MLX_BUNDLE_FORMAT, "mlx-bundle-v1");
    assert_eq!(MLX_BUNDLE_ROOT_ALGORITHM, "sha256:phase/mlx-bundle-root/v1");

    thaw_for_cleanup(&first);
    thaw_for_cleanup(&second);
}

#[cfg(unix)]
#[test]
fn bundle_root_changes_with_file_bytes_and_relative_path() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = create_immutable_bundle(directory.path());
    let original = inspect_mlx_bundle(&bundle).unwrap();

    let weights = bundle.join("model.safetensors");
    set_mode(&weights, 0o644);
    std::fs::write(&weights, b"changed").unwrap();
    set_mode(&weights, 0o444);
    let content_changed = inspect_mlx_bundle(&bundle).unwrap();
    assert_ne!(content_changed.model_cid, original.model_cid);

    set_mode(&bundle, 0o755);
    std::fs::rename(&weights, bundle.join("renamed.safetensors")).unwrap();
    set_mode(&bundle, 0o555);
    let path_changed = inspect_mlx_bundle(&bundle).unwrap();
    assert_ne!(path_changed.model_cid, content_changed.model_cid);

    thaw_for_cleanup(&bundle);
}

#[cfg(unix)]
#[test]
fn bundle_scan_rejects_writable_links_hardlinks_and_ambiguous_paths() {
    use std::os::unix::fs::symlink;

    let writable_root = tempfile::tempdir().unwrap();
    let writable = create_immutable_bundle(writable_root.path());
    set_mode(&writable.join("tokenizer.json"), 0o644);
    assert!(matches!(
        inspect_mlx_bundle(&writable),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("write permission")
    ));
    thaw_for_cleanup(&writable);

    let symlink_root = tempfile::tempdir().unwrap();
    let linked = create_immutable_bundle(symlink_root.path());
    set_mode(&linked, 0o755);
    symlink("config.json", linked.join("config-link.json")).unwrap();
    set_mode(&linked, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&linked),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("symbolic")
    ));
    let root_link = symlink_root.path().join("bundle-root-link");
    symlink(&linked, &root_link).unwrap();
    assert!(matches!(
        inspect_mlx_bundle(&root_link),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("root must not be a symbolic link")
    ));
    thaw_for_cleanup(&linked);

    let hardlink_root = tempfile::tempdir().unwrap();
    let hardlinked = create_immutable_bundle(hardlink_root.path());
    set_mode(&hardlinked, 0o755);
    std::fs::hard_link(
        hardlinked.join("tokenizer.json"),
        hardlinked.join("duplicate-tokenizer.json"),
    )
    .unwrap();
    set_mode(&hardlinked, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&hardlinked),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("hard-linked")
    ));
    thaw_for_cleanup(&hardlinked);

    let ambiguous_root = tempfile::tempdir().unwrap();
    let ambiguous = create_immutable_bundle(ambiguous_root.path());
    set_mode(&ambiguous, 0o755);
    let ambiguous_file = ambiguous.join("bad\\name.json");
    std::fs::write(&ambiguous_file, b"{}").unwrap();
    set_mode(&ambiguous_file, 0o444);
    set_mode(&ambiguous, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&ambiguous),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("portable canonical UTF-8")
    ));
    thaw_for_cleanup(&ambiguous);

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;

        let non_utf8_root = tempfile::tempdir().unwrap();
        let non_utf8 = create_immutable_bundle(non_utf8_root.path());
        set_mode(&non_utf8, 0o755);
        let non_utf8_file = non_utf8.join(std::ffi::OsString::from_vec(vec![b'b', 0xff]));
        std::fs::write(&non_utf8_file, b"x").unwrap();
        set_mode(&non_utf8_file, 0o444);
        set_mode(&non_utf8, 0o555);
        assert!(matches!(
            inspect_mlx_bundle(&non_utf8),
            Err(WorkerError::ArtifactUnavailable(message))
                if message.contains("not valid UTF-8")
        ));
        thaw_for_cleanup(&non_utf8);
    }
}

#[cfg(unix)]
#[test]
fn bundle_scan_rejects_conflicting_or_invalid_context_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = create_immutable_bundle(directory.path());
    let config_path = bundle.join("config.json");
    set_mode(&config_path, 0o644);
    std::fs::write(
        &config_path,
        br#"{"max_position_embeddings":4096,"text_config":{"max_position_embeddings":8192}}"#,
    )
    .unwrap();
    set_mode(&config_path, 0o444);
    assert!(matches!(
        inspect_mlx_bundle(&bundle),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("conflicting context")
    ));
    thaw_for_cleanup(&bundle);

    let custom_code_root = tempfile::tempdir().unwrap();
    let custom_code = create_immutable_bundle(custom_code_root.path());
    let config_path = custom_code.join("config.json");
    set_mode(&config_path, 0o644);
    std::fs::write(
        &config_path,
        br#"{"max_position_embeddings":4096,"nested":{"auto_map":{"AutoModel":"model.Custom"}}}"#,
    )
    .unwrap();
    set_mode(&config_path, 0o444);
    assert!(matches!(
        inspect_mlx_bundle(&custom_code),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("custom code")
    ));
    thaw_for_cleanup(&custom_code);

    let executable_root = tempfile::tempdir().unwrap();
    let executable = create_immutable_bundle(executable_root.path());
    set_mode(&executable, 0o755);
    let executable_file = executable.join("hook.json");
    std::fs::write(&executable_file, b"{}").unwrap();
    set_mode(&executable_file, 0o555);
    set_mode(&executable, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&executable),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("must not be executable")
    ));
    thaw_for_cleanup(&executable);

    let source_root = tempfile::tempdir().unwrap();
    let source = create_immutable_bundle(source_root.path());
    set_mode(&source, 0o755);
    let source_file = source.join("modeling_custom.py");
    std::fs::write(&source_file, b"raise RuntimeError('must not load')").unwrap();
    set_mode(&source_file, 0o444);
    set_mode(&source, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&source),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("executable code")
    ));
    thaw_for_cleanup(&source);
}

#[cfg(unix)]
#[test]
fn bundle_scan_enforces_recursive_count_depth_and_byte_limits() {
    let depth_root = tempfile::tempdir().unwrap();
    let deep = create_immutable_bundle(depth_root.path());
    set_mode(&deep, 0o755);
    let mut cursor = deep.clone();
    let mut created_directories = Vec::new();
    for index in 0..=MAX_BUNDLE_DEPTH {
        cursor = cursor.join(format!("d{index:02}"));
        std::fs::create_dir(&cursor).unwrap();
        created_directories.push(cursor.clone());
    }
    for directory in created_directories.iter().rev() {
        set_mode(directory, 0o555);
    }
    set_mode(&deep, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&deep),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("depth limit")
    ));
    thaw_for_cleanup(&deep);

    let bytes_root = tempfile::tempdir().unwrap();
    let oversized = create_immutable_bundle(bytes_root.path());
    set_mode(&oversized, 0o755);
    let sparse = oversized.join("oversized.safetensors");
    let sparse_file = File::create(&sparse).unwrap();
    sparse_file.set_len(MAX_MODEL_SIZE_BYTES + 1).unwrap();
    drop(sparse_file);
    set_mode(&sparse, 0o444);
    set_mode(&oversized, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&oversized),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("byte limit")
    ));
    thaw_for_cleanup(&oversized);

    let count_root = tempfile::tempdir().unwrap();
    let crowded = create_immutable_bundle(count_root.path());
    set_mode(&crowded, 0o755);
    for index in 0..MAX_BUNDLE_ENTRIES {
        let path = crowded.join(format!("extra-{index:04}.json"));
        File::create(&path).unwrap();
        set_mode(&path, 0o444);
    }
    set_mode(&crowded, 0o555);
    assert!(matches!(
        inspect_mlx_bundle(&crowded),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("entry limit")
    ));
    thaw_for_cleanup(&crowded);
}

#[test]
fn request_forces_default_model_and_rejects_model_override() {
    let request = build_completion_request(&inference("cid:test"), 64).unwrap();
    assert_eq!(request.body["model"], "default_model");
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["max_tokens"], 8);

    let mut hostile = inference("cid:test");
    hostile
        .sampling
        .params
        .insert("model".into(), r#""remote/repo""#.into());
    assert!(build_completion_request(&hostile, 64).is_err());
    hostile.sampling.params.clear();
    hostile
        .sampling
        .params
        .insert("adapters".into(), r#""../../adapter""#.into());
    assert!(build_completion_request(&hostile, 64).is_err());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn command_is_loopback_single_capacity_and_contains_no_remote_code_flags() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 18123);
    let validated = validate_config(config).unwrap();
    let arguments = server_arguments(&validated.config, 18123)
        .into_iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(arguments
        .windows(2)
        .any(|pair| pair == ["--host", "127.0.0.1"]));
    assert!(arguments
        .windows(2)
        .any(|pair| pair == ["--decode-concurrency", "1"]));
    assert!(arguments
        .windows(2)
        .any(|pair| pair == ["--prompt-concurrency", "1"]));
    assert!(arguments
        .windows(2)
        .any(|pair| pair == ["--prompt-cache-size", "0"]));
    assert!(!arguments.iter().any(|value| value == "--trust-remote-code"));
    assert!(!arguments.iter().any(|value| value == "--adapter-path"));
    assert!(!arguments
        .iter()
        .any(|value| value.contains("http://") || value.contains("https://")));
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[test]
fn malicious_sse_is_rejected_fail_closed() {
    assert!(parse_sse_frame(b"data: not-json", Endpoint::Text).is_err());
    assert!(parse_sse_frame(
        br#"data: {"choices":[{"text":"a"},{"text":"b"}]}"#,
        Endpoint::Text,
    )
    .is_err());
    assert!(parse_sse_frame(
        br#"data: {"choices":[{"text":"","finish_reason":"remote_error"}]}"#,
        Endpoint::Text,
    )
    .is_err());
    assert!(parse_sse_frame(b"data: {\"choices\":[]}\ndata: [DONE]", Endpoint::Text,).is_err());
}

#[test]
fn chat_sse_preserves_reasoning_and_content_text() {
    let action = parse_sse_frame(
        br#"data: {"choices":[{"delta":{"reasoning_content":"think ","content":"answer"},"finish_reason":null}]}"#,
        Endpoint::Chat,
    )
    .unwrap();
    assert_eq!(
        action,
        FrameAction::Delta {
            text: "think answer".into(),
            terminal: None,
        }
    );

    let mlx32_action = parse_sse_frame(
        br#"data: {"choices":[{"delta":{"reasoning":"plan ","content":"done"},"finish_reason":null}]}"#,
        Endpoint::Chat,
    )
    .unwrap();
    assert_eq!(
        mlx32_action,
        FrameAction::Delta {
            text: "plan done".into(),
            terminal: None,
        }
    );
}

#[test]
fn oversized_and_crlf_sse_frames_are_bounded() {
    let mut buffer = BytesMut::from(vec![b'x'; MAX_SSE_FRAME_BYTES].as_slice());
    assert_eq!(append_sse_chunk(&mut buffer, b"x"), Err(SseFrameTooLarge));
    assert_eq!(buffer.len(), MAX_SSE_FRAME_BYTES);

    let mut crlf = BytesMut::from(&b"data: [DONE]\r\n\r\nrest"[..]);
    let frame = take_next_sse_frame(&mut crlf).unwrap().unwrap();
    assert_eq!(&frame[..], b"data: [DONE]");
    assert_eq!(&crlf[..], b"rest");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn wrong_model_and_embeddings_fail_before_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 18124);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    let model_cid = worker.model_cid().to_hex();
    assert_eq!(worker.supported_kinds(), &[JobSpecKind::Inference]);
    assert_eq!(worker.capacity_hint(), 1);
    assert_eq!(worker.advertised_capacity(), 1);
    assert_eq!(worker.bundle_format(), MLX_BUNDLE_FORMAT);
    assert_eq!(worker.bundle_metadata().context_length, Some(4096));
    assert_eq!(worker.runtime_attestation(), MLX_RUNTIME_ATTESTATION);
    assert_eq!(worker.hardware_acceptance(), MLX_HARDWARE_ACCEPTANCE);
    assert_eq!(worker.port_binding_status(), MLX_PORT_BINDING_STATUS);
    assert_ne!(worker.runtime_executable_sha256(), [0; 32]);

    let wrong = signed_job(JobSpec::Inference(inference(
        &ModelCid([0x99; 32]).to_hex(),
    )));
    assert!(matches!(
        worker.execute(wrong).await,
        Err(WorkerError::ArtifactUnavailable(_))
    ));

    let embedding = signed_job(JobSpec::Embedding(EmbeddingJobSpec {
        model_cid,
        input: vec!["hello".into()],
    }));
    assert!(matches!(
        worker.execute(embedding).await,
        Err(WorkerError::Unsupported {
            kind: JobSpecKind::Embedding
        })
    ));
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn configured_cid_mismatch_and_post_construction_bundle_mutation_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    let mut wrong = config.clone();
    wrong.model_cid = ModelCid([0x55; 32]);
    assert!(matches!(
        MlxWorker::new(NodeIdentity::generate(), wrong),
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("does not match")
    ));

    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    let model_cid = worker.model_cid().to_hex();
    let weights = bundle.join("model.safetensors");
    set_mode(&weights, 0o644);
    std::fs::write(&weights, b"mutated-after-construction").unwrap();
    set_mode(&weights, 0o444);
    let result = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await;
    assert!(matches!(
        result,
        Err(WorkerError::ArtifactUnavailable(message))
            if message.contains("changed after worker construction")
    ));
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn post_construction_runtime_mutation_fails_before_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    let model_cid = worker.model_cid().to_hex();
    set_mode(&runtime, 0o755);
    std::fs::write(
        &runtime,
        b"#!/bin/sh\n# mutated\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    set_mode(&runtime, 0o555);
    let result = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await;
    assert!(matches!(
        result,
        Err(WorkerError::Other(message))
            if message.contains("changed after worker construction")
    ));
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn mutable_and_symlinked_runtime_entry_points_are_rejected() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    set_mode(&runtime, 0o755);
    assert!(matches!(
        MlxWorker::new(NodeIdentity::generate(), config.clone()),
        Err(WorkerError::Other(message)) if message.contains("write permission")
    ));

    set_mode(&runtime, 0o555);
    let runtime_link = directory.path().join("runtime-link");
    symlink(&runtime, &runtime_link).unwrap();
    let mut linked = config;
    linked.server_binary_path = runtime_link;
    assert!(matches!(
        MlxWorker::new(NodeIdentity::generate(), linked),
        Err(WorkerError::Other(message)) if message.contains("symbolic link")
    ));
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn preload_rejects_occupied_fixed_port_before_loaded_state() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), port);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    let result = worker.preload().await;
    assert!(matches!(
        result,
        Err(WorkerError::Other(message)) if message.contains("failed to start")
    ));
    drop(listener);
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn success_uses_default_model_and_signs_streamed_output() {
    let (port, bodies, fixture) = start_fixture(FixtureMode::Success).await;
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    attach_fixture_server(&worker, port).await;
    worker
        .preload()
        .await
        .expect("an already ready backend must satisfy preload");
    let model_cid = worker.model_cid().to_hex();
    let (handle, mut stream) = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await
        .unwrap();

    let output = stream.next().await.unwrap();
    assert!(matches!(
        output,
        JobEvent::Output(OutputChunk { ref data, seq: 0, .. }) if data == "hello"
    ));
    let final_event = stream.next().await.unwrap();
    let JobEvent::Final { result, error } = final_event else {
        panic!("expected final event");
    };
    assert_eq!(result.completion, Completion::Stop);
    assert_eq!(result.output_chunk_count, 1);
    assert_eq!(result.metrics.extra["hardware_acceptance"], "unverified");
    assert_eq!(
        result.metrics.extra["bundle_root_algorithm"],
        MLX_BUNDLE_ROOT_ALGORITHM
    );
    assert_eq!(result.metrics.extra["bundle_cid"], model_cid);
    assert_eq!(
        result.metrics.extra["runtime_attestation"],
        MLX_RUNTIME_ATTESTATION
    );
    assert!(error.is_none());
    assert!(stream.next().await.is_none());
    let receipt = handle.finish().await.unwrap();
    assert_eq!(receipt.result.output_commitment, result.output_commitment);

    let captured = bodies.lock().await;
    assert_eq!(captured.len(), 1);
    assert!(captured.iter().all(|body| body["model"] == "default_model"));
    fixture.abort();
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn capacity_is_one_and_cancellation_kills_the_server() {
    let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, mut config) = create_runtime_and_bundle(directory.path(), 0);
    config.per_request_idle_timeout = Duration::from_secs(2);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    attach_fixture_server(&worker, port).await;
    let model_cid = worker.model_cid().to_hex();
    let (handle, mut first_stream) = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await
        .unwrap();

    let second = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await;
    assert!(matches!(second, Err(WorkerError::Capacity)));

    handle.cancel();
    let event = timeout(Duration::from_secs(1), first_stream.next())
        .await
        .expect("cancellation must be bounded")
        .expect("final event");
    assert!(matches!(
        event,
        JobEvent::Final {
            result: JobResult {
                completion: Completion::Cancelled,
                ..
            },
            error: None,
        }
    ));
    let slot = worker.inner.server.lock().await;
    assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
    drop(slot);
    fixture.abort();
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn dropping_stream_invalidates_and_kills_the_server() {
    let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    attach_fixture_server(&worker, port).await;
    let model_cid = worker.model_cid().to_hex();
    let (_handle, stream) = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await
        .unwrap();

    drop(stream);
    let slot = worker.inner.server.lock().await;
    assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
    drop(slot);
    fixture.abort();
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
async fn hung_sse_hits_idle_deadline_and_invalidates_server() {
    let (port, _, fixture) = start_fixture(FixtureMode::Hang).await;
    let directory = tempfile::tempdir().unwrap();
    let (runtime, bundle, config) = create_runtime_and_bundle(directory.path(), 0);
    let worker = MlxWorker::new(NodeIdentity::generate(), config).unwrap();
    attach_fixture_server(&worker, port).await;
    let model_cid = worker.model_cid().to_hex();
    let (_handle, mut stream) = worker
        .execute(signed_job(JobSpec::Inference(inference(&model_cid))))
        .await
        .unwrap();

    let event = timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("idle watchdog must be bounded")
        .expect("final event");
    assert!(matches!(
        event,
        JobEvent::Final {
            result: JobResult {
                completion: Completion::Error,
                ..
            },
            error: Some(_),
        }
    ));
    let slot = worker.inner.server.lock().await;
    assert!(slot.as_ref().is_some_and(|server| server.is_failed()));
    drop(slot);
    fixture.abort();
    set_mode(&runtime, 0o755);
    thaw_for_cleanup(&bundle);
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn non_apple_platform_fails_configuration_before_spawn() {
    let config = MlxConfig::new(
        PathBuf::from("/does/not/matter"),
        PathBuf::from("/does/not/matter"),
        ModelCid([1; 32]),
        18124,
    );
    assert!(matches!(
        MlxWorker::new(NodeIdentity::generate(), config),
        Err(WorkerError::Other(message)) if message.contains("macOS on Apple Silicon")
    ));
}
