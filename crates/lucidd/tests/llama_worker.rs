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
use std::sync::{Mutex, OnceLock};
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

/// On macOS, parallel fixture processes have inherited another test's
/// temporary listening socket during startup. Isolate each probe/spawn/use
/// scenario through worker teardown; disjoint port numbers alone cannot
/// prevent an inherited descriptor from keeping a probe bound.
#[cfg(target_os = "macos")]
async fn fixture_process_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}

/// Keep fixture ranges disjoint across parallel tests and probe every slot.
/// Use ports below the standard ephemeral ranges so outgoing health/request
/// connections do not claim a fixture's not-yet-started adjacent server port.
/// The external close-to-child-bind race remains because llama-server cannot
/// inherit listeners, but another test cannot allocate an adjacent range here.
fn free_port_range(span: u16) -> std::ops::Range<u16> {
    static RESERVED: OnceLock<Mutex<std::collections::HashSet<u16>>> = OnceLock::new();
    assert!(span > 0);
    let mut reserved = RESERVED.get_or_init(Mutex::default).lock().unwrap();
    for _ in 0..128 {
        let first = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let start = 20_000 + first.local_addr().unwrap().port() % 10_000;
        let Some(end) = start.checked_add(span) else {
            continue;
        };
        let range = start..end;
        if range.clone().any(|port| reserved.contains(&port)) {
            continue;
        }
        let remaining = range
            .clone()
            .map(|port| TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)))
            .collect::<std::io::Result<Vec<_>>>();
        if let Ok(_listeners) = remaining {
            reserved.extend(range.clone());
            return range;
        }
    }
    panic!("could not allocate a disjoint fixture port range");
}

fn free_port() -> u16 {
    free_port_range(1).start
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
    // Opt-in persistent diagnostics survive TempDir cleanup on a failed test.
    let diagnostics_dir = std::env::var_os("PHASE_TEST_DIAGNOSTICS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    std::fs::create_dir_all(&diagnostics_dir).expect("create fixture diagnostics directory");
    let diagnostics_prefix = diagnostics_dir.join("fixture");
    let records_prefix = dir.join("fixture");
    let script = format!(
        r#"#!/bin/sh
records={records}
argv_tmp="$records.argv.$$"
for arg in "$@"; do
    printf '%s\n' "$arg"
done > "$argv_tmp"
port=""
host="127.0.0.1"
model=""
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
        --model)
            [ "$#" -ge 2 ] || exit 64
            model="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
[ -n "$port" ] || exit 64
mv "$argv_tmp" "$records.$port.argv"
printf '%s\n' "$model" >> "$records.spawns"
export PHASE_FAKE_LLAMA_SERVER_PROCESS=1
export PHASE_FAKE_LLAMA_PORT="$port"
export PHASE_FAKE_LLAMA_HOST="$host"
export PHASE_FAKE_LLAMA_MODEL="$model"
exec {exe} --exact fixture_server_process_entrypoint --ignored --nocapture 2>> {diag}."$port".log
"#,
        records = shell_quote(records_prefix.to_str().expect("UTF-8 fixture path")),
        exe = shell_quote(test_exe),
        diag = shell_quote(diagnostics_prefix.to_str().expect("UTF-8 fixture path"))
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
        ..Default::default()
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
    let port_range = free_port_range(port_range_span);
    let config = LlamaCppConfig {
        server_binary_path: fixture_launcher(dir.path()),
        model_dir: dir.path().to_path_buf(),
        default_n_gpu_layers: 0,
        default_context_size: 2048,
        server_port_range: port_range,
        max_loaded_models,
        model_load_timeout: Duration::from_secs(10),
        per_request_idle_timeout: Duration::from_secs(5),
        extra_env: vec![
            ("FAKE_LLAMA_TOKENS".to_string(), "a,b".to_string()),
            ("FAKE_LLAMA_DELAY_MS".to_string(), "1".to_string()),
        ],
        ..Default::default()
    };
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), config);
    (dir, worker)
}

#[tokio::test]
async fn lru_eviction_at_cap_and_port_reused_on_reload() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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

