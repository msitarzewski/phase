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
use phase_identity::{default_identity_path, NodeIdentity};
use phase_net::{Discovery, DiscoveryConfig};
use phase_protocol::DynWorker;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WorkerChoice {
    /// In-tree EchoWorker. No GPU required; reverses your message.
    Echo,
    /// LlamaCppWorker, shells out to `llama-server`.
    LlamaCpp,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum NodeMode {
    /// Run a local worker. Default. Same node can also serve peers via the
    /// inbound job-relay handler if --no-local-worker is not set.
    Worker,
    /// Consume-only / relay role: no local worker is loaded, every chat
    /// request is routed to a peer (or refused). Sets the same internal
    /// flag as --no-local-worker. Foundation relay-server protocol
    /// (libp2p `relay::server::Behaviour`, DCUtR, etc.) lands in v0.2.
    Relay,
}

#[derive(Debug, Parser)]
#[command(
    name = "lucidd",
    about = "LUCID inference daemon — Ollama-compatible API backed by the LUCID M5 router."
)]
struct Cli {
    /// Run as a worker (default) or as a consume-only relay node.
    /// `--mode relay` is equivalent to `--no-local-worker`.
    #[arg(long, value_enum, default_value_t = NodeMode::Worker)]
    mode: NodeMode,

    /// Which `Worker` impl to expose on :11434. Ignored when
    /// `--no-local-worker` is set or `--mode relay` is selected.
    #[arg(long, value_enum, default_value_t = WorkerChoice::Echo)]
    worker: WorkerChoice,

    /// Run without any local worker — every request gets routed to a
    /// peer over the Phase DHT or refused. Useful on GPU-less laptops
    /// that still want to be useful clients. Same effect as
    /// `--mode relay`.
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

    /// Path to the persistent libp2p identity file. Default:
    /// `~/.config/phase/identity.key` (platform-aware). If absent, lucidd
    /// generates a fresh Ed25519 keypair on first run and persists it
    /// here, so subsequent restarts keep the same peer ID. Two lucidd
    /// instances on the same host need different paths.
    #[arg(long)]
    identity_path: Option<PathBuf>,

    /// libp2p TCP/QUIC listen port. Default `0` = ephemeral random.
    /// Set this to a known value (e.g. `4001`) when you want others to
    /// dial you across WAN with a stable multiaddr — port forwarding on
    /// the home router becomes possible, DNS-based bootstrap records can
    /// be written, etc.
    #[arg(long, default_value_t = 0)]
    libp2p_port: u16,

    /// Multiaddrs of bootstrap peers to dial on startup. Repeatable.
    /// Format: `/ip4/x.x.x.x/tcp/<port>/p2p/<peer-id>` or
    /// `/dns4/host/tcp/<port>/p2p/<peer-id>`. Without bootstraps, a node
    /// on its own LAN finds peers via mDNS; WAN peers won't find each
    /// other without at least one configured bootstrap.
    #[arg(long = "bootstrap-peer", value_name = "MULTIADDR")]
    bootstrap_peers: Vec<String>,

    /// DNS domains to query for TXT-record bootstrap peers. Each TXT
    /// record at the queried name is interpreted as one multiaddr in the
    /// same format as `--bootstrap-peer`. Repeatable.
    ///
    /// Example: `--bootstrap-dns bootstrap.phasebased.net` queries:
    ///   `dig TXT bootstrap.phasebased.net`
    /// and dials every multiaddr it gets back. The foundation maintains
    /// `bootstrap.phasebased.net` with one TXT per public relay so a
    /// fresh install can join the network with zero out-of-band setup.
    #[arg(long = "bootstrap-dns", value_name = "DOMAIN")]
    bootstrap_dns: Vec<String>,
}

