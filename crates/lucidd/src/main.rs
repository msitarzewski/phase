// SPDX-License-Identifier: AGPL-3.0-or-later

//! `lucidd` daemon binary — boots the Ollama-compatible HTTP surface on
//! :11434 (or `LUCIDD_PORT`) backed by the LUCID M5 router.
//!
//! Wiring:
//!
//! 1. Persistent `NodeIdentity` (libp2p PeerId + receipt signing).
//! 2. `phase_net::Discovery` for the DHT and the bounded batch/live job-relay
//!    protocols.
//! 3. `PhaseNetDhtTransport` + `ModelRegistry` for the model index.
//! 4. `PolicyEngine` for operator-controlled gating.
//! 5. Optional local `Worker` (development Echo, llama.cpp, or Apple-Silicon
//!    MLX). With `--no-local-worker` the daemon is consume-only: every request
//!    goes to a peer or refuses.
//! 6. `Router` glues 1–5 and exposes a per-request decision API the
//!    Ollama HTTP layer wraps.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use lucidd::content::{ContentPlane, ContentPlaneConfig, LocalGgufActivation};
use lucidd::echo::EchoWorker;
use lucidd::ollama::{router as ollama_router, AppState};
use lucidd::registry::DhtTransport;
use lucidd::reputation::{
    EvidenceRuntime, OperatorOverride, DEFAULT_EVIDENCE_RETENTION, MAX_OPERATOR_OVERRIDES,
};
use lucidd::router::{
    make_inbound_relay_handlers, RedundantVerificationConfig, Router as LucidRouter,
};
use lucidd::{
    LlamaCppConfig, LlamaCppWorker, MlxConfig, MlxWorker, ModelRegistry, PhaseNetDhtTransport,
    PolicyEngine,
};
use phase_artifact_server::ArtifactStore;
use phase_identity::{default_identity_path, NodeIdentity};
use phase_net::{
    Discovery, DiscoveryConfig, Multiaddr, PeerId, ReachabilityConfig, ReachabilityRole,
    RelayServerLimits,
};
use phase_protocol::DynWorker;

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum WorkerChoice {
    /// In-tree EchoWorker. No GPU required; reverses your message.
    Echo,
    /// LlamaCppWorker, shells out to `llama-server`.
    LlamaCpp,
    /// Apple-Silicon MLX worker, shells out to a pinned `mlx_lm.server` entry point.
    Mlx,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum NodeMode {
    /// Run a local worker. Default. Same node can also serve peers via the
    /// inbound job-relay handler if --no-local-worker is not set.
    Worker,
    /// Consume-only peer: no local worker is loaded; requests route to a
    /// serving peer or are refused. `relay` remains a CLI compatibility alias.
    #[value(name = "consume-only", alias = "relay")]
    ConsumeOnly,
    /// Public infrastructure node: no local inference worker; enables bounded
    /// circuit-relay and AutoNAT-server roles. Rendezvous serving is disabled
    /// until its registration store has hard quotas.
    Infrastructure,
}

#[derive(Debug, Parser)]
#[command(
    name = "lucidd",
    about = "LUCID inference daemon — verified local/peer inference behind an Ollama-compatible API."
)]
struct Cli {
    /// Run as a worker (default) or as a consume-only relay node.
    /// `--mode relay` is equivalent to `--no-local-worker`.
    #[arg(long, value_enum, default_value_t = NodeMode::Worker)]
    mode: NodeMode,

    /// Which `Worker` impl to expose on :11434. Required for worker mode;
    /// `echo` is an explicit development/test opt-in, never a production
    /// default. Ignored in consume-only/relay mode.
    #[arg(long, value_enum)]
    worker: Option<WorkerChoice>,

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

    /// Content-addressed store for verified model weights. Defaults to
    /// `<model-dir>/.phase-artifacts` in llama-cpp mode and the platform's
    /// local-data directory in consume-only/content-provider mode.
    #[arg(long)]
    artifact_dir: Option<PathBuf>,

    /// Serve locally verified model blobs to authenticated Phase peers over
    /// the bounded content stream. Off by default: installing content does
    /// not implicitly opt an operator into contributing upload bandwidth.
    #[arg(long, default_value_t = false)]
    serve_model_content: bool,

    /// Absolute path to the trusted `llama-server` binary. Required with
    /// `--worker llama-cpp`; PATH lookup is intentionally not performed.
    #[arg(long)]
    llama_server_binary: Option<PathBuf>,

    /// `--n-gpu-layers` passed to llama-server. Use `-1` for "all" — the
    /// worker translates that to llama-server's `all` literal.
    #[arg(long, default_value_t = -1)]
    llama_n_gpu_layers: i32,

    /// Default `--ctx-size` for llama-server. Per-request `max_tokens`
    /// still applies on top of this.
    #[arg(long, default_value_t = 8192)]
    llama_ctx_size: usize,

    /// Absolute path to the pinned `mlx_lm.server` entry point. Required with
    /// `--worker mlx`; PATH lookup is intentionally not performed.
    #[arg(long)]
    mlx_server_binary: Option<PathBuf>,

    /// Read-only local MLX model bundle. Required with `--worker mlx`; its
    /// bounded canonical bundle root becomes the advertised model CID.
    #[arg(long)]
    mlx_model_bundle: Option<PathBuf>,

    /// Canonical model alias to publish for the verified MLX bundle. Required
    /// with `--worker mlx` so a filesystem name never becomes network metadata.
    #[arg(long)]
    mlx_model_alias: Option<String>,

    /// Fixed loopback port for `mlx_lm.server`; zero asks Phase to reserve an
    /// ephemeral port immediately before spawn. The backend cannot inherit the
    /// listener, so the worker reports the residual close-to-bind race.
    #[arg(long, default_value_t = 0)]
    mlx_server_port: u16,

    /// Override the policy config path. Default:
    /// `~/.config/lucidd/policy.toml` (with the platform's XDG / AppSupport
    /// resolution). `lucidd` seeds a fully-commented default if absent.
    #[arg(long)]
    policy_config: Option<PathBuf>,

    /// Append-only local execution-evidence store. By default this is placed
    /// beside the persistent identity as `reputation-evidence-v1.log`, which
    /// keeps evidence scoped to the observer PeerId that produced it.
    #[arg(long)]
    evidence_store: Option<PathBuf>,

