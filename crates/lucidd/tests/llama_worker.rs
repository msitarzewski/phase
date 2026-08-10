// SPDX-License-Identifier: AGPL-3.0-or-later

//! Integration tests for `LlamaCppWorker` against the in-tree
//! test-only llama-server protocol fixture (see `tests/fixtures/`).
//!
//! These tests deliberately don't require a real `llama-server` or any
//! GGUF model on disk — the fixture emits SSE frames in the same
//! shape the real server uses, plus configurable failure modes
//! (`FAKE_LLAMA_CRASH_AFTER_MS`, `FAKE_LLAMA_HANG_AFTER`, etc.). The
//! fixture is compiled into this integration-test harness and reached by a
//! temporary re-exec wrapper, so it is never an installable production
//! binary. The real-binary path is exercised separately by `real_llama_server`
//! (gated on `#[ignore]` + the `LLAMA_SERVER_PATH` env var).
//!
//! Each test:
//! 1. Allocates a temp dir; touches a `dummy.gguf` so the model-file
//!    existence check passes. The protocol fixture ignores `--model`.
//! 2. Constructs `LlamaCppConfig { server_binary_path: <fixture-launcher>, … }`.
//! 3. Builds a `SignedManifest<JobSpec>` and drives `worker.execute()`.
//! 4. Collects the resulting [`JobEvent`]s and asserts on shape.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use lucidd::{LlamaCppConfig, LlamaCppWorker};
use phase_identity::NodeIdentity;
use phase_manifest::ManifestBuilder;
use phase_protocol::{
    Completion, EmbeddingJobSpec, InferenceJobSpec, JobEvent, JobSpec, SamplingParams,
    SignedManifest, Worker,
};

#[path = "fixtures/fake_llama_server.rs"]
mod fake_llama_server;

/// Pick a port that's free *right now*. The fixture process will re-bind it
/// almost immediately; on the tiny window between drop and re-bind we
/// accept the rare flake.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

/// Quote arbitrary UTF-8 text for a POSIX shell single-quoted word.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Build an ephemeral subprocess launcher for the fixture entrypoint baked
/// into this integration-test executable. `LlamaCppWorker` can therefore
/// exercise its real `Command` supervision path without publishing or
/// installing a test server binary as part of the `lucidd` package.
fn fixture_launcher(dir: &Path) -> PathBuf {
    let test_exe = std::env::current_exe().expect("resolve llama_worker test executable");
    let test_exe = test_exe
        .to_str()
        .expect("llama_worker test executable path must be UTF-8");
    let launcher = dir.join("llama-server-test-fixture");
    let script = format!(
        r#"#!/bin/sh
port=""
host="127.0.0.1"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --port)
            [ "$#" -ge 2 ] || exit 64
            port="$2"
            shift 2
            ;;
        --host)
            [ "$#" -ge 2 ] || exit 64
            host="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
[ -n "$port" ] || exit 64
export PHASE_FAKE_LLAMA_SERVER_PROCESS=1
export PHASE_FAKE_LLAMA_PORT="$port"
export PHASE_FAKE_LLAMA_HOST="$host"
exec {} --exact fixture_server_process_entrypoint --ignored --nocapture
"#,
        shell_quote(test_exe)
    );
    std::fs::write(&launcher, script).expect("write fixture subprocess launcher");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
            .expect("make fixture subprocess launcher executable");
    }
    launcher
}

/// Internal subprocess entrypoint. It is ignored during ordinary test runs
/// and only entered by `fixture_launcher` with the explicit process marker.
#[test]
#[ignore = "internal llama-server fixture subprocess entrypoint"]
fn fixture_server_process_entrypoint() {
    if std::env::var_os("PHASE_FAKE_LLAMA_SERVER_PROCESS").is_none() {
        eprintln!("internal fixture entrypoint unavailable outside its launcher");
        return;
    }
    fake_llama_server::run();
}

fn make_manifest(model_id: &str, prompt: &str) -> SignedManifest<JobSpec> {
    let id = NodeIdentity::generate();
    let job_spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: model_id.to_string(),
        messages: vec![],
        prompt: Some(prompt.to_string()),
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: Some(32),
        stream: true,
    });
    ManifestBuilder::new(job_spec)
        .sign_with(&id)
        .expect("sign manifest")
}

struct TestModel {
    _dir: tempfile::TempDir,
    config: LlamaCppConfig,
    model_id: String,
}