fn make_chat_manifest(model_id: &str) -> SignedManifest<JobSpec> {
    let spec = JobSpec::Inference(InferenceJobSpec {
        model_cid: model_id.into(),
        messages: vec![phase_protocol::ChatMessage {
            role: phase_protocol::ChatRole::User,
            content: "Hi.".into(),
            images: Vec::new(),
        }],
        prompt: Some("must not replace the conversation".into()),
        resume_from: None,
        sampling: SamplingParams::default(),
        max_tokens: Some(32),
        stream: true,
    });
    ManifestBuilder::new(spec)
        .sign_with(&NodeIdentity::generate())
        .unwrap()
}

#[tokio::test]
async fn model_chat_template_drives_completion_and_preserves_signed_receipt() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    let mut setup = setup("chat-template");
    let expected = format!(
        "<fixture-template>{}</fixture-template>",
        serde_json::json!([{"role": "user", "content": "Hi."}])
    );
    setup
        .config
        .extra_env
        .push(("FAKE_LLAMA_REQUIRED_PROMPT".into(), expected));
    let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);
    let manifest = make_chat_manifest(&setup.model_id);
    let manifest_hash = manifest.manifest_hash().unwrap();
    let (handle, mut stream) = worker.execute(manifest).await.expect("template dispatch");
    let mut chunks = 0;
    while let Some(event) = stream.next().await {
        match event {
            JobEvent::Output(_) => chunks += 1,
            JobEvent::Final { result, error } => {
                assert!(
                    error.is_none(),
                    "unexpected template execution error: {error:?}"
                );
                assert_eq!(result.completion, Completion::Stop);
            }
            _ => {}
        }
    }
    assert!(chunks > 0);
    let receipt = handle.finish().await.expect("template job receipt");
    assert_eq!(receipt.result.job_spec_hash, manifest_hash);
    assert_eq!(receipt.result.output_chunk_count, chunks);
    assert_ne!(receipt.result.output_commitment, [0; 32]);
}

#[tokio::test]
async fn chat_template_failures_fail_closed_while_raw_prompts_bypass_template() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    for mode in ["invalid", "oversize", "hang"] {
        let mut setup = setup(&format!("template-{mode}"));
        setup
            .config
            .extra_env
            .push(("FAKE_LLAMA_TEMPLATE_MODE".into(), mode.into()));
        setup.config.per_request_idle_timeout = Duration::from_millis(500);
        let worker = LlamaCppWorker::new(NodeIdentity::generate(), setup.config);
        let result = worker.execute(make_chat_manifest(&setup.model_id)).await;
        let error = match result {
            Ok(_) => panic!("{mode} template must not dispatch fallback prompt"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("chat template"),
            "{mode}: {error}"
        );
        if mode == "invalid" {
            let result = run_to_final(&worker, &setup.model_id).await.unwrap();
            assert_eq!(result.0, Completion::Stop);
            assert!(result.1.is_none());
        }
    }
}

#[tokio::test]
async fn cancellation_mid_stream_yields_cancelled_completion() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
// Wave 1 Track A: loopback lockdown, lifecycle, and RPC guardrails
// -----------------------------------------------------------------------

/// Direct loopback client standing in for a browser page or any other local
/// process that did not spawn the child (no proxy, no redirects).
fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("loopback client")
}

