use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber;

mod config;
mod wasm;

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
            // TODO: Implement daemon mode (Milestone 2+)
            info!("Daemon mode not yet implemented (coming in Milestone 2)");
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
            let result = runtime.execute(&wasm_bytes, &args_refs)?;

            // In quiet mode, suppress logs - WASM stdout is already inherited
            // In normal mode, show the placeholder message
            if !quiet {
                println!("{}", result.stdout);
            }

            std::process::exit(result.exit_code as i32);
        }
        Commands::Version => {
            println!("plasmd version {}", env!("CARGO_PKG_VERSION"));
            println!("Phase Open MVP - Local WASM Execution");
            Ok(())
        }
    }
}