fn setup(model_id: &str) -> TestModel {
    let dir = tempfile::tempdir().expect("temp dir");
    let model_path = dir.path().join(format!("{model_id}.gguf"));
    std::fs::write(&model_path, b"fake").expect("touch model");
    let port = free_port();
    let config = LlamaCppConfig {
        server_binary_path: fixture_launcher(dir.path()),
        model_dir: dir.path().to_path_buf(),
        default_n_gpu_layers: 0,
        default_context_size: 2048,
        server_port_range: port..(port + 1),
        max_loaded_models: 3,
        model_load_timeout: Duration::from_secs(10),
        per_request_idle_timeout: Duration::from_secs(5),
        extra_env: Vec::new(),
    };
    TestModel {
        _dir: dir,
        config,
        model_id: model_id.to_string(),
    }
}

/// Drive one full inference to its `Final` event so the model ends up
/// resident in the worker. Returns the terminal completion + any error.
async fn run_to_final(
    worker: &LlamaCppWorker,
    model_id: &str,
) -> Result<(Completion, Option<String>), String> {
    let manifest = make_manifest(model_id, "Hello.");
    let (_handle, mut stream) = worker.execute(manifest).await.map_err(|e| e.to_string())?;
    while let Some(ev) = stream.next().await {
        if let JobEvent::Final { result, error } = ev {
            return Ok((result.completion, error));
        }
    }
    Err("stream ended without Final".to_string())
}

/// SEC-07: build a worker whose model_dir holds several distinct GGUFs and
/// whose port range / cap can be tuned per test. Tokens emit fast so each
/// `execute` finishes promptly.
fn multi_model_worker(
    model_ids: &[&str],
    max_loaded_models: usize,
    port_range_span: u16,
) -> (tempfile::TempDir, LlamaCppWorker) {
    let dir = tempfile::tempdir().expect("temp dir");
    for id in model_ids {
        std::fs::write(dir.path().join(format!("{id}.gguf")), b"fake").expect("touch model");
    }
    let base = free_port();
    let config = LlamaCppConfig {
        server_binary_path: fixture_launcher(dir.path()),
        model_dir: dir.path().to_path_buf(),
        default_n_gpu_layers: 0,
        default_context_size: 2048,
        server_port_range: base..(base + port_range_span),
        max_loaded_models,
        model_load_timeout: Duration::from_secs(10),
        per_request_idle_timeout: Duration::from_secs(5),
        extra_env: vec![
            ("FAKE_LLAMA_TOKENS".to_string(), "a,b".to_string()),
            ("FAKE_LLAMA_DELAY_MS".to_string(), "1".to_string()),
        ],
    };
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), config);
    (dir, worker)
}

#[tokio::test]
async fn lru_eviction_at_cap_and_port_reused_on_reload() {
    // SEC-07: cap = 2, three distinct models, three ports available.
    // Loading the third evicts the LRU (the first). Reloading the evicted
    // model then succeeds — its port was freed and is reacquired cleanly.
    let (_dir, worker) = multi_model_worker(&["m-a", "m-b", "m-c"], 2, 3);

    let (c, e) = run_to_final(&worker, "m-a").await.expect("load m-a");
    assert_eq!(c, Completion::Stop, "m-a error: {e:?}");
    let (c, _) = run_to_final(&worker, "m-b").await.expect("load m-b");
    assert_eq!(c, Completion::Stop);
    // Third load is at cap → LRU (m-a) evicted, its child killed + port freed.
    let (c, _) = run_to_final(&worker, "m-c").await.expect("load m-c");
    assert_eq!(c, Completion::Stop);

    // Reload the evicted model. If its port had leaked, the range (3 ports,
    // 2 live) would still have room — but eviction frees the port so this
    // is a clean re-spawn either way.
    let (c, e) = run_to_final(&worker, "m-a").await.expect("reload m-a");
    assert_eq!(c, Completion::Stop, "reload m-a error: {e:?}");
}

#[tokio::test]
async fn port_range_exhaustion_returns_capacity() {
    // SEC-07: a single-port range with a cap of 2 lets the first model
    // bind the only port; loading a SECOND distinct model (still resident,
    // not evicted because we're under the model cap) finds no free port and
    // must return Capacity rather than wrapping onto the live port.
    let (_dir, worker) = multi_model_worker(&["only-a", "only-b"], 2, 1);

    let (c, e) = run_to_final(&worker, "only-a").await.expect("load only-a");
    assert_eq!(c, Completion::Stop, "only-a error: {e:?}");

    // only-b: under the model cap, so no eviction; the one port is taken.
    let manifest = make_manifest("only-b", "Hello.");
    match worker.execute(manifest).await {
        Err(phase_protocol::WorkerError::Capacity) => {}
        Err(other) => panic!("expected WorkerError::Capacity, got {other:?}"),
        Ok(_) => panic!("expected Capacity when port range exhausted, got Ok"),
    }
}