/// Every `--model` the fixture launcher has been asked to spawn, as file
/// stems, in spawn order.
fn spawned_models(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("fixture.spawns"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            Path::new(line)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

/// The argv of the most recent fixture spawn on `port`.
fn spawn_argv(dir: &Path, port: u16) -> Vec<String> {
    std::fs::read_to_string(dir.join(format!("fixture.{port}.argv")))
        .expect("fixture recorded argv")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Wait until nothing listens on `port` any more (the child was killed and
/// the kernel closed its listener).
async fn wait_port_bindable(port: u16, within: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < within {
        if TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

struct FlexibleWorker {
    dir: tempfile::TempDir,
    worker: LlamaCppWorker,
    ports: std::ops::Range<u16>,
}

fn flexible_worker(
    model_ids: &[&str],
    max_loaded_models: usize,
    port_range_span: u16,
    extra_env: &[(&str, &str)],
    model_load_timeout: Duration,
    customize: impl FnOnce(&mut LlamaCppConfig),
) -> FlexibleWorker {
    let dir = tempfile::tempdir().expect("temp dir");
    for id in model_ids {
        std::fs::write(dir.path().join(format!("{id}.gguf")), b"fake").expect("touch model");
    }
    let ports = free_port_range(port_range_span);
    let mut config = LlamaCppConfig {
        server_binary_path: fixture_launcher(dir.path()),
        model_dir: dir.path().to_path_buf(),
        default_n_gpu_layers: 0,
        default_context_size: 2048,
        server_port_range: ports.clone(),
        max_loaded_models,
        model_load_timeout,
        per_request_idle_timeout: Duration::from_secs(5),
        extra_env: [("FAKE_LLAMA_TOKENS", "a,b"), ("FAKE_LLAMA_DELAY_MS", "1")]
            .iter()
            .chain(extra_env)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ..Default::default()
    };
    customize(&mut config);
    FlexibleWorker {
        dir,
        worker: LlamaCppWorker::new(NodeIdentity::generate(), config),
        ports,
    }
}

#[tokio::test]
async fn llama_server_rejects_unauthenticated_loopback_requests() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A1: the worker's own requests carry the per-spawn bearer key, while any
    // other loopback caller (a DNS-rebound page, a local process) is refused.
    let fx = flexible_worker(&["locked"], 1, 1, &[], Duration::from_secs(10), |_| {});
    let (completion, error) = run_to_final(&fx.worker, "locked").await.expect("dispatch");
    assert_eq!(
        completion,
        Completion::Stop,
        "authenticated worker path: {error:?}"
    );

    let port = fx.ports.start;
    let client = loopback_client();
    let base = format!("http://127.0.0.1:{port}");
    let unauthenticated = client
        .post(format!("{base}/completion"))
        .json(&serde_json::json!({"prompt": "exfiltrate", "n_predict": 8, "stream": false}))
        .send()
        .await
        .expect("loopback request");
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let wrong_key = client
        .post(format!("{base}/completion"))
        .bearer_auth("0".repeat(64))
        .json(&serde_json::json!({"prompt": "exfiltrate"}))
        .send()
        .await
        .expect("loopback request");
    assert_eq!(wrong_key.status(), reqwest::StatusCode::UNAUTHORIZED);
    for path in ["/embedding", "/apply-template"] {
        let response = client
            .post(format!("{base}{path}"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("loopback request");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let slots = client.get(format!("{base}/slots")).send().await.unwrap();
    assert_eq!(slots.status(), reqwest::StatusCode::UNAUTHORIZED);
    // Liveness stays public, exactly as in llama-server.
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert!(health.status().is_success());

    // The key travelled through the environment: argv carries the hardening
    // flags but no key material.
    let argv = spawn_argv(fx.dir.path(), port);
    let joined = argv.join(" ");
    assert!(
        joined.contains("--cors-origins localhost --no-cors-credentials --no-webui --no-slots"),
        "{joined}"
    );
    assert!(!argv.iter().any(|arg| arg == "--api-key"), "{joined}");
    assert!(
        !argv
            .iter()
            .any(|arg| arg.len() == 64 && arg.bytes().all(|b| b.is_ascii_hexdigit())),
        "key-shaped argument leaked into argv: {joined}"
    );
}

#[tokio::test]
async fn hang_detection_kills_child_and_releases_its_port() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A2: a single-port pool. If a hung child were merely flagged, its port
    // would stay reserved and the reload below would fail with Capacity.
    let fx = flexible_worker(
        &["wedge"],
        1,
        1,
        &[("FAKE_LLAMA_HANG_AFTER", "1")],
        Duration::from_secs(10),
        |config| config.per_request_idle_timeout = Duration::from_millis(800),
    );
    let port = fx.ports.start;
    let (completion, error) = run_to_final(&fx.worker, "wedge").await.expect("dispatch");
    assert_eq!(completion, Completion::Error);
    assert!(error.unwrap_or_default().contains("hang"));

    assert!(
        wait_port_bindable(port, Duration::from_secs(5)).await,
        "hung llama-server must be killed, not left holding port {port}"
    );

    // The port is back in the pool: the replacement cold-loads on it.
    let (completion, _) = run_to_final(&fx.worker, "wedge")
        .await
        .expect("reload after hang must not hit Capacity");
    assert_eq!(
        completion,
        Completion::Error,
        "fixture still hangs by design"
    );
    assert_eq!(spawned_models(fx.dir.path()), ["wedge", "wedge"]);
}

#[tokio::test]
async fn failed_cold_load_never_evicts_a_warm_model() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A2: cap = 1. Before the fix the broken load evicted `warm` first.
    let fx = flexible_worker(
        &["warm", "broken"],
        1,
        2,
        &[("FAKE_LLAMA_FAIL_HEALTH_FOR_MODEL", "broken")],
        Duration::from_millis(800),
        |_| {},
    );
    let (completion, error) = run_to_final(&fx.worker, "warm").await.expect("load warm");
    assert_eq!(completion, Completion::Stop, "{error:?}");

    let failure = match fx.worker.execute(make_manifest("broken", "Hello.")).await {
        Ok(_) => panic!("broken model must fail its health gate"),
        Err(error) => error.to_string(),
    };
    assert!(failure.contains("health"), "{failure}");

    let (completion, error) = run_to_final(&fx.worker, "warm").await.expect("warm hit");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    assert_eq!(
        spawned_models(fx.dir.path()),
        ["warm", "broken"],
        "warm model must still be the original resident process"
    );
}

#[tokio::test]
async fn cancelled_cold_load_never_evicts_warm_model_or_leaks_its_port() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A2: cap = 1 with exactly cap + 1 ports. A leaked port from the
    // cancelled load would make the final load fail with Capacity.
    let fx = flexible_worker(
        &["warm", "cold", "third"],
        1,
        2,
        &[("FAKE_LLAMA_WARMUP_MS", "1200")],
        Duration::from_secs(10),
        |_| {},
    );
    let (completion, error) = run_to_final(&fx.worker, "warm").await.expect("load warm");
    assert_eq!(completion, Completion::Stop, "{error:?}");

    // Drop the cold load mid-warmup, as a disconnecting client would.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        fx.worker.execute(make_manifest("cold", "Hello.")),
    )
    .await;
    assert!(cancelled.is_err(), "cold load should still be warming up");

    let started = std::time::Instant::now();
    let (completion, error) = run_to_final(&fx.worker, "warm").await.expect("warm hit");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "warm model was reloaded instead of served warm"
    );

    // The cancelled child was killed with its future; once the kernel has
    // closed its listener the port must be allocatable again.
    for port in fx.ports.clone() {
        if spawn_argv(fx.dir.path(), port)
            .iter()
            .any(|a| a.ends_with("cold.gguf"))
        {
            assert!(wait_port_bindable(port, Duration::from_secs(5)).await);
        }
    }
    let (completion, error) = run_to_final(&fx.worker, "third")
        .await
        .expect("cancelled load must not leak its port reservation");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    assert_eq!(spawned_models(fx.dir.path()), ["warm", "cold", "third"]);
}

#[tokio::test]
async fn rpc_offload_follows_live_cluster_gate() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A3: RPC layer offload runs only while cluster_enabled is on; turning it
    // off refuses jobs and tears down the RPC-backed child.
    let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gate_state = enabled.clone();
    let fx = flexible_worker(&["sliced"], 1, 1, &[], Duration::from_secs(10), |config| {
        config.rpc_servers = vec!["127.0.0.1:50052".to_string()];
        config.tensor_split = Some("30,70".to_string());
        config.rpc_cluster_gate = Some(lucidd::worker_llama::RpcClusterGate::new(move || {
            gate_state.load(std::sync::atomic::Ordering::SeqCst)
        }));
    });
    assert_eq!(fx.worker.execution_trust_label(), "trusted-circle-sharded");

    let refused = match fx.worker.execute(make_manifest("sliced", "Hello.")).await {
        Ok(_) => panic!("RPC offload must be refused before opt-in"),
        Err(error) => error.to_string(),
    };
    assert!(refused.contains("cluster_enabled"), "{refused}");
    assert!(
        spawned_models(fx.dir.path()).is_empty(),
        "no child before opt-in"
    );

    enabled.store(true, std::sync::atomic::Ordering::SeqCst);
    let (completion, error) = run_to_final(&fx.worker, "sliced").await.expect("dispatch");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    let port = fx.ports.start;
    let joined = spawn_argv(fx.dir.path(), port).join(" ");
    assert!(
        joined.contains("--rpc 127.0.0.1:50052 --split-mode layer --tensor-split 30,70"),
        "{joined}"
    );

    enabled.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(fx
        .worker
        .execute(make_manifest("sliced", "Hello."))
        .await
        .is_err());
    assert!(
        wait_port_bindable(port, Duration::from_secs(5)).await,
        "RPC-backed child must be torn down when cluster_enabled turns off"
    );
}

#[tokio::test]
async fn rpc_cold_load_is_refused_if_cluster_gate_closes_mid_load() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // A3: RPC cold loads can take minutes; withdrawing the opt-in while one is
    // in flight must stop it from being admitted.
    let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let gate_state = enabled.clone();
    let fx = flexible_worker(
        &["sliced"],
        1,
        1,
        &[("FAKE_LLAMA_WARMUP_MS", "1000")],
        Duration::from_secs(10),
        |config| {
            config.rpc_servers = vec!["127.0.0.1:50052".to_string()];
            config.rpc_cluster_gate = Some(lucidd::worker_llama::RpcClusterGate::new(move || {
                gate_state.load(std::sync::atomic::Ordering::SeqCst)
            }));
        },
    );
    let worker = fx.worker.clone();
    let load = tokio::spawn(async move {
        worker
            .execute(make_manifest("sliced", "Hello."))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    enabled.store(false, std::sync::atomic::Ordering::SeqCst);
    let error = load
        .await
        .expect("load task")
        .expect_err("admission must be refused once cluster_enabled turns off");
    assert!(error.contains("cluster_enabled"), "{error}");
    assert!(
        wait_port_bindable(fx.ports.start, Duration::from_secs(5)).await,
        "refused RPC child must be killed and its port released"
    );
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
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
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
        ..Default::default()
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

/// A1 against the real binary: with only `LLAMA_SERVER_PATH` and a small
/// embedding GGUF in `LLAMA_TEST_EMBED_MODEL_PATH`, prove the pinned
/// llama-server refuses unauthenticated loopback callers, keeps the web UI
/// and `/slots` off, and rejects browser origins — while the worker's own
/// authenticated job succeeds.
#[tokio::test]
#[ignore]
async fn real_llama_server_rejects_unauthenticated_loopback_requests() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    let bin = std::env::var("LLAMA_SERVER_PATH").expect(
        "real llama auth gate unavailable: set LLAMA_SERVER_PATH and LLAMA_TEST_EMBED_MODEL_PATH",
    );
    let model = std::env::var("LLAMA_TEST_EMBED_MODEL_PATH").expect(
        "real llama auth gate unavailable: set LLAMA_SERVER_PATH and LLAMA_TEST_EMBED_MODEL_PATH",
    );
    let model_path = PathBuf::from(&model);
    let model_dir = model_path.parent().expect("model dir").to_path_buf();
    let model_id = model_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("model stem")
        .to_string();
    let port = free_port();
    let worker = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_binary_path: PathBuf::from(bin),
            model_dir,
            default_n_gpu_layers: 0,
            default_context_size: 512,
            server_port_range: port..(port + 1),
            max_loaded_models: 1,
            model_load_timeout: Duration::from_secs(120),
            per_request_idle_timeout: Duration::from_secs(60),
            ..Default::default()
        },
    );
    let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
        model_cid: model_id,
        input: vec!["authenticated".to_string()],
    }))
    .sign_with(&NodeIdentity::generate())
    .expect("sign");
    let (_handle, mut stream) = worker.execute(manifest).await.expect("dispatch real");
    let mut vectors = 0;
    while let Some(event) = stream.next().await {
        match event {
            JobEvent::Output(_) => vectors += 1,
            JobEvent::Final { result, error } => {
                assert_eq!(result.completion, Completion::Stop, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(vectors, 1, "worker's authenticated embedding must succeed");

    let client = loopback_client();
    let base = format!("http://127.0.0.1:{port}");
    for (path, body) in [
        ("/embedding", serde_json::json!({"content": "exfiltrate"})),
        ("/completion", serde_json::json!({"prompt": "exfiltrate"})),
    ] {
        let response = client
            .post(format!("{base}{path}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{path}"
        );
        let wrong = client
            .post(format!("{base}{path}"))
            .bearer_auth("0".repeat(64))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED, "{path}");
    }
    let web_ui = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(
        web_ui.status(),
        reqwest::StatusCode::NOT_FOUND,
        "--no-webui"
    );
    let slots = client.get(format!("{base}/slots")).send().await.unwrap();
    assert_eq!(slots.status(), reqwest::StatusCode::UNAUTHORIZED);
    let browser = client
        .get(format!("{base}/health"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(
        browser.status(),
        reqwest::StatusCode::FORBIDDEN,
        "browser origins must be refused even for public endpoints"
    );
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert!(health.status().is_success());
}