/// Query TXT records at each domain and return the parsed multiaddr
/// strings (one per TXT record). Strings starting with `/` are kept; any
/// other shape is logged and dropped. phase-net's bootstrap-peer parser
/// is the authoritative validator — if a TXT contains garbage with a
/// leading slash, it'll be logged as an invalid multiaddr there.
///
/// Failures are best-effort: a single domain returning NXDOMAIN, SERVFAIL,
/// or timing out logs a warning and the function continues with whatever
/// it did get from other domains.
async fn resolve_dns_bootstrap_peers(domains: &[String]) -> Vec<String> {
    if domains.is_empty() {
        return Vec::new();
    }
    let resolver = match hickory_resolver::TokioAsyncResolver::tokio_from_system_conf() {
        Ok(r) => r,
        Err(e) => {
            // Most failure modes here mean /etc/resolv.conf isn't
            // readable (containers without it, locked-down sandboxes).
            // Fall back to Cloudflare/Google defaults.
            tracing::warn!(
                error = %e,
                "could not load system DNS config; falling back to Cloudflare/Google"
            );
            hickory_resolver::TokioAsyncResolver::tokio(
                hickory_resolver::config::ResolverConfig::cloudflare(),
                hickory_resolver::config::ResolverOpts::default(),
            )
        }
    };
    let mut out = Vec::new();
    for domain in domains {
        match resolver.txt_lookup(domain).await {
            Ok(answers) => {
                let mut domain_count = 0usize;
                for record in answers.iter() {
                    for chunk in record.txt_data() {
                        if let Ok(s) = std::str::from_utf8(chunk) {
                            let s = s.trim().to_string();
                            if s.starts_with('/') {
                                out.push(s);
                                domain_count += 1;
                            } else if !s.is_empty() {
                                tracing::debug!(
                                    domain = %domain,
                                    value = %s,
                                    "skipping non-multiaddr TXT record"
                                );
                            }
                        }
                    }
                }
                tracing::info!(domain = %domain, count = domain_count, "DNS bootstrap resolved");
            }
            Err(e) => {
                tracing::warn!(domain = %domain, error = %e, "DNS bootstrap lookup failed");
            }
        }
    }
    out
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

    // --mode relay collapses into --no-local-worker for the v0.1 binary.
    // The real libp2p relay::server::Behaviour wiring lands in v0.2.
    let no_local_worker = cli.no_local_worker || cli.mode == NodeMode::Relay;

    // Persistent identity: libp2p peer-id + receipt signing key derive
    // from this. Default location: ~/.config/phase/identity.key (or the
    // platform equivalent). Persistent so peer-id is stable across
    // restarts — required for any node that wants to be a bootstrap
    // peer, since other nodes will encode the peer-id in their config.
    let identity_path: PathBuf = match cli.identity_path.clone() {
        Some(p) => p,
        None => default_identity_path()
            .map_err(|e| format!("could not resolve default identity path: {e}"))?,
    };
    let node_identity = NodeIdentity::load_or_create(&identity_path)
        .map_err(|e| format!("identity load_or_create({identity_path:?}): {e}"))?;
    tracing::info!(
        path = %identity_path.display(),
        "identity loaded (phase-net will log the libp2p peer-id on swarm init)"
    );

    // Merge explicit --bootstrap-peer args with DNS-resolved ones from
    // --bootstrap-dns. DNS failures (timeout, NXDOMAIN) are non-fatal
    // because mDNS may still discover peers locally and the operator
    // may have explicit --bootstrap-peer args that work.
    let mut bootstrap_peers = cli.bootstrap_peers.clone();
    let dns_peers = resolve_dns_bootstrap_peers(&cli.bootstrap_dns).await;
    if !dns_peers.is_empty() {
        tracing::info!(
            total = dns_peers.len(),
            domains = cli.bootstrap_dns.len(),
            "merged DNS-resolved bootstrap peers"
        );
        bootstrap_peers.extend(dns_peers);
    }

    // Build the phase-net discovery layer. mDNS may be denied in
    // restricted CI envs — that's expected; the daemon still serves
    // local requests in that case.
    let disc_config = DiscoveryConfig {
        identity: Some(node_identity.clone()),
        bootstrap_peers,
        ..DiscoveryConfig::default()
    };
    let discovery = Arc::new(Discovery::new(disc_config)?);

    // Start the libp2p listeners — both IPv4 and IPv6 wildcard binds on
    // the configured port. IPv6 matters for residential nodes on dual-
    // stack ISPs (e.g. Sonic) because the public IPv6 is typically
    // routable without any router port-forwarding — the firewall just
    // needs to allow inbound for /tcp/<port>. Port `0` = ephemeral
    // random (the historical default; fine on LAN where mDNS handles
    // discovery). Port `>0` = stable, suitable for WAN bootstrap-peer
    // multiaddrs and for routers that need a known forward port.
    let listen_v4 = format!("/ip4/0.0.0.0/tcp/{}", cli.libp2p_port);
    let listen_v6 = format!("/ip6/::/tcp/{}", cli.libp2p_port);
    if let Err(e) = discovery.listen(&listen_v4).await {
        tracing::warn!(error = %e, addr = %listen_v4, "discovery IPv4 listen failed (continuing)");
    }
    if let Err(e) = discovery.listen(&listen_v6).await {
        tracing::warn!(error = %e, addr = %listen_v6, "discovery IPv6 listen failed (continuing)");
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
    let local_worker: Option<Arc<dyn DynWorker>> = if no_local_worker {
        tracing::info!(
            mode = ?cli.mode,
            "consume-only / relay node (no local worker loaded)"
        );
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