#[tokio::test]
async fn happy_path_streams_tokens_and_signs_receipt() {
    let setup = setup("happy");
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);
    let manifest = make_manifest(&setup.model_id, "Hello.");

    let (handle, mut stream) = worker.execute(manifest).await.expect("dispatch");

    let mut tokens = Vec::new();
    let mut final_completion: Option<Completion> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            JobEvent::Output(chunk) => {
                tokens.push(String::from_utf8_lossy(&chunk.data).into_owned());
            }
            JobEvent::Final { result, error } => {
                assert!(error.is_none(), "unexpected error: {error:?}");
                final_completion = Some(result.completion.clone());
                assert_eq!(result.output_chunk_count as usize, tokens.len());
            }
            _ => {}
        }
    }
    assert_eq!(final_completion, Some(Completion::Stop));
    assert!(!tokens.is_empty(), "expected at least one token");
    let joined: String = tokens.concat();
    // Default fixture token list is "Hello,, ,world,!".
    assert!(joined.contains("Hello"));
    assert!(joined.contains("world"));

    // Receipt should be deliverable.
    let receipt = handle.finish().await.expect("receipt");
    assert_eq!(receipt.result.completion, Completion::Stop);
    // Commitment is a non-empty hash.
    assert_ne!(receipt.result.output_commitment, [0u8; 32]);
}

#[tokio::test]
async fn cancellation_mid_stream_yields_cancelled_completion() {
    let mut setup = setup("cancel");
    // Slow tokens so we definitely cancel before the stream finishes.
    // Per-spawn env (not process env) keeps this test isolated when run
    // concurrently with other tests in the file.
    setup.config.per_request_idle_timeout = Duration::from_secs(10);
    setup.config.extra_env = vec![
        ("FAKE_LLAMA_DELAY_MS".to_string(), "200".to_string()),
        (
            "FAKE_LLAMA_TOKENS".to_string(),
            "a,b,c,d,e,f,g,h".to_string(),
        ),
    ];
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);
    let manifest = make_manifest(&setup.model_id, "Hello.");

    let (handle, mut stream) = worker.execute(manifest).await.expect("dispatch");

    // Take one token, then cancel.
    let first = stream.next().await;
    assert!(matches!(first, Some(JobEvent::Output(_))));
    handle.cancel();

    let mut saw_final = false;
    let mut final_completion = None;
    while let Some(ev) = stream.next().await {
        if let JobEvent::Final { result, .. } = ev {
            saw_final = true;
            final_completion = Some(result.completion.clone());
            break;
        }
    }
    assert!(saw_final, "expected a Final event after cancel");
    assert_eq!(final_completion, Some(Completion::Cancelled));

    // Receipt still arrives.
    let _receipt = handle.finish().await.expect("receipt after cancel");
}

#[tokio::test]
async fn hang_detection_aborts_request_and_marks_model_suspect() {
    let setup = setup("hang");
    // Tight idle window so the test runs fast.
    let mut cfg = setup.config;
    cfg.per_request_idle_timeout = Duration::from_secs(2);
    cfg.extra_env = vec![
        ("FAKE_LLAMA_HANG_AFTER".to_string(), "2".to_string()),
        ("FAKE_LLAMA_DELAY_MS".to_string(), "20".to_string()),
    ];
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), cfg);
    let manifest = make_manifest(&setup.model_id, "Hello.");

    let (_handle, mut stream) = worker.execute(manifest).await.expect("dispatch");
    let mut final_event: Option<JobEvent> = None;
    let start = std::time::Instant::now();
    while let Some(ev) = stream.next().await {
        if let JobEvent::Final { .. } = ev {
            final_event = Some(ev);
            break;
        }
        if start.elapsed() > Duration::from_secs(15) {
            panic!("never saw Final event after hang");
        }
    }
    let final_event = final_event.expect("Final event");
    if let JobEvent::Final { result, error } = final_event {
        assert_eq!(result.completion, Completion::Error);
        let err = error.unwrap_or_default();
        assert!(
            err.contains("hang") || err.contains("no token"),
            "expected hang-related error, got: {err}"
        );
    }
}