    /// Locally block a serving PeerId from reputation-based routing. Repeatable
    /// and process-configured; no network record can override it.
    #[arg(long = "block-peer", value_name = "PEER_ID")]
    blocked_peers: Vec<String>,

    /// Locally pin a serving PeerId in reputation assessment. Repeatable;
    /// an explicit block always wins if both are configured.
    #[arg(long = "pin-peer", value_name = "PEER_ID")]
    pinned_peers: Vec<String>,

    /// Durable signed-alias rollback/equivocation checkpoint. Defaults to a
    /// private sidecar of the persistent identity path.
    #[arg(long)]
    alias_replay_state: Option<PathBuf>,

    /// Deterministic remote jobs to spot-check redundantly, in permille.
    /// Zero disables redundant execution. Values above 1000 are rejected;
    /// the router additionally permits only one two-peer check at a time.
    #[arg(long, default_value_t = 0)]
    redundant_verification_permille: u16,

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

    /// Public Phase relay/rendezvous multiaddrs. Repeatable. Ordinary nodes
    /// reserve a circuit through each bounded candidate and use the pinned
    /// PeerId for rendezvous. By default bootstrap peers are also tried as
    /// relay candidates; use `--no-bootstrap-relays` to opt out.
    #[arg(long = "relay-peer", value_name = "MULTIADDR")]
    relay_peers: Vec<String>,

    /// Do not treat configured/DNS bootstrap peers as relay candidates.
    /// Explicit `--relay-peer` values remain active.
    #[arg(long, default_value_t = false)]
    no_bootstrap_relays: bool,

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

    /// Disable the foundation bootstrap domain for offline/LAN-only use.
    /// Explicit `--bootstrap-dns` and `--bootstrap-peer` values still apply.
    #[arg(long = "no-default-bootstrap", default_value_t = false)]
    no_default_bootstrap: bool,

    /// Opt in to falling back to public DNS resolvers (Cloudflare 1.1.1.1
    /// / Google 8.8.8.8) when the system resolver config can't be loaded.
    /// SEC-09: this widens the set of resolvers you trust for bootstrap
    /// records, so it is OFF by default — without it, a node that can't
    /// read its resolver config fails the DNS-bootstrap step closed and
    /// logs loudly rather than silently trusting a public resolver.
    #[arg(long = "dns-fallback", default_value_t = false)]
    dns_fallback: bool,

    /// Operator-confirmed externally reachable libp2p multiaddr. Repeatable.
    /// Infrastructure nodes need at least one public address before relay
    /// reservations can be granted safely.
    #[arg(long = "external-address", value_name = "MULTIADDR")]
    external_addresses: Vec<String>,

    /// Disable circuit-relay service while retaining other infrastructure
    /// roles. Valid only with `--mode infrastructure`.
    #[arg(long, default_value_t = false)]
    no_relay_server: bool,

    /// Disable AutoNAT service while retaining relay/rendezvous roles. Valid
    /// only with `--mode infrastructure`.
    #[arg(long, default_value_t = false)]
    no_autonat_server: bool,
}

/// SEC-09: per-domain cap on accepted bootstrap multiaddrs. A spoofed or
/// MITM'd TXT record set returning thousands of multiaddrs would otherwise
/// be dialed unbounded (connection-flood / fd-exhaustion). 64 is well
/// above the realistic relay count for the foundation record.
const MAX_BOOTSTRAP_ADDRS_PER_DOMAIN: usize = 64;
const DEFAULT_BOOTSTRAP_DOMAIN: &str = "bootstrap.phasebased.net";
const MAX_RELAY_CLIENT_CANDIDATES: usize = 8;
const LUCID_RENDEZVOUS_NAMESPACE: &str = "phase-lucid-workers";
const RENDEZVOUS_TTL_SECONDS: u64 = 2 * 60 * 60;
const RENDEZVOUS_DISCOVER_LIMIT: u64 = 64;
const RENDEZVOUS_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const REACHABILITY_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const REACHABILITY_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone)]
struct PreparedMlxStartup {
    config: MlxConfig,
    alias: String,
    metadata: lucidd::MlxBundleMetadata,
}

/// Resolve and fully validate the local-only MLX startup contract before any
/// persistent identity, DNS, libp2p listener, or HTTP listener is opened.
/// Construction is repeated with the persistent identity later so a mutation
/// between preflight and advertisement is still rejected.
fn prepare_mlx_startup(cli: &Cli, platform_supported: bool) -> Result<PreparedMlxStartup, String> {
    if !platform_supported {
        return Err("MLX backend requires macOS on Apple Silicon".into());
    }
    let runtime = cli.mlx_server_binary.as_ref().cloned().ok_or_else(|| {
        "--mlx-server-binary is required with --worker mlx and must be an absolute trusted path"
            .to_string()
    })?;
    if !runtime.is_absolute() {
        return Err("--mlx-server-binary must be an absolute trusted path".into());
    }
    let bundle = cli
        .mlx_model_bundle
        .as_ref()
        .cloned()
        .ok_or_else(|| "--mlx-model-bundle is required with --worker mlx".to_string())?;
    let alias = cli
        .mlx_model_alias
        .as_deref()
        .ok_or_else(|| "--mlx-model-alias is required with --worker mlx".to_string())?;
    let normalized_alias =
        lucidd::normalize_model_alias(alias).map_err(|error| error.to_string())?;
    if normalized_alias != alias {
        return Err("--mlx-model-alias must already be in canonical normalized form".into());
    }

    let metadata = lucidd::inspect_mlx_bundle(&bundle).map_err(|error| error.to_string())?;
    if metadata.context_length.is_none() {
        return Err(
            "verified MLX bundle config.json does not declare an unambiguous supported context length"
                .into(),
        );
    }
    let config = MlxConfig::new(runtime, bundle, metadata.model_cid, cli.mlx_server_port);
    let probe = MlxWorker::new(NodeIdentity::generate(), config.clone())
        .map_err(|error| error.to_string())?;
    if probe.bundle_metadata() != metadata {
        return Err("MLX bundle metadata changed during startup preflight".into());
    }

    Ok(PreparedMlxStartup {
        config,
        alias: normalized_alias,
        metadata,
    })
}

