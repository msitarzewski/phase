// SPDX-License-Identifier: AGPL-3.0-or-later

//! `lucidd` daemon binary — boots the Ollama-compatible HTTP surface on
//! :11434 (or `LUCIDD_PORT`) backed by the LUCID M5 router.
//!
//! Wiring:
//!
//! 1. Persistent `NodeIdentity` (libp2p PeerId + receipt signing).
//! 2. `phase_net::Discovery` for the DHT and the `/phase/job-relay/1.0.0`
//!    request/response protocol.
//! 3. `PhaseNetDhtTransport` + `ModelRegistry` for the model index.
//! 4. `PolicyEngine` for operator-controlled gating.
//! 5. Optional local `Worker` (Echo or LlamaCpp). With `--no-local-worker`
//!    the daemon is consume-only: every request goes to a peer or refuses.
//! 6. `Router` glues 1–5 and exposes a per-request decision API the
//!    Ollama HTTP layer wraps.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use lucidd::echo::EchoWorker;
use lucidd::ollama::{router as ollama_router, AppState};
use lucidd::registry::DhtTransport;
use lucidd::router::{make_inbound_relay_handler, Router as LucidRouter};
use lucidd::{LlamaCppConfig, LlamaCppWorker, ModelRegistry, PhaseNetDhtTransport, PolicyEngine};
use phase_identity::NodeIdentity;
use phase_net::{Discovery, DiscoveryConfig};
use phase_protocol::DynWorker;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WorkerChoice {
    /// In-tree EchoWorker. No GPU required; reverses your message.
    Echo,
    /// LlamaCppWorker, shells out to `llama-server`.
    LlamaCpp,
}

#[derive(Debug, Parser)]
#[command(
    name = "lucidd",
    about = "LUCID inference daemon — Ollama-compatible API backed by the LUCID M5 router."
)]
struct Cli {
    /// Which `Worker` impl to expose on :11434. Ignored when
    /// `--no-local-worker` is set.
    #[arg(long, value_enum, default_value_t = WorkerChoice::Echo)]
    worker: WorkerChoice,

    /// Run without any local worker — every request gets routed to a
    /// peer over the Phase DHT or refused. Useful on GPU-less laptops
    /// that still want to be useful clients.
    #[arg(long, default_value_t = false)]
    no_local_worker: bool,

    /// Directory containing `.gguf` model files. Required when
    /// `--worker llama-cpp`.
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// Path to the `llama-server` binary. Default: rely on `$PATH`.
    #[arg(long, default_value = "llama-server")]
    llama_server_binary: PathBuf,

    /// `--n-gpu-layers` passed to llama-server. Use `-1` for "all" — the
    /// worker translates that to llama-server's `all` literal.
    #[arg(long, default_value_t = -1)]
    llama_n_gpu_layers: i32,

    /// Default `--ctx-size` for llama-server. Per-request `max_tokens`
    /// still applies on top of this.
    #[arg(long, default_value_t = 8192)]
    llama_ctx_size: usize,

    /// Override the policy config path. Default:
    /// `~/.config/lucidd/policy.toml` (with the platform's XDG / AppSupport
    /// resolution). `lucidd` seeds a fully-commented default if absent.
    #[arg(long)]
    policy_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Default to `info` so the per-request lines are visible without needing
    // to set RUST_LOG; respect the env if it's set.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,lucidd=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    let port: u16 = std::env::var("LUCIDD_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(11434);