#[tokio::test]
async fn crash_during_load_surfaces_as_dispatch_error() {
    let setup = setup("crash-at-load");
    // Crash before /health ever returns 200.
    let mut cfg = setup.config;
    cfg.model_load_timeout = Duration::from_millis(800);
    cfg.extra_env = vec![("FAKE_LLAMA_FAIL_HEALTH".to_string(), "1".to_string())];
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), cfg);
    let manifest = make_manifest(&setup.model_id, "Hello.");

    let result = worker.execute(manifest).await;
    let msg = match result {
        Ok(_) => panic!("expected load failure, got success"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("health") || msg.contains("did not become healthy"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn missing_model_file_returns_artifact_unavailable() {
    let setup = setup("present"); // Real file on disk.
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);

    // Request a different model alias that has no `.gguf` on disk.
    let manifest = make_manifest("nope-not-here", "Hello.");
    let result = worker.execute(manifest).await;
    let msg = match result {
        Ok(_) => panic!("expected missing-model failure, got success"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("artifact") || msg.contains("not found"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn embedding_streams_vectors_and_signs_receipt() {
    // Drive the embedding path against the fake server's canned `/embedding`
    // handler. Two inputs → two "embedding"-kind chunks, one per input,
    // ordered by `seq`, each decoding to the canned Vec<f32>. The terminal
    // Final carries a real commitment + Completion::Stop.
    let setup = setup("embed");
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);

    let id = NodeIdentity::generate();
    let job_spec = JobSpec::Embedding(EmbeddingJobSpec {
        model_cid: setup.model_id.clone(),
        input: vec!["first".to_string(), "second".to_string()],
    });
    let manifest = phase_manifest::ManifestBuilder::new(job_spec)
        .sign_with(&id)
        .expect("sign manifest");

    let (handle, mut stream) = worker.execute(manifest).await.expect("dispatch embedding");

    let mut vectors: Vec<(u64, Vec<f32>)> = Vec::new();
    let mut final_completion: Option<Completion> = None;
    while let Some(ev) = stream.next().await {
        match ev {
            JobEvent::Output(chunk) => {
                assert_eq!(chunk.kind, "embedding");
                let v: Vec<f32> =
                    serde_json::from_slice(&chunk.data).expect("decode embedding vector");
                vectors.push((chunk.seq, v));
            }
            JobEvent::Final { result, error } => {
                assert!(error.is_none(), "unexpected error: {error:?}");
                final_completion = Some(result.completion.clone());
                assert_eq!(result.output_chunk_count as usize, vectors.len());
            }
            _ => {}
        }
    }
    assert_eq!(final_completion, Some(Completion::Stop));
    assert_eq!(vectors.len(), 2, "expected one chunk per input");
    // Ordered by seq, matching input order.
    vectors.sort_by_key(|(seq, _)| *seq);
    assert_eq!(vectors[0].0, 0);
    assert_eq!(vectors[1].0, 1);
    assert_eq!(vectors[0].1, vec![0.1f32, 0.2, 0.3, 0.4]);

    let receipt = handle.finish().await.expect("receipt");
    assert_eq!(receipt.result.completion, Completion::Stop);
    assert_ne!(receipt.result.output_commitment, [0u8; 32]);
}

// -----------------------------------------------------------------------
// Optional: real llama-server integration test. Marked `#[ignore]` so
// ordinary `cargo test` reports the external hardware gate without trying
// it. An explicit run fails (rather than silently passing) unless both
// required paths are supplied.
// -----------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn real_llama_server_smoke_test_requires_explicit_hardware_env() {
    let bin = std::env::var("LLAMA_SERVER_PATH").expect(
        "real llama hardware gate unavailable: set LLAMA_SERVER_PATH and LLAMA_TEST_MODEL_PATH",
    );
    let model = std::env::var("LLAMA_TEST_MODEL_PATH").expect(
        "real llama hardware gate unavailable: set LLAMA_SERVER_PATH and LLAMA_TEST_MODEL_PATH",
    );

    let model_path = PathBuf::from(&model);
    let model_dir = model_path
        .parent()
        .expect("model has a parent dir")
        .to_path_buf();
    let model_id = model_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model")
        .to_string();

    let port = free_port();
    let config = LlamaCppConfig {
        server_binary_path: PathBuf::from(bin),
        model_dir,
        default_n_gpu_layers: i32::MAX,
        default_context_size: 2048,
        server_port_range: port..(port + 1),
        max_loaded_models: 3,
        model_load_timeout: Duration::from_secs(120),
        per_request_idle_timeout: Duration::from_secs(60),
        extra_env: Vec::new(),
    };
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), config);
    let manifest = make_manifest(&model_id, "Say hi in five words.");
    let (_handle, mut stream) = worker.execute(manifest).await.expect("dispatch real");
    let mut got_token = false;
    let mut got_final = false;
    while let Some(ev) = stream.next().await {
        match ev {
            JobEvent::Output(_) => got_token = true,
            JobEvent::Final { .. } => {
                got_final = true;
                break;
            }
            _ => {}
        }
    }
    assert!(got_token, "expected at least one token from real server");
    assert!(got_final, "expected Final from real server");
}
