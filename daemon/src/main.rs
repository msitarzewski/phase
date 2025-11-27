use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{info, warn};
use tracing_subscriber;

// Use the plasm library crate
use plasm::{
    config::Config,
    network::{Discovery, DiscoveryConfig, ExecutionHandler, JobRequest, JobRequirements},
    provider::{ProviderConfig, ProviderServer},
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
    /// Start boot artifact provider server
    Serve {
        /// Artifacts directory
        #[arg(short = 'a', long)]
        artifacts: Option<PathBuf>,

        /// Release channel
        #[arg(short = 'c', long, default_value = "stable")]
        channel: String,

        /// Architecture (auto-detect if not specified)
        #[arg(short = 'A', long)]
        arch: Option<String>,

        /// HTTP port
        #[arg(short = 'p', long, default_value = "8080")]
        port: u16,

        /// Bind address
        #[arg(short = 'b', long, default_value = "0.0.0.0")]
        bind: String,

        /// Disable DHT advertisement
        #[arg(long)]
        no_dht: bool,

        /// Disable mDNS advertisement
        #[arg(long)]
        no_mdns: bool,
    },
    /// Provider management commands
    Provider {
        #[command(subcommand)]
        command: ProviderCommands,
    },
    /// Show version information
    Version,
}

#[derive(Subcommand)]
enum ProviderCommands {
    /// Show provider status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Provider HTTP address
        #[arg(short, long, default_value = "http://localhost:8080")]
        addr: String,
    },
    /// List available artifacts
    List {
        /// Provider HTTP address
        #[arg(short, long, default_value = "http://localhost:8080")]
        addr: String,
    },
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
        Commands::Serve {
            artifacts,
            channel,
            arch,
            port,
            bind,
            no_dht,
            no_mdns,
        } => {
            // Build provider config
            let config = ProviderConfig {
                enabled: true,
                bind_addr: bind,
                port,
                artifacts_dir: artifacts.unwrap_or_else(|| {
                    // Use the default_artifacts_dir from config module
                    #[cfg(target_os = "linux")]
                    {
                        PathBuf::from("/var/lib/plasm/artifacts")
                    }
                    #[cfg(target_os = "macos")]
                    {
                        dirs::data_local_dir()
                            .map(|mut path| {
                                path.push("plasm");
                                path.push("artifacts");
                                path
                            })
                            .unwrap_or_else(|| PathBuf::from("/usr/local/var/plasm/artifacts"))
                    }
                    #[cfg(target_os = "windows")]
                    {
                        dirs::data_local_dir()
                            .map(|mut path| {
                                path.push("plasm");
                                path.push("artifacts");
                                path
                            })
                            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData\\plasm\\artifacts"))
                    }
                    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
                    {
                        PathBuf::from("/var/lib/plasm/artifacts")
                    }
                }),
                channel,
                arch: arch.unwrap_or_else(|| {
                    // Auto-detect architecture
                    #[cfg(target_arch = "x86_64")]
                    {
                        "x86_64".to_string()
                    }
                    #[cfg(target_arch = "aarch64")]
                    {
                        "aarch64".to_string()
                    }
                    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
                    {
                        std::env::consts::ARCH.to_string()
                    }
                }),
            };

            // Display startup banner
            println!("╔══════════════════════════════════════════════╗");
            println!("║           Phase Boot Provider                ║");
            println!("╠══════════════════════════════════════════════╣");
            println!("║ HTTP:     http://{}:{:<21} ║", config.bind_addr, config.port);
            println!("║ Artifacts: {:<33} ║", config.artifacts_dir.display().to_string().chars().take(33).collect::<String>());
            println!("║ Channel:  {:<34} ║", config.channel);
            println!("║ Arch:     {:<34} ║", config.arch);
            println!("║ DHT:      {:<34} ║", if no_dht { "disabled" } else { "enabled" });
            println!("║ mDNS:     {:<34} ║", if no_mdns { "disabled" } else { "enabled" });
            println!("╚══════════════════════════════════════════════╝");
            println!();

            // Create and run server
            let server = ProviderServer::new(config);
            server.run().await?;

            Ok(())
        }
        Commands::Provider { command } => {
            match command {
                ProviderCommands::Status { json, addr } => {
                    // Query provider status endpoint
                    let status_url = format!("{}/status", addr);
                    let client = reqwest::Client::new();

                    match client.get(&status_url).send().await {
                        Ok(response) => {
                            if response.status().is_success() {
                                let status: serde_json::Value = response.json().await?;

                                if json {
                                    println!("{}", serde_json::to_string_pretty(&status)?);
                                } else {
                                    // Human-friendly output
                                    println!("Provider Status:");
                                    println!("  Name:     {}", status["name"].as_str().unwrap_or("unknown"));
                                    println!("  Version:  {}", status["version"].as_str().unwrap_or("unknown"));
                                    println!("  Channel:  {}", status["channel"].as_str().unwrap_or("unknown"));
                                    println!("  Arch:     {}", status["arch"].as_str().unwrap_or("unknown"));
                                    println!("  Uptime:   {}s", status["uptime_seconds"].as_u64().unwrap_or(0));
                                    println!();
                                    println!("Health:");
                                    println!("  Status:             {}", status["health"]["status"].as_str().unwrap_or("unknown"));
                                    println!("  Artifacts readable: {}", status["health"]["artifacts_readable"].as_bool().unwrap_or(false));
                                    println!("  Disk space ok:      {}", status["health"]["disk_space_ok"].as_bool().unwrap_or(false));
                                    println!();
                                    println!("Metrics:");
                                    println!("  Requests total:     {}", status["metrics"]["requests_total"].as_u64().unwrap_or(0));
                                    println!("  Bytes served total: {}", status["metrics"]["bytes_served_total"].as_u64().unwrap_or(0));
                                }
                            } else {
                                eprintln!("Error: Provider returned status {}", response.status());
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            eprintln!("Error connecting to provider at {}: {}", addr, e);
                            eprintln!("Is the provider running? Try: plasmd serve");
                            std::process::exit(1);
                        }
                    }

                    Ok(())
                }
                ProviderCommands::List { addr } => {
                    // Query provider manifest endpoint
                    let manifest_url = format!("{}/manifest.json", addr);
                    let client = reqwest::Client::new();

                    match client.get(&manifest_url).send().await {
                        Ok(response) => {
                            if response.status().is_success() {
                                let manifest: serde_json::Value = response.json().await?;

                                println!("Available Artifacts:");
                                println!("  Channel: {}", manifest["channel"].as_str().unwrap_or("unknown"));
                                println!("  Arch:    {}", manifest["arch"].as_str().unwrap_or("unknown"));
                                println!();

                                if let Some(artifacts) = manifest["artifacts"].as_object() {
                                    // Manifest uses object format: {"kernel": {...}, "initramfs": {...}}
                                    for (name, artifact) in artifacts {
                                        let size = artifact["size_bytes"].as_u64().unwrap_or(0);
                                        let hash = artifact["hash"].as_str().unwrap_or("unknown");
                                        let url = artifact["download_url"].as_str().unwrap_or("");

                                        println!("  {} ({} bytes)", name, size);
                                        println!("    Hash: {}", hash);
                                        if !url.is_empty() {
                                            println!("    URL:  {}", url);
                                        }
                                        println!();
                                    }

                                    println!("Total: {} artifacts", artifacts.len());
                                } else {
                                    println!("  No artifacts found");
                                }
                            } else {
                                eprintln!("Error: Provider returned status {}", response.status());
                                std::process::exit(1);
                            }
                        }
                        Err(e) => {
                            eprintln!("Error connecting to provider at {}: {}", addr, e);
                            eprintln!("Is the provider running? Try: plasmd serve");
                            std::process::exit(1);
                        }
                    }

                    Ok(())
                }
            }
        }
        Commands::Version => {
            println!("plasmd version {}", env!("CARGO_PKG_VERSION"));
            println!("Phase Open MVP - Local WASM Execution");
            Ok(())
        }
    }
}
