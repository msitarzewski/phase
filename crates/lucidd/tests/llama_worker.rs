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
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use lucidd::worker_llama::ResidentModelKind;
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
// Wave 3: honest resident-model reporting
// -----------------------------------------------------------------------

/// `(model_id, kind, trusted_circle_sharded)` of every resident model.
fn resident_summary(worker: &LlamaCppWorker) -> Vec<(String, ResidentModelKind, bool)> {
    worker
        .resident_models()
        .into_iter()
        .map(|model| (model.model_id, model.kind, model.trusted_circle_sharded))
        .collect()
}

/// Poll until `predicate` holds for the worker's resident models.
async fn wait_resident(
    worker: &LlamaCppWorker,
    within: Duration,
    predicate: impl Fn(&[lucidd::worker_llama::ResidentModelInfo]) -> bool,
) -> Vec<lucidd::worker_llama::ResidentModelInfo> {
    let started = std::time::Instant::now();
    loop {
        let resident = worker.resident_models();
        if predicate(&resident) {
            return resident;
        }
        assert!(
            started.elapsed() < within,
            "resident models never reached the expected state: {resident:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run_embedding_to_final(worker: &LlamaCppWorker, model_id: &str) -> Completion {
    let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
        model_cid: model_id.to_string(),
        input: vec!["resident".to_string()],
    }))
    .sign_with(&NodeIdentity::generate())
    .expect("sign embedding manifest");
    let (_handle, mut stream) = worker.execute(manifest).await.expect("dispatch embedding");
    while let Some(event) = stream.next().await {
        if let JobEvent::Final { result, error } = event {
            assert!(error.is_none(), "{error:?}");
            return result.completion;
        }
    }
    panic!("embedding stream ended without Final");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resident_models_list_only_healthy_instances_by_kind() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // The fixture answers `/health` with 503 for its first 1.5 s: a cold load
    // in progress is not resident yet.
    let fx = flexible_worker(
        &["alpha"],
        2,
        3,
        &[("FAKE_LLAMA_WARMUP_MS", "1500")],
        Duration::from_secs(10),
        |_| {},
    );
    let handle = fx.worker.resident_models_handle();
    assert!(handle.snapshot().is_empty(), "nothing is loaded yet");

    let before = SystemTime::now();
    let loading = tokio::spawn({
        let worker = fx.worker.clone();
        async move { run_to_final(&worker, "alpha").await }
    });
    // Wait until the child exists, then sample residency while it warms up.
    let started = std::time::Instant::now();
    while spawned_models(fx.dir.path()).is_empty() {
        assert!(started.elapsed() < Duration::from_secs(5), "no spawn");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for _ in 0..10 {
        assert!(
            fx.worker.resident_models().is_empty(),
            "a loading child must not be reported resident"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (completion, error) = loading.await.unwrap().expect("dispatch alpha");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    assert_eq!(
        run_embedding_to_final(&fx.worker, "alpha").await,
        Completion::Stop
    );
    let after = SystemTime::now();

    // Chat and embedding flavours are separate processes, reported apart.
    let resident = fx.worker.resident_models();
    assert_eq!(
        resident_summary(&fx.worker),
        [
            ("alpha".to_string(), ResidentModelKind::Generation, false),
            ("alpha".to_string(), ResidentModelKind::Embedding, false),
        ]
    );
    for model in &resident {
        assert_eq!(model.backend, lucidd::worker_llama::LLAMA_CPP_BACKEND);
        assert!(before <= model.loaded_since && model.loaded_since <= after);
    }
    assert_eq!(handle.snapshot(), resident);

    // The handle never keeps a dropped worker (or its children) alive.
    let FlexibleWorker {
        dir: _dir, worker, ..
    } = fx;
    drop(worker);
    assert!(handle.snapshot().is_empty());
}

#[tokio::test]
async fn crashed_child_leaves_resident_models_until_its_respawn_is_healthy() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    // Every fixture child exits on its own 1.5 s after it starts.
    let fx = flexible_worker(
        &["flaky"],
        1,
        1,
        &[("FAKE_LLAMA_CRASH_AFTER_MS", "1500")],
        Duration::from_secs(10),
        |_| {},
    );
    let (completion, error) = run_to_final(&fx.worker, "flaky").await.expect("dispatch");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    let first = fx.worker.resident_models();
    assert_eq!(first.len(), 1, "{first:?}");

    // The crash is noticed by supervision alone, with no request in flight.
    wait_resident(&fx.worker, Duration::from_secs(5), <[_]>::is_empty).await;
    // The respawned child is reported again once it passes `/health`, with
    // its own readiness time.
    let respawned = wait_resident(&fx.worker, Duration::from_secs(10), |models| {
        !models.is_empty()
    })
    .await;
    assert_eq!(respawned[0].model_id, "flaky");
    assert!(
        respawned[0].loaded_since > first[0].loaded_since,
        "{respawned:?} vs {first:?}"
    );
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
    assert_eq!(
        resident_summary(&fx.worker),
        [("sliced".to_string(), ResidentModelKind::Generation, true)],
        "an RPC-backed child is reported as trusted-circle sharded"
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

// -----------------------------------------------------------------------
// Wave 2: Tier-1 RPC layer offload to enrolled peers over the
// Noise-encrypted `/phase/shard-rpc/1.0.0` libp2p tunnel.
// -----------------------------------------------------------------------

mod shard_rpc_tunnel {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use lucidd::node_config::{NodeConfig, NodeConfigStore};
    use lucidd::shard_worker::make_opt_in_inbound_shard_rpc_handler;
    use lucidd::worker_llama::rpc_peers::{RpcPeerOffload, RpcPeerTunnelConfig};
    use lucidd::worker_llama::RpcClusterGate;
    use lucidd::PolicyEngine;
    use phase_identity::NodeIdentity;
    use phase_net::{Discovery, DiscoveryConfig, PeerId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    pub(super) fn discovery_for(identity: &NodeIdentity) -> Arc<Discovery> {
        Arc::new(
            Discovery::new(DiscoveryConfig {
                identity: Some(identity.clone()),
                ..DiscoveryConfig::default()
            })
            .expect("discovery"),
        )
    }

    pub(super) async fn connect(client: &Discovery, server: &Discovery) {
        server.listen("/ip4/127.0.0.1/tcp/0").await.expect("listen");
        let mut address = None;
        for _ in 0..100 {
            if let Some(found) = server.listen_addrs().await.unwrap().into_iter().next() {
                address = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let address = address.expect("server listen address");
        client
            .dial_peer(&format!("{address}/p2p/{}", server.local_peer_id()))
            .await
            .expect("dial");
        for _ in 0..250 {
            if client
                .reachability_snapshot()
                .await
                .unwrap()
                .connections
                .iter()
                .any(|connection| connection.peer_id == *server.local_peer_id())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("coordinator never connected to provider");
    }

    /// A provider node that enrolls `coordinator` (explicit allowlist entry)
    /// and bridges admitted tunnels to `127.0.0.1:<bridge_port>`.
    pub(super) struct Provider {
        pub(super) discovery: Arc<Discovery>,
        pub(super) node_config: Arc<NodeConfigStore>,
        _policy_dir: tempfile::TempDir,
    }

    /// Provider policy that explicitly enrolls exactly `coordinator`.
    pub(super) async fn policy_enrolling(
        coordinator: &NodeIdentity,
    ) -> (Arc<PolicyEngine>, tempfile::TempDir) {
        let policy_dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(
            PolicyEngine::load_or_default(Some(policy_dir.path().join("policy.toml")))
                .await
                .unwrap(),
        );
        let coordinator_key = hex::encode(coordinator.peer_id_bytes());
        policy
            .update_config(|config| config.authorized_submitters = vec![coordinator_key])
            .unwrap();
        (policy, policy_dir)
    }

    pub(super) async fn provider_enrolling(
        coordinator: &NodeIdentity,
        bridge_port: u16,
    ) -> Provider {
        let identity = NodeIdentity::generate();
        let discovery = discovery_for(&identity);
        let (policy, policy_dir) = policy_enrolling(coordinator).await;
        let node_config = NodeConfigStore::in_memory(NodeConfig {
            cluster_enabled: true,
            ..NodeConfig::default()
        });
        discovery
            .set_shard_rpc_handler(Some(
                make_opt_in_inbound_shard_rpc_handler(policy, node_config.clone(), bridge_port)
                    .unwrap(),
            ))
            .unwrap();
        Provider {
            discovery,
            node_config,
            _policy_dir: policy_dir,
        }
    }

    /// Loopback TCP forwarder in front of `upstream` that counts bytes in
    /// each direction (stand-in for, or tap in front of, `ggml-rpc-server`).
    pub(super) struct CountingForwarder {
        pub(super) port: u16,
        pub(super) to_upstream: Arc<AtomicU64>,
        pub(super) from_upstream: Arc<AtomicU64>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for CountingForwarder {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn copy_counting(
        mut from: tokio::net::tcp::OwnedReadHalf,
        mut to: tokio::net::tcp::OwnedWriteHalf,
        counter: Arc<AtomicU64>,
    ) {
        let mut buffer = vec![0_u8; 64 * 1024];
        while let Ok(read) = from.read(&mut buffer).await {
            if read == 0 || to.write_all(&buffer[..read]).await.is_err() {
                break;
            }
            counter.fetch_add(read as u64, Ordering::SeqCst);
        }
        let _ = to.shutdown().await;
    }

    /// `upstream = None` echoes instead of forwarding.
    pub(super) async fn counting_forwarder(upstream: Option<u16>) -> CountingForwarder {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let to_upstream = Arc::new(AtomicU64::new(0));
        let from_upstream = Arc::new(AtomicU64::new(0));
        let (up, down) = (to_upstream.clone(), from_upstream.clone());
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (up, down) = (up.clone(), down.clone());
                tokio::spawn(async move {
                    match upstream {
                        Some(upstream) => {
                            if let Ok(target) =
                                TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, upstream)).await
                            {
                                let _ = target.set_nodelay(true);
                                let (client_read, client_write) = socket.into_split();
                                let (target_read, target_write) = target.into_split();
                                tokio::join!(
                                    copy_counting(client_read, target_write, up),
                                    copy_counting(target_read, client_write, down),
                                );
                            }
                        }
                        None => {
                            // Echo; a chunk containing `BYE` makes the backend
                            // hang up on that one connection.
                            let mut socket = socket;
                            let mut buffer = vec![0_u8; 64 * 1024];
                            while let Ok(read) = socket.read(&mut buffer).await {
                                if read == 0 || buffer[..read].windows(3).any(|w| w == b"BYE") {
                                    break;
                                }
                                if socket.write_all(&buffer[..read]).await.is_err() {
                                    break;
                                }
                                up.fetch_add(read as u64, Ordering::SeqCst);
                            }
                        }
                    }
                });
            }
        });
        CountingForwarder {
            port,
            to_upstream,
            from_upstream,
            task,
        }
    }

    pub(super) fn switch_gate(open: bool) -> (RpcClusterGate, Arc<AtomicBool>) {
        let state = Arc::new(AtomicBool::new(open));
        let reader = state.clone();
        (
            RpcClusterGate::new(move || reader.load(Ordering::SeqCst)),
            state,
        )
    }

    pub(super) fn offload_to(coordinator: &Arc<Discovery>, provider: PeerId) -> RpcPeerOffload {
        RpcPeerOffload::new(
            coordinator.clone(),
            vec![provider],
            RpcPeerTunnelConfig::default(),
        )
        .expect("rpc peer offload config")
    }

    /// The `ggml-rpc` HELLO frame the pinned llama.cpp client opens with.
    pub(super) fn hello() -> Vec<u8> {
        let mut frame = vec![0x0e];
        frame.extend_from_slice(&24_u64.to_le_bytes());
        frame.extend_from_slice(&[0_u8; 24]);
        frame
    }

    /// Connect to a `--rpc` endpoint as llama-server's ggml-rpc client does
    /// and prove the bytes reach the provider's backend and return.
    pub(super) async fn rpc_client(endpoint: &str) -> TcpStream {
        let mut stream = TcpStream::connect(endpoint)
            .await
            .expect("--rpc endpoint accepts");
        let hello = hello();
        stream.write_all(&hello).await.unwrap();
        let mut echoed = vec![0_u8; hello.len()];
        tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut echoed))
            .await
            .expect("tunnel round trip")
            .unwrap();
        assert_eq!(echoed, hello);
        stream
    }

    /// `true` once `stream` reports EOF or an error.
    pub(super) async fn closed_within(stream: &mut TcpStream, within: Duration) -> bool {
        let mut byte = [0_u8; 1];
        matches!(
            tokio::time::timeout(within, stream.read(&mut byte)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    }
}

/// The `--rpc` endpoint the fixture launcher recorded for the child serving
/// `model` (searching `ports`), with that child's port.
/// The port and `--rpc` endpoint of the spawn recorded for `model`, if any.
fn try_rpc_endpoint_for(
    dir: &Path,
    ports: std::ops::Range<u16>,
    model: &str,
) -> Option<(u16, String)> {
    for port in ports {
        let Ok(raw) = std::fs::read_to_string(dir.join(format!("fixture.{port}.argv"))) else {
            continue;
        };
        let argv: Vec<&str> = raw.lines().collect();
        if argv
            .iter()
            .any(|arg| arg.ends_with(&format!("{model}.gguf")))
        {
            let index = argv
                .iter()
                .position(|arg| *arg == "--rpc")
                .expect("llama-server receives --rpc");
            return Some((port, argv[index + 1].to_string()));
        }
    }
    None
}

/// Cold-load `model` with enrolled-peer offload the way the real
/// `llama-server` attaches its RPC device: while the fixture is still
/// warming up (`FAKE_LLAMA_WARMUP_MS`), connect to the spawn's `--rpc` proxy
/// and complete a HELLO round trip through the tunnel. Returns the model's
/// port, its proxy endpoint, and the attached link.
async fn load_attaching_rpc_peer(
    fx: &FlexibleWorker,
    model: &str,
    previous_endpoint: Option<&str>,
) -> (u16, String, tokio::net::TcpStream) {
    let loading = tokio::spawn({
        let worker = fx.worker.clone();
        let model = model.to_string();
        async move { run_to_final(&worker, &model).await }
    });
    let started = std::time::Instant::now();
    let (port, endpoint) = loop {
        match try_rpc_endpoint_for(fx.dir.path(), fx.ports.clone(), model) {
            Some((port, endpoint)) if Some(endpoint.as_str()) != previous_endpoint => {
                break (port, endpoint)
            }
            _ => {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "{model} never spawned"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    let link = shard_rpc_tunnel::rpc_client(&endpoint).await;
    let (completion, error) = loading.await.unwrap().expect("dispatch");
    assert_eq!(completion, Completion::Stop, "{model}: {error:?}");
    (port, endpoint, link)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_peer_offload_gives_each_model_its_own_tunnels_and_retires_only_the_loser() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    use shard_rpc_tunnel::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let coordinator_identity = NodeIdentity::generate();
    let coordinator = discovery_for(&coordinator_identity);
    let backend = counting_forwarder(None).await;
    let provider = provider_enrolling(&coordinator_identity, backend.port).await;
    connect(&coordinator, &provider.discovery).await;
    let provider_peer = *provider.discovery.local_peer_id();

    let (gate, _gate_open) = switch_gate(true);
    let fx = flexible_worker(
        &["alpha", "beta"],
        2,
        3,
        &[("FAKE_LLAMA_WARMUP_MS", "1500")],
        Duration::from_secs(10),
        |config| {
            config.rpc_peer_offload = Some(offload_to(&coordinator, provider_peer));
            config.tensor_split = Some("30,70".to_string());
            config.rpc_cluster_gate = Some(gate);
        },
    );
    assert!(fx.worker.uses_rpc_layer_offload());
    assert_eq!(fx.worker.execution_trust_label(), "trusted-circle-sharded");

    // Each model's llama-server attaches a tunnel to the enrolled provider.
    let (alpha_port, alpha_rpc, mut alpha_link) = load_attaching_rpc_peer(&fx, "alpha", None).await;
    let (beta_port, beta_rpc, mut beta_link) = load_attaching_rpc_peer(&fx, "beta", None).await;
    assert_ne!(alpha_rpc, beta_rpc, "every spawn binds its own peer proxy");
    assert_eq!(
        resident_summary(&fx.worker),
        [
            ("alpha".to_string(), ResidentModelKind::Generation, true),
            ("beta".to_string(), ResidentModelKind::Generation, true),
        ]
    );
    for endpoint in [&alpha_rpc, &beta_rpc] {
        let address: std::net::SocketAddr = endpoint.parse().unwrap();
        assert!(address.ip().is_loopback());
    }
    let joined = spawn_argv(fx.dir.path(), alpha_port).join(" ");
    assert!(
        joined.contains(&format!(
            "--rpc {alpha_rpc} --split-mode layer --tensor-split 30,70"
        )),
        "{joined}"
    );

    // alpha's link dies on the provider side (its backend hangs up). Only
    // alpha is retired; beta keeps serving warm on its own healthy tunnel.
    alpha_link.write_all(b"BYE").await.unwrap();
    assert!(closed_within(&mut alpha_link, Duration::from_secs(5)).await);
    drop(alpha_link);
    assert!(
        wait_port_bindable(alpha_port, Duration::from_secs(8)).await,
        "the model whose tunnel was lost must be retired"
    );
    assert!(
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, beta_port)).is_err(),
        "a model with a healthy tunnel must stay resident"
    );
    assert_eq!(
        resident_summary(&fx.worker),
        [("beta".to_string(), ResidentModelKind::Generation, true)],
        "the retired model drops out of the resident list"
    );
    let (completion, error) = run_to_final(&fx.worker, "beta").await.expect("beta warm");
    assert_eq!(completion, Completion::Stop, "{error:?}");
    beta_link.write_all(b"still-up").await.unwrap();
    let mut echoed = [0_u8; 8];
    beta_link.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"still-up");
    assert_eq!(spawned_models(fx.dir.path()), ["alpha", "beta"]);

    // The next alpha request cold-loads over a fresh proxy.
    let (_, reloaded_rpc, _reloaded_link) =
        load_attaching_rpc_peer(&fx, "alpha", Some(&alpha_rpc)).await;
    assert_eq!(spawned_models(fx.dir.path()), ["alpha", "beta", "alpha"]);
    assert_ne!(reloaded_rpc, alpha_rpc);

    // The provider withdrawing cluster_enabled drops beta's live tunnel and
    // retires beta too.
    provider
        .node_config
        .update(|config| {
            config.cluster_enabled = false;
            Ok(())
        })
        .unwrap();
    assert!(closed_within(&mut beta_link, Duration::from_secs(5)).await);
    drop(beta_link);
    assert!(
        wait_port_bindable(beta_port, Duration::from_secs(8)).await,
        "RPC-backed model must be retired after its tunnel is revoked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_peer_offload_refuses_a_load_whose_peer_device_never_attached() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    use shard_rpc_tunnel::*;

    // The pinned llama-server passes /health even when an --rpc device never
    // attached (it runs those layers locally). This fixture never connects
    // to its --rpc endpoint, exactly like that case: the load is refused.
    let coordinator_identity = NodeIdentity::generate();
    let coordinator = discovery_for(&coordinator_identity);
    let backend = counting_forwarder(None).await;
    let provider = provider_enrolling(&coordinator_identity, backend.port).await;
    connect(&coordinator, &provider.discovery).await;
    let (gate, _gate_open) = switch_gate(true);
    let fx = flexible_worker(&["lonely"], 1, 1, &[], Duration::from_secs(10), |config| {
        config.rpc_peer_offload = Some(offload_to(
            &coordinator,
            *provider.discovery.local_peer_id(),
        ));
        config.rpc_cluster_gate = Some(gate);
    });
    let refused = run_to_final(&fx.worker, "lonely")
        .await
        .expect_err("an unattached peer device must refuse the load");
    assert!(refused.contains("did not attach"), "{refused}");
    assert!(
        !refused.contains(&provider.discovery.local_peer_id().to_string()),
        "callers must not learn circle membership: {refused}"
    );
    assert_eq!(spawned_models(fx.dir.path()), ["lonely"]);
    assert!(fx.worker.resident_models().is_empty());
    assert!(
        wait_port_bindable(fx.ports.start, Duration::from_secs(5)).await,
        "the refused child is killed and its port released"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rpc_peer_offload_retires_a_respawn_that_did_not_reattach_its_peer() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    use shard_rpc_tunnel::*;

    let coordinator_identity = NodeIdentity::generate();
    let coordinator = discovery_for(&coordinator_identity);
    let backend = counting_forwarder(None).await;
    let provider = provider_enrolling(&coordinator_identity, backend.port).await;
    connect(&coordinator, &provider.discovery).await;
    let (gate, _gate_open) = switch_gate(true);
    // Every child warms up for 1.5 s and exits on its own at 3 s.
    let fx = flexible_worker(
        &["phoenix"],
        1,
        1,
        &[
            ("FAKE_LLAMA_WARMUP_MS", "1500"),
            ("FAKE_LLAMA_CRASH_AFTER_MS", "3000"),
        ],
        Duration::from_secs(10),
        |config| {
            config.rpc_peer_offload = Some(offload_to(
                &coordinator,
                *provider.discovery.local_peer_id(),
            ));
            config.rpc_cluster_gate = Some(gate);
        },
    );
    let (port, _, link) = load_attaching_rpc_peer(&fx, "phoenix", None).await;
    assert_eq!(fx.worker.resident_models().len(), 1);

    // The child crashes; llama-server's RPC socket dies with it. The
    // respawn never re-attaches (as when the peer is gone), so the pinned
    // llama-server would run every layer locally: it is retired instead.
    wait_resident(&fx.worker, Duration::from_secs(5), <[_]>::is_empty).await;
    drop(link);
    let started = std::time::Instant::now();
    while spawned_models(fx.dir.path()).len() < 2 {
        assert!(started.elapsed() < Duration::from_secs(10), "no respawn");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The respawn passes /health 1.5 s after it starts; it must never be
    // reported resident, and no further respawn follows its retirement.
    let watched = std::time::Instant::now();
    while watched.elapsed() < Duration::from_secs(3) {
        assert!(
            fx.worker.resident_models().is_empty(),
            "an unattached respawn must not serve as trusted-circle work"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        wait_port_bindable(port, Duration::from_secs(2)).await,
        "the unattached respawn is killed and its port released"
    );
    assert_eq!(spawned_models(fx.dir.path()), ["phoenix", "phoenix"]);
}

/// End-to-end Tier-1 layer offload through the shard-rpc tunnel with the
/// real binaries: `llama-server --rpc <coordinator proxy>` -> libp2p (Noise)
/// -> enrolled provider -> loopback `ggml-rpc-server`. Requires
/// `LLAMA_SERVER_PATH`, `LLAMA_RPC_SERVER_PATH` (the pinned build's
/// `ggml-rpc-server`) and a small embedding GGUF in
/// `LLAMA_TEST_EMBED_MODEL_PATH`. llama.cpp puts RPC devices first in a
/// model's device list, so the default `LLAMA_TEST_RPC_TENSOR_SPLIT` of `1,0`
/// places every offloaded layer on the tunnelled RPC device and none on the
/// local GPU; override it if the host has a different device list.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn real_llama_server_offloads_layers_through_shard_rpc_tunnel() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    use shard_rpc_tunnel::*;
    use std::sync::atomic::Ordering;

    let missing = "real shard-rpc gate unavailable: set LLAMA_SERVER_PATH, LLAMA_RPC_SERVER_PATH and LLAMA_TEST_EMBED_MODEL_PATH";
    let server_bin = PathBuf::from(std::env::var("LLAMA_SERVER_PATH").expect(missing));
    let rpc_bin = PathBuf::from(std::env::var("LLAMA_RPC_SERVER_PATH").expect(missing));
    let model_path = PathBuf::from(std::env::var("LLAMA_TEST_EMBED_MODEL_PATH").expect(missing));
    let split = std::env::var("LLAMA_TEST_RPC_TENSOR_SPLIT").unwrap_or_else(|_| "1,0".to_string());
    let model_dir = model_path.parent().expect("model dir").to_path_buf();
    let model_id = model_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("model stem")
        .to_string();

    // Provider: operator-run loopback ggml-rpc-server, tapped by a counting
    // forwarder so the test can prove tensors crossed the tunnel.
    let rpc_port = free_port();
    let mut rpc_server = tokio::process::Command::new(&rpc_bin)
        .env_clear()
        .args(["-H", "127.0.0.1", "-p", &rpc_port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn ggml-rpc-server");
    let mut listening = false;
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(("127.0.0.1", rpc_port))
            .await
            .is_ok()
        {
            listening = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(listening, "ggml-rpc-server never listened");
    // Our probe connection took the server's only client slot briefly; let it
    // notice the disconnect before llama-server connects.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tap = counting_forwarder(Some(rpc_port)).await;

    let coordinator_identity = NodeIdentity::generate();
    let coordinator = discovery_for(&coordinator_identity);
    let provider = provider_enrolling(&coordinator_identity, tap.port).await;
    connect(&coordinator, &provider.discovery).await;
    let (gate, _) = switch_gate(true);
    let offload = offload_to(&coordinator, *provider.discovery.local_peer_id());

    let embed = |worker: LlamaCppWorker, label: &'static str| {
        let model_id = model_id.clone();
        async move {
            let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
                model_cid: model_id,
                input: vec!["trusted circle layer offload".to_string()],
            }))
            .sign_with(&NodeIdentity::generate())
            .expect("sign");
            let (_handle, mut stream) = worker.execute(manifest).await.expect(label);
            let mut vector = None;
            while let Some(event) = stream.next().await {
                match event {
                    JobEvent::Output(chunk) => {
                        vector = Some(serde_json::from_slice::<Vec<f32>>(&chunk.data).unwrap());
                    }
                    JobEvent::Final { result, error } => {
                        assert_eq!(result.completion, Completion::Stop, "{label}: {error:?}");
                        break;
                    }
                    _ => {}
                }
            }
            vector.unwrap_or_else(|| panic!("{label}: no embedding vector"))
        }
    };

    let port = free_port();
    let sharded = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_binary_path: server_bin.clone(),
            model_dir: model_dir.clone(),
            default_n_gpu_layers: i32::MAX,
            default_context_size: 512,
            server_port_range: port..(port + 1),
            max_loaded_models: 1,
            model_load_timeout: Duration::from_secs(180),
            per_request_idle_timeout: Duration::from_secs(120),
            rpc_peer_offload: Some(offload),
            tensor_split: Some(split),
            rpc_cluster_gate: Some(gate),
            ..Default::default()
        },
    );
    assert_eq!(sharded.execution_trust_label(), "trusted-circle-sharded");
    let remote = embed(sharded.clone(), "sharded embedding").await;
    let sent = tap.to_upstream.load(Ordering::SeqCst);
    let received = tap.from_upstream.load(Ordering::SeqCst);
    eprintln!("shard-rpc tunnel carried {sent} bytes to and {received} bytes from ggml-rpc-server");
    assert!(
        sent > 8 * 1024 * 1024,
        "model tensors must cross the tunnel to the RPC device (sent {sent} bytes)"
    );
    assert!(received > 0);

    // The same input computed entirely locally must agree.
    let local_port = free_port();
    let local = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_binary_path: server_bin,
            model_dir,
            default_n_gpu_layers: i32::MAX,
            default_context_size: 512,
            server_port_range: local_port..(local_port + 1),
            max_loaded_models: 1,
            model_load_timeout: Duration::from_secs(180),
            per_request_idle_timeout: Duration::from_secs(120),
            ..Default::default()
        },
    );
    let reference = embed(local, "local embedding").await;
    assert_eq!(remote.len(), reference.len());
    let dot: f64 = remote
        .iter()
        .zip(&reference)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let cosine = dot / (norm(&remote) * norm(&reference));
    eprintln!("cosine(sharded, local) = {cosine:.6}");
    assert!(
        cosine > 0.999,
        "sharded and local embeddings diverge: {cosine}"
    );

    drop(sharded);
    let _ = rpc_server.start_kill();
    let _ = rpc_server.wait().await;
}

/// One real embedding through `worker`, or the dispatch/stream error.
async fn real_embedding(worker: &LlamaCppWorker, model_id: &str) -> Result<Vec<f32>, String> {
    let manifest = ManifestBuilder::new(JobSpec::Embedding(EmbeddingJobSpec {
        model_cid: model_id.to_string(),
        input: vec!["trusted circle layer offload".to_string()],
    }))
    .sign_with(&NodeIdentity::generate())
    .expect("sign");
    let (_handle, mut stream) = worker.execute(manifest).await.map_err(|e| e.to_string())?;
    let mut vector = None;
    while let Some(event) = stream.next().await {
        match event {
            JobEvent::Output(chunk) => {
                vector = Some(serde_json::from_slice::<Vec<f32>>(&chunk.data).unwrap());
            }
            JobEvent::Final { result, error } => {
                if result.completion != Completion::Stop {
                    return Err(format!("{:?}: {error:?}", result.completion));
                }
                break;
            }
            _ => {}
        }
    }
    vector.ok_or_else(|| "no embedding vector".to_string())
}

/// Zero-CLI provider end to end with the real pinned binaries: the provider
/// runs no operator `ggml-rpc-server`. lucidd locates the pinned build beside
/// `LLAMA_SERVER_PATH` (which must be a pinned `b12651` runtime directory),
/// starts it only once `cluster_enabled` is on, and the coordinator's layers
/// execute on it through the tunnel; withdrawing the opt-in kills it and
/// retires the coordinator's RPC-backed model. Requires `LLAMA_SERVER_PATH`
/// and `LLAMA_TEST_EMBED_MODEL_PATH`; see the test above for
/// `LLAMA_TEST_RPC_TENSOR_SPLIT`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn real_auto_managed_ggml_rpc_server_serves_layer_offload_only_while_cluster_enabled() {
    #[cfg(target_os = "macos")]
    let _fixture_guard = fixture_process_guard().await;
    use lucidd::node_config::{NodeConfig, NodeConfigStore};
    use lucidd::shard_worker::{
        locate_pinned_ggml_rpc_server, ManagedRpcServerConfig, ManagedShardRpcProvider,
    };
    use shard_rpc_tunnel::*;

    let missing = "real auto-managed shard-rpc gate unavailable: set LLAMA_SERVER_PATH and LLAMA_TEST_EMBED_MODEL_PATH";
    let server_bin = PathBuf::from(std::env::var("LLAMA_SERVER_PATH").expect(missing));
    let model_path = PathBuf::from(std::env::var("LLAMA_TEST_EMBED_MODEL_PATH").expect(missing));
    let split = std::env::var("LLAMA_TEST_RPC_TENSOR_SPLIT").unwrap_or_else(|_| "1,0".to_string());
    let model_dir = model_path.parent().expect("model dir").to_path_buf();
    let model_id = model_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("model stem")
        .to_string();
    #[cfg(unix)]
    let owner = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&server_bin).unwrap().uid()
    };
    #[cfg(not(unix))]
    let owner = 0;
    let binary = locate_pinned_ggml_rpc_server(&server_bin, owner)
        .expect("LLAMA_SERVER_PATH must sit beside the pinned b12651 ggml-rpc-server");

    let coordinator_identity = NodeIdentity::generate();
    let coordinator = discovery_for(&coordinator_identity);
    let provider = discovery_for(&NodeIdentity::generate());
    let (policy, _policy_dir) = policy_enrolling(&coordinator_identity).await;
    let provider_config = NodeConfigStore::in_memory(NodeConfig::default());
    let cache = tempfile::tempdir().unwrap();
    let managed = ManagedShardRpcProvider::start(
        provider.clone(),
        policy,
        provider_config.clone(),
        ManagedRpcServerConfig::new(binary, cache.path().join("ggml-rpc-cache")),
    );
    connect(&coordinator, &provider).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(managed.serving_port(), None, "default off: no server runs");

    let (gate, _) = switch_gate(true);
    let port = free_port();
    let sharded = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_binary_path: server_bin.clone(),
            model_dir: model_dir.clone(),
            default_n_gpu_layers: i32::MAX,
            default_context_size: 512,
            server_port_range: port..(port + 1),
            max_loaded_models: 1,
            model_load_timeout: Duration::from_secs(120),
            per_request_idle_timeout: Duration::from_secs(120),
            rpc_peer_offload: Some(offload_to(&coordinator, *provider.local_peer_id())),
            tensor_split: Some(split),
            rpc_cluster_gate: Some(gate),
            ..Default::default()
        },
    );
    // The provider has not opted in, so its tunnels are refused and the
    // RPC-backed load fails closed.
    let refused = tokio::time::timeout(
        Duration::from_secs(150),
        real_embedding(&sharded, &model_id),
    )
    .await
    .expect("refused load ends");
    assert!(
        refused.is_err(),
        "no layer offload before the provider opts in"
    );
    assert!(sharded.resident_models().is_empty());

    // Opting in starts the pinned server on demand and the same load works.
    provider_config
        .update(|config| {
            config.cluster_enabled = true;
            Ok(())
        })
        .unwrap();
    let started = std::time::Instant::now();
    let rpc_port = loop {
        if let Some(port) = managed.serving_port() {
            break port;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "managed ggml-rpc-server never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let remote = real_embedding(&sharded, &model_id)
        .await
        .expect("sharded embedding over the auto-managed server");
    assert_eq!(
        resident_summary(&sharded),
        [(model_id.clone(), ResidentModelKind::Embedding, true)]
    );

    let local_port = free_port();
    let local = LlamaCppWorker::new(
        NodeIdentity::generate(),
        LlamaCppConfig {
            server_binary_path: server_bin,
            model_dir,
            default_n_gpu_layers: i32::MAX,
            default_context_size: 512,
            server_port_range: local_port..(local_port + 1),
            max_loaded_models: 1,
            model_load_timeout: Duration::from_secs(120),
            per_request_idle_timeout: Duration::from_secs(120),
            ..Default::default()
        },
    );
    let reference = real_embedding(&local, &model_id)
        .await
        .expect("local embedding");
    drop(local);
    assert_eq!(remote.len(), reference.len());
    let dot: f64 = remote
        .iter()
        .zip(&reference)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let cosine = dot / (norm(&remote) * norm(&reference));
    eprintln!("auto-managed ggml-rpc-server on 127.0.0.1:{rpc_port}; cosine(sharded, local) = {cosine:.6}");
    assert!(
        cosine > 0.999,
        "sharded and local embeddings diverge: {cosine}"
    );

    // Withdrawing the opt-in kills and reaps the server and retires the
    // coordinator's model whose layers lived there.
    provider_config
        .update(|config| {
            config.cluster_enabled = false;
            Ok(())
        })
        .unwrap();
    let started = std::time::Instant::now();
    while managed.serving_port().is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "server outlived the opt-in"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(wait_port_bindable(rpc_port, Duration::from_secs(2)).await);
    wait_resident(&sharded, Duration::from_secs(15), <[_]>::is_empty).await;
    managed.shutdown().await;
}
