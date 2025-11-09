use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber;

mod config;
mod wasm;
mod network;

use wasm::runtime::WasmRuntime;

#[derive(Parser)]
#[command(name = "plasmd")]
#[command(about = "Phase local WASM execution daemon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon
    Start {
        /// Configuration file path
        #[arg(short, long, default_value = "/etc/plasm/config.json")]
        config: String,
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
        Commands::Start { config } => {
            info!("Starting plasmd with config: {}", config);

            // Create discovery configuration
            let disc_config = network::DiscoveryConfig::default();

            // Create discovery service
            let mut discovery = network::Discovery::new(disc_config)?;

            // Start listening
            discovery.listen("/ip4/0.0.0.0/tcp/0")?;

            // Bootstrap DHT
            discovery.bootstrap()?;

            // Advertise this node's capabilities
            discovery.advertise_capabilities()?;

            info!("Phase daemon started. Peer ID: {}", discovery.local_peer_id());
            info!("Capabilities: {:?}", discovery.capabilities());

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
            let runtime = wasm::runtime::Wasm3Runtime::new();
            let result = runtime.execute(&wasm_bytes, &args_refs).await?;

            // In quiet mode, suppress logs - WASM stdout is already inherited
            // In normal mode, show the placeholder message
            if !quiet {
                println!("{}", result.stdout);
            }

            std::process::exit(result.exit_code as i32);
        }
        Commands::ExecuteJob { wasm_file, args } => {
            use network::{JobRequest, JobRequirements};
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
            let handler = network::ExecutionHandler::new(signing_key);

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
