//! Phase Fetch - Boot artifact downloader with SHA256 verification
//!
//! Downloads boot artifacts (kernel, initramfs, rootfs) from verified manifests.
//! Used in initramfs to fetch artifacts after manifest verification.
//!
//! Usage:
//!   phase-fetch --manifest manifest.json --output /boot --artifact all
//!   phase-fetch --manifest manifest.json --output /boot --artifact kernel --retry 5

use std::fs::{self, File};
use std::io::{Write, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;
use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use hex;
use anyhow::{anyhow, Context, Result};
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;

/// Phase Fetch - Boot artifact downloader
#[derive(Parser, Debug)]
#[command(name = "phase-fetch")]
#[command(about = "Download and verify Phase Boot artifacts")]
#[command(version)]
struct Args {
    /// Path to verified manifest JSON file
    #[arg(short, long)]
    manifest: PathBuf,

    /// Output directory for artifacts
    #[arg(short, long)]
    output: PathBuf,

    /// Specific artifact to fetch (kernel, initramfs, rootfs, or all)
    #[arg(short, long, default_value = "all")]
    artifact: String,

    /// Retry count per artifact
    #[arg(short, long, default_value = "3")]
    retry: u32,

    /// Per-download timeout in seconds
    #[arg(short, long, default_value = "300")]
    timeout: u64,

    /// Quiet mode (minimal output)
    #[arg(short, long)]
    quiet: bool,
}

/// Boot manifest structure (matching phase-verify)
#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    version: String,
    manifest_version: u64,
    channel: String,
    arch: String,
    artifacts: Artifacts,
}