/// SEC-09: validate one TXT-derived bootstrap string. Returns `Some` only
/// for a multiaddr that (a) starts with `/` (multiaddr shape) and (b)
/// pins a `/p2p/<peer-id>` component. The PeerID pin is what makes a
/// spoofed record at worst "dial this host" rather than "trust this
/// identity" — libp2p Noise rejects the handshake if the host can't prove
/// the pinned PeerID.
fn validate_bootstrap_multiaddr(raw: &str) -> Option<String> {
    let s = raw.trim();
    let parsed: Multiaddr = s.parse().ok()?;
    let components = parsed
        .iter()
        .map(|component| component.to_string())
        .collect::<Vec<_>>();
    let peer_components = components
        .iter()
        .filter(|component| component.starts_with("/p2p/"))
        .collect::<Vec<_>>();
    if peer_components.len() != 1 || components.last() != peer_components.first().copied() {
        return None;
    }
    Some(parsed.to_string())
}

/// SEC-09: filter candidate TXT strings down to valid, PeerID-pinned
/// multiaddrs, capping at `cap`. Pure + testable; the async resolver feeds
/// it the decoded TXT chunks. Returns `(accepted, truncated)`.
fn collect_valid_bootstrap_addrs<'a, I>(candidates: I, cap: usize) -> (Vec<String>, bool)
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out = Vec::new();
    let mut truncated = false;
    for cand in candidates {
        if let Some(valid) = validate_bootstrap_multiaddr(cand) {
            if out.len() >= cap {
                truncated = true;
                break;
            }
            out.push(valid);
        }
    }
    (out, truncated)
}

/// Validate a relay candidate as a concrete transport multiaddr ending in a
/// pinned PeerId, then derive the circuit-listen address. Appending only after
/// an exact terminal `/p2p/<peer>` prevents an attacker-controlled suffix from
/// changing which relay receives the reservation.
fn relay_reservation_target(raw: &str) -> Result<(PeerId, String), String> {
    let candidate = raw.trim().trim_end_matches('/');
    if candidate.is_empty() || candidate.contains("/p2p-circuit") {
        return Err("relay candidate must be a direct, peer-pinned multiaddr".to_string());
    }
    let segments = candidate.split('/').collect::<Vec<_>>();
    if segments.len() < 3 || segments[segments.len() - 2] != "p2p" {
        return Err("relay candidate must end in /p2p/<peer-id>".to_string());
    }
    let peer_id = segments
        .last()
        .ok_or_else(|| "relay candidate is missing its peer id".to_string())?
        .parse::<PeerId>()
        .map_err(|_| "relay candidate contains an invalid peer id".to_string())?;
    // `Discovery::listen` performs the authoritative Multiaddr parse before
    // the value reaches libp2p. Validate the base here too so malformed
    // operator input fails before a background task is started.
    candidate
        .parse::<Multiaddr>()
        .map_err(|_| "relay candidate is not a valid multiaddr".to_string())?;
    Ok((peer_id, format!("{candidate}/p2p-circuit")))
}

fn identity_sidecar_path(identity_path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = identity_path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_else(|| "identity".into());
    name.push(suffix);
    identity_path.with_file_name(name)
}

