//! Phase Discover - Boot-time manifest discovery via libp2p DHT
//!
//! This is a minimal binary for Phase Boot that discovers boot manifests
//! via the Phase network's Kademlia DHT. It reuses the plasm library's
//! networking infrastructure.
//!
//! Usage:
//!   phase-discover --arch x86_64 --channel stable
//!   phase-discover --arch arm64 --channel testing --ephemeral

use std::time::Duration;
use clap::Parser;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;

use libp2p::{
    kad::{self, store::MemoryStore, Mode, QueryResult, GetRecordOk, RecordKey},
    noise, yamux,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, Multiaddr, PeerId,
};
use futures::StreamExt;

/// Phase Discover - Boot-time manifest discovery
#[derive(Parser, Debug)]
#[command(name = "phase-discover")]
#[command(about = "Discover Phase boot manifests via libp2p DHT")]
#[command(version)]
struct Args {
    /// Target architecture (x86_64, arm64)
    #[arg(short, long, default_value = "x86_64")]
    arch: String,

    /// Update channel (stable, testing)
    #[arg(short, long, default_value = "stable")]
    channel: String,

    /// Use ephemeral identity (Private Mode)
    #[arg(long)]
    ephemeral: bool,

    /// Bootstrap nodes (multiaddr format)
    #[arg(short, long)]
    bootstrap: Vec<String>,

    /// Discovery timeout in seconds
    #[arg(short, long, default_value = "30")]
    timeout: u64,

    /// Output format (text, json)
    #[arg(short, long, default_value = "text")]
    format: String,

    /// Quiet mode (only output manifest URL)
    #[arg(short, long)]
    quiet: bool,
}

/// Combined network behaviour for discovery
#[derive(NetworkBehaviour)]
struct DiscoverBehaviour {
    kademlia: kad::Behaviour<MemoryStore>,
}

/// Manifest discovery result
#[derive(Debug, serde::Serialize)]
struct DiscoveryResult {
    key: String,
    manifest_url: String,
    peer_id: String,
    provider_count: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize logging (unless quiet mode)
    if !args.quiet {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .with_target(false)
            .compact()
            .finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }

    // Generate identity (ephemeral or persistent)
    let local_key = if args.ephemeral {
        if !args.quiet {
            info!("Using ephemeral identity (Private Mode)");
        }
        libp2p::identity::Keypair::generate_ed25519()
    } else {
        // In non-ephemeral mode, we could load from disk
        // For now, still generate (boot environment has no persistent storage yet)
        if !args.quiet {
            info!("Generating session identity");
        }
        libp2p::identity::Keypair::generate_ed25519()
    };
    let local_peer_id = PeerId::from(local_key.public());

    if !args.quiet {
        info!("Local peer ID: {}", local_peer_id);
    }

    // Build DHT key for manifest lookup
    let manifest_key = format!("/phase/{}/{}/manifest", args.channel, args.arch);
    if !args.quiet {
        info!("Looking up: {}", manifest_key);
    }

    // Build the swarm
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            let peer_id = PeerId::from(key.public());
            let store = MemoryStore::new(peer_id);
            let mut kademlia = kad::Behaviour::new(peer_id, store);
            kademlia.set_mode(Some(Mode::Client));
            DiscoverBehaviour { kademlia }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Listen on random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Add bootstrap nodes
    let bootstrap_nodes = if args.bootstrap.is_empty() {
        default_bootstrap_nodes()
    } else {
        args.bootstrap.clone()
    };

    for addr_str in &bootstrap_nodes {
        if let Ok(addr) = addr_str.parse::<Multiaddr>() {
            if let Some(peer_id) = extract_peer_id(&addr) {
                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                if !args.quiet {
                    info!("Added bootstrap node: {}", peer_id);
                }
            }
        }
    }

    // Bootstrap the DHT
    if let Err(e) = swarm.behaviour_mut().kademlia.bootstrap() {
        if !args.quiet {
            warn!("Bootstrap failed (no nodes): {}", e);
        }
    }

    // Start the DHT query
    let record_key = RecordKey::new(&manifest_key);
    let query_id = swarm.behaviour_mut().kademlia.get_record(record_key.clone());

    if !args.quiet {
        info!("Started DHT query: {:?}", query_id);
    }

    // Run event loop with timeout
    let timeout_duration = Duration::from_secs(args.timeout);
    let start_time = std::time::Instant::now();

    loop {
        if start_time.elapsed() > timeout_duration {
            if !args.quiet {
                error!("Discovery timeout after {}s", args.timeout);
            }
            std::process::exit(1);
        }

        tokio::select! {
            event = swarm.select_next_some() => {
                match event {
                    SwarmEvent::Behaviour(DiscoverBehaviourEvent::Kademlia(kad_event)) => {
                        match kad_event {
                            kad::Event::OutboundQueryProgressed { result, .. } => {
                                match result {
                                    QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(record))) => {
                                        // Found the manifest!
                                        let value = String::from_utf8_lossy(&record.record.value);

                                        if args.format == "json" {
                                            let result = DiscoveryResult {
                                                key: manifest_key.clone(),
                                                manifest_url: value.to_string(),
                                                peer_id: local_peer_id.to_string(),
                                                provider_count: 1,
                                            };
                                            println!("{}", serde_json::to_string(&result)?);
                                        } else if args.quiet {
                                            println!("{}", value);
                                        } else {
                                            info!("Found manifest!");
                                            println!("MANIFEST_URL={}", value);
                                        }

                                        return Ok(());
                                    }
                                    QueryResult::GetRecord(Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. })) => {
                                        // Query finished, no more records
                                    }
                                    QueryResult::GetRecord(Err(err)) => {
                                        if !args.quiet {
                                            warn!("DHT query error: {:?}", err);
                                        }
                                    }
                                    QueryResult::Bootstrap(Ok(_)) => {
                                        if !args.quiet {
                                            info!("Bootstrap successful");
                                        }
                                    }
                                    QueryResult::Bootstrap(Err(e)) => {
                                        if !args.quiet {
                                            warn!("Bootstrap error: {:?}", e);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            kad::Event::RoutingUpdated { peer, .. } => {
                                if !args.quiet {
                                    info!("Discovered peer: {}", peer);
                                }
                            }
                            _ => {}
                        }
                    }
                    SwarmEvent::NewListenAddr { address, .. } => {
                        if !args.quiet {
                            info!("Listening on: {}", address);
                        }
                    }
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        if !args.quiet {
                            info!("Connected to: {}", peer_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Default bootstrap nodes for the Phase network
fn default_bootstrap_nodes() -> Vec<String> {
    // TODO: Replace with actual Phase bootstrap nodes when deployed
    // For now, return empty - local testing uses --bootstrap flag
    vec![
        // Example format:
        // "/ip4/1.2.3.4/tcp/4001/p2p/12D3KooWABC..."
    ]
}

/// Extract PeerId from multiaddr if present
fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(peer_id) => Some(peer_id),
        _ => None,
    })
}