#[derive(Debug, Deserialize, Serialize)]
struct Artifacts {
    kernel: Artifact,
    initramfs: Artifact,
    #[serde(default)]
    rootfs: Option<Artifact>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Artifact {
    hash: String,
    size: u64,
    urls: Vec<String>,
}

/// Download result
#[derive(Debug)]
struct DownloadResult {
    artifact_name: String,
    size: u64,
    hash: String,
    output_path: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    if !args.quiet {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .with_target(false)
            .compact()
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    // Load manifest
    let manifest_content = fs::read_to_string(&args.manifest)
        .context("Failed to read manifest file")?;

    let manifest: Manifest = serde_json::from_str(&manifest_content)
        .context("Failed to parse manifest JSON")?;

    if !args.quiet {
        info!("Fetching artifacts from manifest v{} ({}/{})",
              manifest.manifest_version, manifest.channel, manifest.arch);
    }

    // Create output directory
    fs::create_dir_all(&args.output)
        .context("Failed to create output directory")?;

    // Determine which artifacts to fetch
    let artifacts_to_fetch = match args.artifact.to_lowercase().as_str() {
        "all" => vec!["kernel", "initramfs", "rootfs"],
        "kernel" => vec!["kernel"],
        "initramfs" => vec!["initramfs"],
        "rootfs" => vec!["rootfs"],
        other => return Err(anyhow!("Unknown artifact: {}. Use: kernel, initramfs, rootfs, or all", other)),
    };

    // Fetch each artifact
    let mut results = Vec::new();
    let mut failed = false;

    for artifact_name in artifacts_to_fetch {
        let artifact = match artifact_name {
            "kernel" => Some(&manifest.artifacts.kernel),
            "initramfs" => Some(&manifest.artifacts.initramfs),
            "rootfs" => manifest.artifacts.rootfs.as_ref(),
            _ => None,
        };

        let Some(artifact) = artifact else {
            if !args.quiet {
                warn!("Artifact {} not present in manifest, skipping", artifact_name);
            }
            continue;
        };

        if !args.quiet {
            info!("Fetching {} ({} bytes, hash: {}...)",
                  artifact_name, artifact.size, &artifact.hash[..16]);
        }

        match fetch_artifact(
            artifact_name,
            artifact,
            &args.output,
            args.retry,
            args.timeout,
            args.quiet,
        ) {
            Ok(result) => {
                if !args.quiet {
                    info!("Successfully fetched {}", artifact_name);
                }
                results.push(result);
            }
            Err(e) => {
                error!("Failed to fetch {}: {}", artifact_name, e);
                failed = true;
            }
        }
    }

    if failed {
        return Err(anyhow!("One or more artifacts failed to download"));
    }

    // Summary
    if !args.quiet {
        info!("All artifacts fetched successfully:");
        for result in &results {
            info!("  {} -> {} ({} bytes)",
                  result.artifact_name,
                  result.output_path.display(),
                  result.size);
        }
    }

    Ok(())
}

/// Fetch a single artifact with retry logic
fn fetch_artifact(
    name: &str,
    artifact: &Artifact,
    output_dir: &Path,
    retry_count: u32,
    timeout_secs: u64,
    quiet: bool,
) -> Result<DownloadResult> {
    let output_path = output_dir.join(name);
    let hash_path = output_dir.join(format!("{}.sha256", name));

    // Try each URL with retry logic
    let mut last_error = None;

    for (url_idx, url) in artifact.urls.iter().enumerate() {
        for attempt in 0..retry_count {
            if !quiet && attempt > 0 {
                info!("  Retry {}/{} for URL {}", attempt + 1, retry_count, url_idx + 1);
            }

            match download_and_verify(url, &artifact.hash, artifact.size, &output_path, timeout_secs, quiet) {
                Ok(_) => {
                    // Write hash file
                    fs::write(&hash_path, &artifact.hash)
                        .context("Failed to write .sha256 file")?;

                    return Ok(DownloadResult {
                        artifact_name: name.to_string(),
                        size: artifact.size,
                        hash: artifact.hash.clone(),
                        output_path,
                    });
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < retry_count - 1 {
                        // Exponential backoff: 2^attempt seconds
                        let backoff_secs = 2_u64.pow(attempt);
                        if !quiet {
                            warn!("  Download failed, retrying in {} seconds...", backoff_secs);
                        }
                        std::thread::sleep(Duration::from_secs(backoff_secs));
                    }
                }
            }
        }

        // If we get here, all retries for this URL failed
        if !quiet && url_idx < artifact.urls.len() - 1 {
            warn!("  All retries failed for URL {}, trying next URL", url_idx + 1);
        }
    }

    // All URLs exhausted
    Err(last_error.unwrap_or_else(|| anyhow!("All download attempts failed")))
}

/// Download file and verify hash
fn download_and_verify(
    url: &str,
    expected_hash: &str,
    expected_size: u64,
    output_path: &Path,
    timeout_secs: u64,
    quiet: bool,
) -> Result<()> {
    // Create HTTP client with timeout
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("Failed to create HTTP client")?;

    // Download to temporary file
    let temp_path = output_path.with_extension("tmp");

    if !quiet {
        info!("  Downloading from {}...", url);
    }

    let mut response = client
        .get(url)
        .send()
        .context("HTTP request failed")?;

    if !response.status().is_success() {
        return Err(anyhow!("HTTP error: {}", response.status()));
    }

    // Check Content-Length if available
    if let Some(content_length) = response.content_length() {
        if content_length != expected_size {
            return Err(anyhow!(
                "Size mismatch: expected {} bytes, server reports {} bytes",
                expected_size,
                content_length
            ));
        }
    }

    // Download with progress indication
    let mut file = File::create(&temp_path)
        .context("Failed to create temporary file")?;
    let mut hasher = Sha256::new();
    let mut total_bytes = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = response.read(&mut buffer)
            .context("Failed to read response")?;

        if bytes_read == 0 {
            break;
        }

        file.write_all(&buffer[..bytes_read])
            .context("Failed to write to file")?;
        hasher.update(&buffer[..bytes_read]);
        total_bytes += bytes_read as u64;

        // Progress indicator (every 10 MB)
        if !quiet && total_bytes % (10 * 1024 * 1024) == 0 {
            info!("  Downloaded {} MB...", total_bytes / (1024 * 1024));
        }
    }

    file.flush()
        .context("Failed to flush file")?;
    drop(file);

    // Verify size
    if total_bytes != expected_size {
        let _ = fs::remove_file(&temp_path);
        return Err(anyhow!(
            "Size mismatch: expected {} bytes, downloaded {} bytes",
            expected_size,
            total_bytes
        ));
    }

    // Verify hash
    let computed_hash = hex::encode(hasher.finalize());
    if computed_hash != expected_hash {
        let _ = fs::remove_file(&temp_path);
        return Err(anyhow!(
            "Hash mismatch: expected {}, computed {}",
            expected_hash,
            computed_hash
        ));
    }

    if !quiet {
        info!("  Hash verified: {}", &computed_hash[..16]);
    }

    // Move to final location
    fs::rename(&temp_path, output_path)
        .context("Failed to move file to final location")?;

    Ok(())
}