fn start_reachability_maintenance(
    discovery: Arc<Discovery>,
    rendezvous_nodes: Vec<PeerId>,
    register_worker: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let now = tokio::time::Instant::now();
        let mut registration_due = rendezvous_nodes
            .iter()
            .copied()
            .map(|peer| (peer, now))
            .collect::<std::collections::HashMap<_, _>>();
        let mut retry_soon = true;
        loop {
            let mut has_relay_address = false;
            match discovery.listen_addrs().await {
                Ok(addresses) => {
                    for address in addresses {
                        if address.to_string().contains("/p2p-circuit") {
                            has_relay_address = true;
                            if let Err(error) = discovery.add_external_address(address).await {
                                tracing::debug!(%error, "relay address is not yet externally advertisable");
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not inspect relay reservation addresses");
                }
            }

            let now = tokio::time::Instant::now();
            for rendezvous_node in &rendezvous_nodes {
                let registration_is_due = registration_due
                    .get(rendezvous_node)
                    .is_none_or(|due| now >= *due);
                if register_worker && has_relay_address && registration_is_due {
                    match tokio::time::timeout(
                        REACHABILITY_OPERATION_TIMEOUT,
                        discovery.register_rendezvous(
                            *rendezvous_node,
                            LUCID_RENDEZVOUS_NAMESPACE,
                            Some(RENDEZVOUS_TTL_SECONDS),
                        ),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            registration_due.insert(
                                *rendezvous_node,
                                now + std::time::Duration::from_secs(90 * 60),
                            );
                            tracing::info!(
                                relay = %rendezvous_node,
                                namespace = LUCID_RENDEZVOUS_NAMESPACE,
                                "worker rendezvous registration refreshed"
                            );
                        }
                        Ok(Err(error)) => {
                            retry_soon = true;
                            registration_due
                                .insert(*rendezvous_node, now + REACHABILITY_RETRY_INTERVAL);
                            tracing::warn!(relay = %rendezvous_node, %error, "worker rendezvous registration failed");
                        }
                        Err(_) => {
                            retry_soon = true;
                            registration_due
                                .insert(*rendezvous_node, now + REACHABILITY_RETRY_INTERVAL);
                            tracing::warn!(relay = %rendezvous_node, "worker rendezvous registration timed out");
                        }
                    }
                }

                match tokio::time::timeout(
                    REACHABILITY_OPERATION_TIMEOUT,
                    discovery.discover_rendezvous(
                        *rendezvous_node,
                        Some(LUCID_RENDEZVOUS_NAMESPACE),
                        RENDEZVOUS_DISCOVER_LIMIT,
                    ),
                )
                .await
                {
                    Ok(Ok(peers)) => tracing::debug!(
                        relay = %rendezvous_node,
                        peers = peers.len(),
                        "bounded rendezvous discovery refreshed"
                    ),
                    Ok(Err(error)) => {
                        retry_soon = true;
                        tracing::warn!(relay = %rendezvous_node, %error, "rendezvous discovery failed");
                    }
                    Err(_) => {
                        retry_soon = true;
                        tracing::warn!(relay = %rendezvous_node, "rendezvous discovery timed out");
                    }
                }
            }
            tokio::time::sleep(if retry_soon {
                REACHABILITY_RETRY_INTERVAL
            } else {
                RENDEZVOUS_REFRESH_INTERVAL
            })
            .await;
            retry_soon = false;
        }
    })
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
async fn resolve_dns_bootstrap_peers(domains: &[String], allow_fallback: bool) -> Vec<String> {
    if domains.is_empty() {
        return Vec::new();
    }
    // hickory-resolver 0.26 (SEC-02): the 0.24-era `TokioAsyncResolver::tokio*`
    // free functions were replaced by a builder. `builder_tokio()` reads the
    // system resolv.conf (same source as the old `tokio_from_system_conf`) and
    // `.build()` finalises the resolver. Both can fail, so we collapse them and
    // route any error through the same fail-closed / --dns-fallback gate.
    let resolver = match hickory_resolver::TokioResolver::builder_tokio()
        .and_then(|builder| builder.build())
    {
        Ok(r) => r,
        Err(e) => {
            // SEC-09: failing to load the system resolver config (containers
            // without /etc/resolv.conf, locked-down sandboxes) used to
            // silently fall back to public resolvers, widening the
            // trusted-resolver set without the operator knowing. Now that
            // fallback is gated behind `--dns-fallback`; without it we fail
            // closed and log loudly.
            if !allow_fallback {
                tracing::error!(
                    error = %e,
                    "could not load system DNS config and --dns-fallback not set; \
                     SKIPPING DNS bootstrap (fail-closed). Pass --dns-fallback to \
                     explicitly opt into Cloudflare/Google public resolvers."
                );
                return Vec::new();
            }
            tracing::warn!(
                error = %e,
                "could not load system DNS config; --dns-fallback set, \
                 FALLING BACK TO PUBLIC RESOLVERS (Cloudflare/Google) — \
                 bootstrap records are now trust-on-first-use via a public resolver"
            );
            // 0.26 dropped the `ResolverConfig::cloudflare()` preset; the
            // equivalent is `udp_and_tcp(&config::CLOUDFLARE)` (same 1.1.1.1 /
            // 1.0.0.1 + v6 IP set). Build via the explicit-config builder.
            match hickory_resolver::TokioResolver::builder_with_config(
                hickory_resolver::config::ResolverConfig::udp_and_tcp(
                    &hickory_resolver::config::CLOUDFLARE,
                ),
                hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
            )
            .build()
            {
                Ok(resolver) => resolver,
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "failed to construct explicitly requested public DNS resolver"
                    );
                    return Vec::new();
                }
            }
        }
    };
    let mut out = Vec::new();
    for domain in domains {
        match resolver.txt_lookup(domain).await {
            Ok(answers) => {
                // Decode every TXT chunk to UTF-8 up front, then filter +
                // cap via the shared helper. SEC-09: require multiaddr shape
                // + a pinned `/p2p/<peer-id>` (a spoofed record can then at
                // most make us dial a host, never impersonate an identity —
                // Noise enforces the pin), and cap per domain so an
                // oversized TXT set can't flood us into dialing thousands.
                // hickory 0.26 (SEC-02): `txt_lookup` now returns a plain
                // `Lookup`; iterate `answers()` and pull TXT rdata via a match
                // (the 0.24 `TxtLookup::iter()` + `record.txt_data()` accessor
                // were removed). `TXT::txt_data` is now a public field. Same
                // flattened set of UTF-8 chunks as before — pure API shape change.
                use hickory_resolver::proto::rr::RData;
                let candidates: Vec<String> = answers
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::TXT(txt) => Some(txt.txt_data.to_vec()),
                        _ => None,
                    })
                    .flatten()
                    .filter_map(|chunk| std::str::from_utf8(&chunk).ok().map(|s| s.to_string()))
                    .collect();
                let (accepted, truncated) = collect_valid_bootstrap_addrs(
                    candidates.iter().map(|s| s.as_str()),
                    MAX_BOOTSTRAP_ADDRS_PER_DOMAIN,
                );
                let domain_count = accepted.len();
                out.extend(accepted);
                if truncated {
                    tracing::warn!(
                        domain = %domain,
                        cap = MAX_BOOTSTRAP_ADDRS_PER_DOMAIN,
                        "DNS bootstrap records truncated at cap; extra records dropped"
                    );
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

    if cli.redundant_verification_permille > 1_000 {
        return Err("--redundant-verification-permille must be in 0..=1000".into());
    }
    if cli
        .blocked_peers
        .len()
        .saturating_add(cli.pinned_peers.len())
        > MAX_OPERATOR_OVERRIDES
    {
        return Err(format!(
            "at most {MAX_OPERATOR_OVERRIDES} combined --block-peer/--pin-peer values are allowed"
        )
        .into());
    }
    if cli.llama_ctx_size == 0 || cli.llama_ctx_size > u32::MAX as usize {
        return Err("--llama-ctx-size must be in 1..=4294967295".into());
    }

    let port: u16 = std::env::var("LUCIDD_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(11434);

    // Bind to localhost by default — the research brief flagged unauth
    // exposure on :11434 as a real gotcha. LUCID M7 will gate any
    // non-loopback bind behind explicit policy.
    let host: String = std::env::var("LUCIDD_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    validate_http_bind_addr(addr)?;

    let no_local_worker = cli.no_local_worker || cli.mode != NodeMode::Worker;
    if cli.mode == NodeMode::Infrastructure && cli.external_addresses.is_empty() {
        return Err(
            "--mode infrastructure requires at least one operator-confirmed --external-address"
                .into(),
        );
    }
    if cli.mode != NodeMode::Infrastructure && (cli.no_relay_server || cli.no_autonat_server) {
        return Err(
            "--no-relay-server and --no-autonat-server require --mode infrastructure".into(),
        );
    }
    let worker_choice = if no_local_worker {
        None
    } else {
        Some(cli.worker.ok_or(
            "--worker is required in worker mode; use --worker llama-cpp or --worker mlx for real inference, or --worker echo only for development/testing",
        )?)
    };
    let prepared_mlx_startup = if worker_choice == Some(WorkerChoice::Mlx) {
        Some(prepare_mlx_startup(
            &cli,
            cfg!(all(target_os = "macos", target_arch = "aarch64")),
        )?)
    } else {
        None
    };

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
    let mut bootstrap_domains = cli.bootstrap_dns.clone();
    if !cli.no_default_bootstrap
        && !bootstrap_domains
            .iter()
            .any(|domain| domain == DEFAULT_BOOTSTRAP_DOMAIN)
    {
        bootstrap_domains.push(DEFAULT_BOOTSTRAP_DOMAIN.to_string());
    }
    let dns_peers = resolve_dns_bootstrap_peers(&bootstrap_domains, cli.dns_fallback).await;
    if !dns_peers.is_empty() {
        tracing::info!(
            total = dns_peers.len(),
            domains = bootstrap_domains.len(),
            "merged DNS-resolved bootstrap peers"
        );
        bootstrap_peers.extend(dns_peers);
    }
    let resolved_bootstrap_peers = bootstrap_peers.clone();

    // Build the phase-net discovery layer. mDNS may be denied in
    // restricted CI envs — that's expected; the daemon still serves
    // local requests in that case.
    let disc_config = DiscoveryConfig {
        identity: Some(node_identity.clone()),
        bootstrap_peers,
        ..DiscoveryConfig::default()
    };
    let reachability = if cli.mode == NodeMode::Infrastructure {
        ReachabilityConfig {
            role: ReachabilityRole::Infrastructure,
            autonat_server: !cli.no_autonat_server,
            relay_server: (!cli.no_relay_server).then(RelayServerLimits::default),
            // The upstream rendezvous store has TTL bounds but no global,
            // per-peer, or per-namespace registration quotas. Public serving
            // stays fail-closed until that admission layer exists.
            rendezvous_server: None,
            ..ReachabilityConfig::default()
        }
    } else {
        ReachabilityConfig::default()
    };
    let discovery = Arc::new(Discovery::new_with_reachability(disc_config, reachability)?);

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
    let listen_v4_ok = match discovery.listen(&listen_v4).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, addr = %listen_v4, "discovery IPv4 listen failed");
            false
        }
    };
    let listen_v6_ok = match discovery.listen(&listen_v6).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, addr = %listen_v6, "discovery IPv6 listen failed");
            false
        }
    };
    if cli.mode == NodeMode::Infrastructure && !listen_v4_ok && !listen_v6_ok {
        return Err("infrastructure mode could not establish any libp2p listener".into());
    }
    for address in &cli.external_addresses {
        let parsed = address
            .parse()
            .map_err(|error| format!("invalid --external-address {address:?}: {error}"))?;
        discovery.add_external_address(parsed).await?;
    }
    if let Err(e) = discovery.bootstrap().await {
        tracing::warn!(error = %e, "discovery bootstrap failed (continuing)");
    }

    // Ordinary nodes actively reserve circuit paths and use the same pinned
    // public peers as rendezvous servers. Relay client support is independent
    // of the infrastructure/server role: operators can disable server duties
    // without losing their own NAT traversal path.
    let _reachability_maintenance = if cli.mode != NodeMode::Infrastructure {
        if cli.relay_peers.len() > MAX_RELAY_CLIENT_CANDIDATES {
            return Err(format!(
                "at most {MAX_RELAY_CLIENT_CANDIDATES} explicit --relay-peer values are allowed"
            )
            .into());
        }
        let explicit_count = cli.relay_peers.len();
        let mut raw_candidates = cli.relay_peers.clone();
        if !cli.no_bootstrap_relays {
            raw_candidates.extend(resolved_bootstrap_peers);
        }
        let mut seen = HashSet::new();
        let mut rendezvous_nodes = Vec::new();
        for (index, raw) in raw_candidates.into_iter().enumerate() {
            let target = relay_reservation_target(&raw);
            let (relay_peer, reservation_addr) = match target {
                Ok(target) => target,
                Err(error) if index < explicit_count => {
                    return Err(format!("invalid --relay-peer {raw:?}: {error}").into());
                }
                Err(error) => {
                    tracing::debug!(candidate = %raw, %error, "bootstrap peer is not a relay candidate");
                    continue;
                }
            };
            if !seen.insert(relay_peer) {
                continue;
            }
            if rendezvous_nodes.len() >= MAX_RELAY_CLIENT_CANDIDATES {
                tracing::warn!(
                    cap = MAX_RELAY_CLIENT_CANDIDATES,
                    "relay candidate set truncated at the client cap"
                );
                break;
            }
            match discovery.listen(&reservation_addr).await {
                Ok(()) => {
                    tracing::info!(relay = %relay_peer, "circuit-relay reservation requested");
                    rendezvous_nodes.push(relay_peer);
                }
                Err(error) => {
                    tracing::warn!(relay = %relay_peer, %error, "circuit-relay reservation request failed");
                }
            }
        }
        if rendezvous_nodes.is_empty() {
            None
        } else {
            Some(start_reachability_maintenance(
                discovery.clone(),
                rendezvous_nodes,
                !no_local_worker,
            ))
        }
    } else {
        None
    };

    // Model registry, backed by phase-net's Kademlia DHT.
    let transport: Arc<dyn DhtTransport> = Arc::new(PhaseNetDhtTransport::new(discovery.clone()));
    let alias_replay_path = cli
        .alias_replay_state
        .clone()
        .unwrap_or_else(|| identity_sidecar_path(&identity_path, ".alias-replay-state-v1.json"));
    let registry = Arc::new(ModelRegistry::new_with_alias_replay_state(
        node_identity.clone(),
        transport,
        alias_replay_path,
    )?);

    // Operator policy. The engine seeds `~/.config/lucidd/policy.toml`
    // on first run with a fully-commented default.
    let policy = Arc::new(PolicyEngine::load_or_default(cli.policy_config.clone()).await?);

    // Llama execution is rooted in verified content, never directly in the
    // operator's mutable source directory. Consume-only nodes also get a
    // persistent store so `/api/pull` can cache/resume content without
    // falsely claiming to have an inference worker loaded.
    let source_model_dir = if worker_choice == Some(WorkerChoice::LlamaCpp) {
        Some(
            cli.model_dir
                .clone()
                .ok_or("--model-dir is required with --worker llama-cpp")?,
        )
    } else {
        None
    };
    let needs_content_store = source_model_dir.is_some()
        || cli.mode == NodeMode::ConsumeOnly
        || cli.serve_model_content
        || cli.artifact_dir.is_some();
    let content_storage = if needs_content_store {
        let artifact_dir = cli
            .artifact_dir
            .clone()
            .or_else(|| {
                source_model_dir
                    .as_ref()
                    .map(|source| source.join(".phase-artifacts"))
            })
            .or_else(|| dirs::data_local_dir().map(|base| base.join("lucidd").join("artifacts")))
            .ok_or("could not resolve a persistent content-store directory; pass --artifact-dir")?;
        let verified_model_dir = artifact_dir.join("verified-models");
        let artifact_store = Arc::new(ArtifactStore::new(artifact_dir)?);
        std::fs::create_dir_all(&verified_model_dir)?;
        Some((verified_model_dir, artifact_store))
    } else {
        None
    };

    // Optional local worker.
    let local_worker: Option<Arc<dyn DynWorker>> = if no_local_worker {
        tracing::info!(
            mode = ?cli.mode,
            "consume-only / relay node (no local worker loaded)"
        );
        None
    } else {
        match worker_choice.ok_or("worker selection missing after validation")? {
            WorkerChoice::Echo => {
                tracing::info!("worker: echo (no GPU, reverses input)");
                // EchoWorker handles every model_id (it doesn't care
                // about the weights). Advertise a synthetic "echo"
                // entry in the registry so the router's "local has
                // model" check resolves on common Ollama CLI calls.
                //
                // Echo has no content artifact, so its explicit development
                // advertisement uses a clearly non-production name hash.
                // Production alias/pull paths never synthesize this value.
                let echo_cid = lucidd::ModelCid::development_name_hash("echo");
                let caps =
                    lucidd::ModelCapabilities::now("echo", echo_cid, "none", 8192, 16, "echo");
                if let Err(e) = registry.advertise_loaded(caps).await {
                    tracing::warn!(error = %e, "failed to advertise synthetic echo entry");
                }
                let sequence = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                let echo_alias =
                    lucidd::AliasRecord::new("echo", echo_cid, "phase-echo", 1, sequence.max(1))?;
                registry.publish_alias(echo_alias).await?;
                // SEC-05: the worker signs receipts, and a peer verifying a
                // relayed receipt binds `worker_pubkey -> delivering PeerId`.
                // So the worker MUST sign with THIS node's identity (the same
                // key its libp2p PeerId derives from) — a random worker key
                // would make every peer-served receipt fail the bind check.
                Some(Arc::new(EchoWorker {
                    identity: node_identity.clone(),
                    ..EchoWorker::new()
                }) as Arc<dyn DynWorker>)
            }
            WorkerChoice::LlamaCpp => {
                let model_dir = source_model_dir
                    .as_ref()
                    .cloned()
                    .ok_or("llama.cpp source model directory was not initialized")?;
                let (verified_model_dir, artifact_store) = content_storage
                    .as_ref()
                    .cloned()
                    .ok_or("verified model content store was not initialized")?;
                let n_gpu_layers = if cli.llama_n_gpu_layers < 0 {
                    i32::MAX
                } else {
                    cli.llama_n_gpu_layers
                };

                // SEC-04 (L8): resolve the llama-server binary to an
                // absolute, existing path at startup. The default
                // `--llama-server-binary llama-server` relies on the
                // inherited `$PATH` — a binary-hijack vector. Canonicalize
                // it (which also fails fast if it's missing) and require the
                // result to be absolute so the spawn never PATH-resolves.
                let configured_server_binary = cli.llama_server_binary.as_ref().ok_or(
                    "--llama-server-binary is required with --worker llama-cpp and must be an absolute trusted path",
                )?;
                if !configured_server_binary.is_absolute() {
                    return Err("--llama-server-binary must be an absolute trusted path".into());
                }
                let server_binary_path = configured_server_binary.canonicalize().map_err(|e| {
                    format!(
                        "--llama-server-binary {:?} must be an existing regular executable path: {e}",
                        configured_server_binary
                    )
                })?;
                if !server_binary_path.is_absolute() {
                    return Err(format!(
                        "--llama-server-binary resolved to a non-absolute path: {}",
                        server_binary_path.display()
                    )
                    .into());
                }
                let server_metadata = std::fs::metadata(&server_binary_path).map_err(|error| {
                    format!(
                        "cannot inspect --llama-server-binary {}: {error}",
                        server_binary_path.display()
                    )
                })?;
                if !server_metadata.is_file() {
                    return Err("--llama-server-binary must be a regular file".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if server_metadata.permissions().mode() & 0o111 == 0 {
                        return Err("--llama-server-binary is not executable".into());
                    }
                }

                // Auto-detect source GGUFs, verify their content, and publish
                // signed alias→CID records before the worker can execute them.
                {
                    let entries = std::fs::read_dir(&model_dir).map_err(|error| {
                        format!("cannot read --model-dir {}: {error}", model_dir.display())
                    })?;
                    let mut advertised = 0usize;
                    for entry in entries {
                        let entry = entry.map_err(|error| {
                            format!(
                                "cannot enumerate --model-dir {}: {error}",
                                model_dir.display()
                            )
                        })?;
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) != Some("gguf") {
                            continue;
                        }
                        let Some(model_id) =
                            path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
                        else {
                            continue;
                        };
                        match registry
                            .import_verified_gguf(
                                artifact_store.clone(),
                                model_dir.clone(),
                                path,
                                verified_model_dir.clone(),
                                &model_id,
                                cli.llama_ctx_size as u32,
                                1,
                                "llama.cpp",
                            )
                            .await
                        {
                            Ok(caps) => {
                                advertised += 1;
                                tracing::info!(
                                    model = %model_id,
                                    cid = %caps.model_cid.to_hex(),
                                    "verified and advertised local model"
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    model = %model_id,
                                    error = %error,
                                    "failed to verify/import local model"
                                );
                            }
                        }
                    }
                    tracing::info!(
                        count = advertised,
                        source_dir = ?model_dir,
                        verified_dir = ?verified_model_dir,
                        "verified local models"
                    );
                }

                let config = LlamaCppConfig {
                    server_binary_path,
                    model_dir: verified_model_dir,
                    default_n_gpu_layers: n_gpu_layers,
                    default_context_size: cli.llama_ctx_size,
                    ..Default::default()
                };
                tracing::info!(?config, "worker: llama-cpp");
                // SEC-05: sign receipts with THIS node's identity so a peer
                // verifying a relayed receipt can bind worker_pubkey -> our
                // PeerId (a random key would fail that bind). See the echo arm.
                let worker_identity = node_identity.clone();
                Some(Arc::new(LlamaCppWorker::new(worker_identity, config)) as Arc<dyn DynWorker>)
            }
            WorkerChoice::Mlx => {
                let prepared = prepared_mlx_startup
                    .ok_or("MLX startup configuration missing after successful preflight")?;
                let context_length = prepared
                    .metadata
                    .context_length
                    .ok_or("MLX context length missing after successful preflight")?;
                let worker = MlxWorker::new(node_identity.clone(), prepared.config)?;
                worker.preload().await?;
                let metadata = worker.bundle_metadata();
                if metadata != prepared.metadata {
                    return Err(
                        "MLX bundle metadata changed between preflight and worker construction"
                            .into(),
                    );
                }

                let caps = lucidd::ModelCapabilities::now(
                    prepared.alias.clone(),
                    worker.model_cid(),
                    worker.bundle_format(),
                    context_length,
                    worker.advertised_capacity(),
                    "mlx",
                );
                registry.advertise_loaded(caps).await?;
                let sequence = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                registry
                    .publish_alias(lucidd::AliasRecord::new(
                        &prepared.alias,
                        worker.model_cid(),
                        worker.bundle_format(),
                        metadata.total_bytes,
                        sequence.max(1),
                    )?)
                    .await?;
                tracing::info!(
                    model = %prepared.alias,
                    cid = %worker.model_cid().to_hex(),
                    bundle_files = metadata.file_count,
                    bundle_bytes = metadata.total_bytes,
                    runtime_sha256 = ?worker.runtime_executable_sha256(),
                    runtime_attestation = worker.runtime_attestation(),
                    hardware_acceptance = worker.hardware_acceptance(),
                    port_binding = worker.port_binding_status(),
                    "worker: mlx"
                );
                Some(Arc::new(worker) as Arc<dyn DynWorker>)
            }
        }
    };

    // Register the inbound peer-relay handler so other peers can ask us
    // to serve work. Only installed when we have a local worker —
    // consume-only nodes can't help anyone.
    if let Some(worker) = local_worker.clone() {
        let handlers = make_inbound_relay_handlers(worker, registry.clone(), policy.clone());
        if let Err(e) = discovery.set_job_relay_handler(Some(handlers.batch)).await {
            tracing::warn!(error = %e, "set_job_relay_handler failed");
        }
        if let Err(e) = discovery.set_job_relay_stream_handler(Some(handlers.stream)) {
            tracing::warn!(error = %e, "set_job_relay_stream_handler failed");
        }
    }

    // The router itself. Evidence is local, privacy-minimal, and bound to the
    // same persistent PeerId used for transport and receipts. Redundant checks
    // remain disabled unless the operator supplies a non-zero sample cap.
    let evidence_path = cli
        .evidence_store
        .clone()
        .unwrap_or_else(|| identity_sidecar_path(&identity_path, ".reputation-evidence-v1.log"));
    let evidence = Arc::new(EvidenceRuntime::open(
        evidence_path,
        *discovery.local_peer_id(),
    )?);
    let mut configured_overrides = HashSet::new();
    for raw in &cli.pinned_peers {
        let peer: PeerId = raw
            .parse()
            .map_err(|_| format!("invalid --pin-peer PeerId {raw:?}"))?;
        configured_overrides.insert(peer);
        evidence.set_operator_override(
            peer,
            OperatorOverride {
                pinned: true,
                blocked: false,
            },
        )?;
    }
    for raw in &cli.blocked_peers {
        let peer: PeerId = raw
            .parse()
            .map_err(|_| format!("invalid --block-peer PeerId {raw:?}"))?;
        configured_overrides.insert(peer);
        evidence.set_operator_override(
            peer,
            OperatorOverride {
                pinned: false,
                blocked: true,
            },
        )?;
    }
    if !configured_overrides.is_empty() {
        tracing::info!(
            peers = configured_overrides.len(),
            "local reputation overrides configured"
        );
    }
    let _evidence_compaction_task = {
        let evidence = evidence.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match evidence.compact_retaining(DEFAULT_EVIDENCE_RETENTION).await {
                    Ok(removed) if removed > 0 => {
                        tracing::info!(removed, "expired execution evidence compacted");
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(%error, "execution evidence compaction failed");
                    }
                }
            }
        })
    };
    let redundant_config = RedundantVerificationConfig {
        enabled: cli.redundant_verification_permille > 0,
        sample_cap_permille: cli.redundant_verification_permille,
    };
    let router = Arc::new(
        LucidRouter::new(
            local_worker.clone(),
            registry.clone(),
            policy.clone(),
            node_identity.clone(),
            discovery.clone(),
        )
        .with_evidence_runtime(evidence)
        .with_redundant_verification(redundant_config),
    );

    // Outgoing manifests must be signed by the same persistent identity that
    // authenticates this libp2p peer. Inbound peers bind the manifest signer
    // to the delivering PeerId; an ephemeral client key makes normal
    // peer-to-peer requests fail the default authorization policy.
    let client_identity = node_identity.clone();
    let content_plane = match content_storage.as_ref() {
        Some((verified_model_dir, artifact_store)) => Some(Arc::new(ContentPlane::new(
            discovery.clone(),
            registry.clone(),
            artifact_store.clone(),
            verified_model_dir.clone(),
            ContentPlaneConfig {
                publish_provider: cli.serve_model_content,
                local_gguf_activation: (worker_choice == Some(WorkerChoice::LlamaCpp)).then(|| {
                    LocalGgufActivation {
                        context_length: cli.llama_ctx_size as u32,
                        max_concurrent: 1,
                        backend: "llama.cpp".to_string(),
                    }
                }),
                ..ContentPlaneConfig::default()
            },
        )?)),
        None => None,
    };
    if let Some(plane) = content_plane.as_ref() {
        let restored = plane.restore_installed_catalog().await?;
        plane.persist_installed_catalog().await?;
        if !restored.is_empty() {
            tracing::info!(
                count = restored.len(),
                "restored independently verified installed-content catalog"
            );
        }
    }
    if cli.serve_model_content {
        let plane = content_plane
            .as_ref()
            .ok_or("--serve-model-content requires a configured content store")?;
        discovery.set_blob_stream_handler(Some(plane.blob_stream_handler()))?;
        for installed in registry.local_installed_async().await {
            registry
                .publish_installed_content_provider(&installed.model_cid)
                .await?;
        }
        tracing::info!("verified model-content serving enabled");
    }

    let (verified_model_dir, artifact_store) = match content_storage {
        Some((verified, store)) => (Some(verified), Some(store)),
        None => (None, None),
    };
    let state = AppState {
        router,
        client_identity,
        registry: registry.clone(),
        model_dir: source_model_dir,
        artifact_store,
        verified_model_dir,
        content_plane,
    };
    let app = ollama_router(state);

    tracing::info!(%addr, "lucidd listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn validate_http_bind_addr(addr: SocketAddr) -> Result<(), String> {
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing unauthenticated LUCID HTTP bind on non-loopback address {addr}; \
             bind lucidd to 127.0.0.1/::1 and expose it only through an authenticating proxy"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthenticated_http_bind_is_loopback_only() {
        assert!(validate_http_bind_addr("127.0.0.1:11434".parse().unwrap()).is_ok());
        assert!(validate_http_bind_addr("[::1]:11434".parse().unwrap()).is_ok());
        assert!(validate_http_bind_addr("0.0.0.0:11434".parse().unwrap()).is_err());
        assert!(validate_http_bind_addr("192.0.2.10:11434".parse().unwrap()).is_err());
    }

    #[test]
    fn mlx_platform_preflight_fails_before_paths_or_network_are_touched() {
        let cli = Cli::try_parse_from(["lucidd", "--worker", "mlx"])
            .expect("minimal MLX CLI must parse before semantic preflight");
        let error = prepare_mlx_startup(&cli, false).unwrap_err();
        assert_eq!(error, "MLX backend requires macOS on Apple Silicon");
    }

    const GOOD: &str = "/dns4/bootstrap.phasebased.net/tcp/4001/p2p/12D3KooWJwbyF4d4so1sqd1qsSF24dTDtc3DduLYX7v9VruBfpH7";

    #[test]
    fn validate_accepts_pinned_multiaddr() {
        // SEC-09: a well-formed multiaddr with /p2p/<id> is accepted.
        assert_eq!(validate_bootstrap_multiaddr(GOOD), Some(GOOD.to_string()));
        // Leading/trailing whitespace tolerated (TXT records often padded).
        assert_eq!(
            validate_bootstrap_multiaddr(&format!("  {GOOD}  ")),
            Some(GOOD.to_string())
        );
    }

    #[test]
    fn validate_rejects_missing_p2p_pin() {
        // SEC-09: multiaddr shape but no /p2p/<id> → rejected.
        assert_eq!(validate_bootstrap_multiaddr("/ip4/1.2.3.4/tcp/4001"), None);
        // /p2p present but empty value → rejected.
        assert_eq!(
            validate_bootstrap_multiaddr("/ip4/1.2.3.4/tcp/4001/p2p/"),
            None
        );
        // Not a multiaddr at all (no leading slash) → rejected.
        assert_eq!(validate_bootstrap_multiaddr("evil.example.com"), None);
        assert_eq!(validate_bootstrap_multiaddr(""), None);
        let second = "12D3KooWPFG2jjhoRd3bWdZnAwAEhBRJp7jNDE5KZQynxSkHKAQH";
        assert_eq!(
            validate_bootstrap_multiaddr(&format!("{GOOD}/p2p/{second}")),
            None,
            "multiple identity components are ambiguous"
        );
        assert_eq!(
            validate_bootstrap_multiaddr(&format!("{GOOD}/p2p-circuit")),
            None,
            "bootstrap addresses must terminate at the pinned peer"
        );
    }

    #[test]
    fn collect_caps_record_count() {
        // SEC-09: a synthetic set well over the cap keeps only `cap`.
        let many: Vec<String> = (0..1000).map(|_| GOOD.to_string()).collect();
        let (kept, truncated) = collect_valid_bootstrap_addrs(
            many.iter().map(|s| s.as_str()),
            MAX_BOOTSTRAP_ADDRS_PER_DOMAIN,
        );
        assert_eq!(kept.len(), MAX_BOOTSTRAP_ADDRS_PER_DOMAIN);
        assert!(truncated, "expected truncation flag set");
    }

    #[test]
    fn collect_filters_invalid_and_does_not_truncate_under_cap() {
        // SEC-09: invalid records dropped, valid kept, no false truncation.
        let inputs = vec![GOOD, "/ip4/1.2.3.4/tcp/4001", "garbage", GOOD];
        let (kept, truncated) =
            collect_valid_bootstrap_addrs(inputs, MAX_BOOTSTRAP_ADDRS_PER_DOMAIN);
        assert_eq!(kept.len(), 2);
        assert!(!truncated);
    }

    #[test]
    fn relay_target_requires_terminal_valid_peer_pin_and_derives_circuit_address() {
        let direct =
            "/dns4/relay.example/tcp/4001/p2p/12D3KooWJwbyF4d4so1sqd1qsSF24dTDtc3DduLYX7v9VruBfpH7";
        let (peer, reservation) = relay_reservation_target(direct).unwrap();
        assert_eq!(
            peer.to_string(),
            "12D3KooWJwbyF4d4so1sqd1qsSF24dTDtc3DduLYX7v9VruBfpH7"
        );
        assert_eq!(reservation, format!("{direct}/p2p-circuit"));

        assert!(relay_reservation_target("/ip4/127.0.0.1/tcp/4001").is_err());
        assert!(relay_reservation_target(&format!("{direct}/p2p-circuit")).is_err());
        assert!(relay_reservation_target("/ip4/127.0.0.1/tcp/4001/p2p/not-a-peer").is_err());
    }

    #[test]
    fn identity_sidecars_are_unique_per_identity_filename() {
        let first = identity_sidecar_path(
            std::path::Path::new("/tmp/node-a.key"),
            ".alias-replay-state-v1.json",
        );
        let second = identity_sidecar_path(
            std::path::Path::new("/tmp/node-b.key"),
            ".alias-replay-state-v1.json",
        );
        assert_eq!(
            first,
            std::path::Path::new("/tmp/node-a.key.alias-replay-state-v1.json")
        );
        assert_ne!(first, second);
    }
}