    // Bind to localhost by default — the research brief flagged unauth
    // exposure on :11434 as a real gotcha. LUCID M7 will gate any
    // non-loopback bind behind explicit policy.
    let host: String = std::env::var("LUCIDD_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    // Persistent identity: libp2p peer-id + receipt signing key derive
    // from this. Default location matches plasm's convention.
    let node_identity = NodeIdentity::generate();

    // Build the phase-net discovery layer. mDNS may be denied in
    // restricted CI envs — that's expected; the daemon still serves
    // local requests in that case.
    let disc_config = DiscoveryConfig {
        identity: Some(node_identity.clone()),
        ..DiscoveryConfig::default()
    };
    let discovery = Arc::new(Discovery::new(disc_config)?);

    // Start the libp2p listener and bootstrap the DHT. Both calls are
    // tolerant of network restrictions.
    if let Err(e) = discovery.listen("/ip4/0.0.0.0/tcp/0").await {
        tracing::warn!(error = %e, "discovery listen failed (continuing)");
    }
    if let Err(e) = discovery.bootstrap().await {
        tracing::warn!(error = %e, "discovery bootstrap failed (continuing)");
    }

    // Model registry, backed by phase-net's Kademlia DHT.
    let transport: Arc<dyn DhtTransport> =
        Arc::new(PhaseNetDhtTransport::new(discovery.clone()));
    let registry = Arc::new(ModelRegistry::new(node_identity.clone(), transport));

    // Operator policy. The engine seeds `~/.config/lucidd/policy.toml`
    // on first run with a fully-commented default.
    let policy = Arc::new(PolicyEngine::load_or_default(cli.policy_config.clone()).await?);

    // Optional local worker.
    let local_worker: Option<Arc<dyn DynWorker>> = if cli.no_local_worker {
        tracing::info!("--no-local-worker: this daemon is consume-only");
        None
    } else {
        match cli.worker {
            WorkerChoice::Echo => {
                tracing::info!("worker: echo (no GPU, reverses input)");
                // EchoWorker handles every model_id (it doesn't care
                // about the weights). Advertise a synthetic "echo"
                // entry in the registry so the router's "local has
                // model" check resolves on common Ollama CLI calls.
                let caps = lucidd::ModelCapabilities::now(
                    "echo",
                    lucidd::ModelCid([0u8; 32]),
                    "none",
                    8192,
                    16,
                    "echo",
                );
                if let Err(e) = registry.advertise_loaded(caps).await {
                    tracing::warn!(error = %e, "failed to advertise synthetic echo entry");
                }
                Some(Arc::new(EchoWorker::new()) as Arc<dyn DynWorker>)
            }
            WorkerChoice::LlamaCpp => {
                let model_dir = cli
                    .model_dir
                    .clone()
                    .ok_or("--model-dir is required with --worker llama-cpp")?;
                let n_gpu_layers = if cli.llama_n_gpu_layers < 0 {
                    i32::MAX
                } else {
                    cli.llama_n_gpu_layers
                };

                // Auto-detect GGUFs and advertise them so the router's local
                // check resolves on first request (otherwise the registry is
                // empty until a model is loaded, causing Refused before
                // ensure_loaded() even runs).
                if let Ok(entries) = std::fs::read_dir(&model_dir) {
                    let mut advertised = 0usize;
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) != Some("gguf") {
                            continue;
                        }
                        let Some(model_id) = path.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        // Deterministic placeholder CID derived from the
                        // name via SHA-256 with domain separation. Two
                        // peers see the same CID for the same model_id, so
                        // a consume-only peer can DHT-look-up by name
                        // without ever loading the weights itself. Real
                        // content-hashed CIDs land in v0.2.
                        let cid = lucidd::ModelCid::from_model_id(model_id);

                        let caps = lucidd::ModelCapabilities::now(
                            model_id,
                            cid,
                            "unknown",
                            cli.llama_ctx_size as u32,
                            1,
                            "llama.cpp",
                        );
                        if let Err(e) = registry.advertise_loaded(caps).await {
                            tracing::warn!(model = %model_id, error = %e, "failed to advertise");
                        } else {
                            advertised += 1;
                            tracing::info!(model = %model_id, "advertised local model");
                        }
                    }
                    tracing::info!(count = advertised, dir = ?model_dir, "advertised local models");
                }

                let config = LlamaCppConfig {
                    server_binary_path: cli.llama_server_binary.clone(),
                    model_dir,
                    default_n_gpu_layers: n_gpu_layers,
                    default_context_size: cli.llama_ctx_size,
                    ..Default::default()
                };
                tracing::info!(?config, "worker: llama-cpp");
                let worker_identity = NodeIdentity::generate();
                Some(Arc::new(LlamaCppWorker::new(worker_identity, config)) as Arc<dyn DynWorker>)
            }
        }
    };

    // Register the inbound peer-relay handler so other peers can ask us
    // to serve work. Only installed when we have a local worker —
    // consume-only nodes can't help anyone.
    if let Some(worker) = local_worker.clone() {
        let handler = make_inbound_relay_handler(worker, registry.clone(), policy.clone());
        if let Err(e) = discovery.set_job_relay_handler(Some(handler)).await {
            tracing::warn!(error = %e, "set_job_relay_handler failed");
        }
    }

    // The router itself.
    let router = Arc::new(LucidRouter::new(
        local_worker.clone(),
        registry.clone(),
        policy.clone(),
        node_identity.clone(),
        discovery.clone(),
    ));

    let client_identity = NodeIdentity::generate();
    let state = AppState {
        router,
        client_identity,
    };
    let app = ollama_router(state);

    tracing::info!(%addr, "lucidd listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
