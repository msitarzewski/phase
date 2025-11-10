use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, warn};
use tracing_subscriber;

// Use the plasm library crate
use plasm::{
    config::Config,
    network::{Discovery, DiscoveryConfig, ExecutionHandler, JobRequest, JobRequirements},
    wasm::runtime::{WasmRuntime, Wasm3Runtime},
};

#[derive(Parser)]
#[command(name = "plasmd")]
#[command(about = "Phase local WASM execution daemon", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a default config file
    Init {
        /// Where to create the config file (default: ~/.config/plasm/config.json)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Start the daemon
    Start {
        /// Configuration file path (default: auto-detect from ~/.config/plasm/ or /etc/plasm/)
        #[arg(short, long)]
        config: Option<String>,

        /// Listen addresses (overrides config file, can be specified multiple times)
        /// Format: /ip4/0.0.0.0/tcp/8000 or /ip4/192.168.1.144/tcp/8000
        #[arg(short, long)]
        listen: Vec<String>,

        /// Peer multiaddrs to connect to (overrides config file, can be specified multiple times)
        /// Format: /ip4/192.168.1.25/tcp/12345/p2p/12D3Koo...
        /// Or: /ip4/192.168.1.25/tcp/12345 (peer ID will be discovered)
        #[arg(short, long)]
        peer: Vec<String>,
    },
    /// Execute a WASM module locally (for testing)
    Run {
        /// Path to WASM file
        wasm_file: String,

        /// Arguments to pass to WASM module
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        /// Quiet mode: suppress logs, output only WASM stdout
        #[arg(short, long)]
        quiet: bool,
    },
    /// Execute a job from JSON request (for testing M3 signing)
    ExecuteJob {
        /// Path to WASM file
        wasm_file: String,

        /// Arguments to pass to WASM module
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Show version information
    Version,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => {
            let config_path = if let Some(p) = path {
                let path = std::path::PathBuf::from(p);
                let config = Config::default();
                config.save(&path)?;
                path
            } else {
                Config::init_user_config()?
            };

            println!("Initialized config at: {}", config_path.display());
            println!("\nDefault config:");
            println!("{}", serde_json::to_string_pretty(&Config::default())?);
            println!("\nEdit this file to configure:");
            println!("  - listen_addrs: Addresses to listen on");
            println!("  - peer_addrs: Peers to connect to on startup");
            println!("  - bootstrap_peers: DHT bootstrap nodes");

            Ok(())
        }
        Commands::Start { config, listen, peer } => {
            // Load config from file (with fallback chain)
            let mut cfg = Config::load_or_default(config.as_deref())?;

            // CLI flags override config file
            if !listen.is_empty() {
                cfg.listen_addrs = listen;
            }
            if !peer.is_empty() {
                cfg.peer_addrs = peer;
            }

            // Determine listen addresses (use default if empty)
            let listen_addrs = if cfg.listen_addrs.is_empty() {
                vec!["/ip4/0.0.0.0/tcp/0".to_string()]
            } else {
                cfg.listen_addrs.clone()
            };

            info!("Starting plasmd");
            if let Some(user_config) = Config::default_user_config_path() {
                info!("Config search path: {}", user_config.display());
            }

            // Create discovery configuration
            let disc_config = DiscoveryConfig::default();

            // Create discovery service
            let mut discovery = Discovery::new(disc_config)?;

            // Start listening on configured addresses
            for addr in &listen_addrs {
                discovery.listen(addr)?;
            }

            // Bootstrap DHT
            discovery.bootstrap()?;

            // Advertise this node's capabilities
            discovery.advertise_capabilities()?;

            info!("Phase daemon started. Peer ID: {}", discovery.local_peer_id());
            info!("Capabilities: {:?}", discovery.capabilities());

            // Dial configured peers
            for peer_addr in &cfg.peer_addrs {
                info!("Connecting to peer: {}", peer_addr);
                if let Err(e) = discovery.dial_peer(peer_addr) {
                    warn!("Failed to dial peer {}: {}", peer_addr, e);
                } else {
                    info!("Dialing initiated to: {}", peer_addr);
                }
            }

            // Run event loop
            discovery.run().await?;

            Ok(())
        }
        Commands::Run { wasm_file, args, quiet } => {
            if !quiet {
                info!("Executing WASM file: {}", wasm_file);
            }

            // Load WASM file
            let wasm_bytes = std::fs::read(&wasm_file)?;

            // Convert args to &[&str]
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

            // Create runtime and execute
            let runtime = Wasm3Runtime::new();
            let result = runtime.execute(&wasm_bytes, &args_refs).await?;

            // In quiet mode, suppress logs - WASM stdout is already inherited
            // In normal mode, show the placeholder message
            if !quiet {
                println!("{}", result.stdout);
            }

            std::process::exit(result.exit_code as i32);
        }
        Commands::ExecuteJob { wasm_file, args } => {
            use sha2::{Digest, Sha256};

            info!("Executing job from: {}", wasm_file);

            // Load WASM file
            let wasm_bytes = std::fs::read(&wasm_file)?;

            // Compute module hash
            let mut hasher = Sha256::new();
            hasher.update(&wasm_bytes);
            let module_hash = format!("sha256:{}", hex::encode(hasher.finalize()));

            // Create job request
            let request = JobRequest::new(
                format!("job-{}", uuid::Uuid::new_v4()),
                module_hash,
                wasm_bytes,
                args,
                JobRequirements {
                    cpu_cores: 1,
                    memory_mb: 128,
                    timeout_seconds: 30,
                    arch: std::env::consts::ARCH.to_string(),
                    wasm_runtime: format!("wasmtime-{}", env!("CARGO_PKG_VERSION")),
                },
            );

            // Create execution handler with a fresh signing key
            use ed25519_dalek::SigningKey;
            use rand::RngCore;
            let mut secret_bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut secret_bytes);
            let signing_key = SigningKey::from_bytes(&secret_bytes);
            let handler = ExecutionHandler::new(signing_key);

            // Execute job
            let result = handler.execute_job(request).await?;

            // Output result as JSON
            let json_output = serde_json::json!({
                "job_id": result.job_id,
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "receipt": serde_json::from_str::<serde_json::Value>(&result.receipt_json)?
            });

            println!("{}", serde_json::to_string_pretty(&json_output)?);

            Ok(())
        }
        Commands::Version => {
            println!("plasmd version {}", env!("CARGO_PKG_VERSION"));
            println!("Phase Open MVP - Local WASM Execution");
            Ok(())
        }
    }
}
