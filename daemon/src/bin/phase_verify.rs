//! Phase Verify - Boot manifest signature verification
//!
//! Verifies Ed25519 signatures on Phase Boot manifests.
//! Used in initramfs to validate manifests before fetching artifacts.
//!
//! Usage:
//!   phase-verify --manifest manifest.json
//!   phase-verify --manifest manifest.json --key targets.pub --check-version /cache/version

use std::fs;
use std::path::PathBuf;
use clap::Parser;
use serde::{Deserialize, Serialize};
use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use sha2::{Sha256, Digest};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;

/// Phase Verify - Boot manifest signature verification
#[derive(Parser, Debug)]
#[command(name = "phase-verify")]
#[command(about = "Verify Phase Boot manifest signatures")]
#[command(version)]
struct Args {
    /// Path to manifest JSON file
    #[arg(short, long)]
    manifest: PathBuf,

    /// Path to targets public key (optional, uses embedded key if not provided)
    #[arg(short, long)]
    key: Option<PathBuf>,

    /// Path to cached version file for rollback protection
    #[arg(long)]
    check_version: Option<PathBuf>,

    /// Update cached version after successful verification
    #[arg(long)]
    update_version: bool,

    /// Output format (text, json)
    #[arg(short, long, default_value = "text")]
    format: String,

    /// Quiet mode (exit code only)
    #[arg(short, long)]
    quiet: bool,
}

/// Boot manifest structure
#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    version: String,
    manifest_version: u64,
    channel: String,
    arch: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    artifacts: Artifacts,
    #[serde(default)]
    cmdline: Option<String>,
    signatures: Vec<ManifestSignature>,
    signed: SignedData,
}

#[derive(Debug, Deserialize, Serialize)]
struct Artifacts {
    kernel: Artifact,
    initramfs: Artifact,
    #[serde(default)]
    rootfs: Option<Artifact>,
    #[serde(default)]
    dtbs: Option<std::collections::HashMap<String, Artifact>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Artifact {
    hash: String,
    size: u64,
    urls: Vec<String>,
    #[serde(default)]
    ipfs: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestSignature {
    keyid: String,
    sig: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct SignedData {
    data: String,
}

/// Verification result
#[derive(Debug, Serialize)]
struct VerificationResult {
    status: String,
    manifest_version: u64,
    channel: String,
    arch: String,
    key_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Embedded root public key (placeholder - replace in production)
/// This is a development key - real deployments embed the actual root key
const EMBEDDED_ROOT_KEY: &[u8] = include_bytes!("../../keys/root.pub.placeholder");

fn main() -> anyhow::Result<()> {
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
        .map_err(|e| anyhow::anyhow!("Failed to read manifest: {}", e))?;

    let manifest: Manifest = serde_json::from_str(&manifest_content)
        .map_err(|e| anyhow::anyhow!("Failed to parse manifest: {}", e))?;

    if !args.quiet {
        info!("Verifying manifest v{} ({}/{})",
              manifest.manifest_version, manifest.channel, manifest.arch);
    }

    // Check rollback protection
    if let Some(version_path) = &args.check_version {
        if version_path.exists() {
            let cached_version: u64 = fs::read_to_string(version_path)?
                .trim()
                .parse()
                .unwrap_or(0);

            if manifest.manifest_version < cached_version {
                let result = VerificationResult {
                    status: "FAILED".to_string(),
                    manifest_version: manifest.manifest_version,
                    channel: manifest.channel.clone(),
                    arch: manifest.arch.clone(),
                    key_id: "N/A".to_string(),
                    error: Some(format!(
                        "Rollback detected: manifest v{} < cached v{}",
                        manifest.manifest_version, cached_version
                    )),
                };
                output_result(&args, &result)?;
                std::process::exit(1);
            }
        }
    }

    // Load verification key
    let key_bytes = if let Some(key_path) = &args.key {
        fs::read(key_path)
            .map_err(|e| anyhow::anyhow!("Failed to read key file: {}", e))?
    } else {
        // Use embedded key
        if EMBEDDED_ROOT_KEY.is_empty() || EMBEDDED_ROOT_KEY == b"PLACEHOLDER" {
            if !args.quiet {
                warn!("No embedded key and no --key provided");
            }
            let result = VerificationResult {
                status: "FAILED".to_string(),
                manifest_version: manifest.manifest_version,
                channel: manifest.channel.clone(),
                arch: manifest.arch.clone(),
                key_id: "N/A".to_string(),
                error: Some("No verification key available".to_string()),
            };
            output_result(&args, &result)?;
            std::process::exit(1);
        }
        EMBEDDED_ROOT_KEY.to_vec()
    };

    // Parse public key
    let verifying_key = parse_public_key(&key_bytes)?;

    // Verify at least one signature
    let mut verified = false;
    let mut verified_keyid = String::new();

    for sig in &manifest.signatures {
        match verify_signature(&manifest.signed.data, &sig.sig, &verifying_key) {
            Ok(true) => {
                verified = true;
                verified_keyid = sig.keyid.clone();
                if !args.quiet {
                    info!("Signature verified (keyid: {})", sig.keyid);
                }
                break;
            }
            Ok(false) => {
                if !args.quiet {
                    warn!("Signature invalid (keyid: {})", sig.keyid);
                }
            }
            Err(e) => {
                if !args.quiet {
                    warn!("Signature error (keyid: {}): {}", sig.keyid, e);
                }
            }
        }
    }

    if verified {
        // Update cached version if requested
        if args.update_version {
            if let Some(version_path) = &args.check_version {
                if let Some(parent) = version_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                fs::write(version_path, manifest.manifest_version.to_string())?;
                if !args.quiet {
                    info!("Updated cached version to {}", manifest.manifest_version);
                }
            }
        }

        let result = VerificationResult {
            status: "VERIFIED".to_string(),
            manifest_version: manifest.manifest_version,
            channel: manifest.channel,
            arch: manifest.arch,
            key_id: verified_keyid,
            error: None,
        };
        output_result(&args, &result)?;
        std::process::exit(0);
    } else {
        let result = VerificationResult {
            status: "FAILED".to_string(),
            manifest_version: manifest.manifest_version,
            channel: manifest.channel,
            arch: manifest.arch,
            key_id: "none".to_string(),
            error: Some("No valid signature found".to_string()),
        };
        output_result(&args, &result)?;
        std::process::exit(1);
    }
}

/// Parse public key from bytes (supports raw 32-byte or hex-encoded)
fn parse_public_key(bytes: &[u8]) -> anyhow::Result<VerifyingKey> {
    // Try as raw 32-byte key
    if bytes.len() == 32 {
        let key_bytes: [u8; 32] = bytes.try_into()?;
        return Ok(VerifyingKey::from_bytes(&key_bytes)?);
    }

    // Try as hex-encoded (64 chars)
    let hex_str = String::from_utf8_lossy(bytes);
    let hex_str = hex_str.trim();
    if hex_str.len() == 64 {
        let decoded = hex::decode(hex_str)?;
        let key_bytes: [u8; 32] = decoded.try_into()
            .map_err(|_| anyhow::anyhow!("Invalid key length"))?;
        return Ok(VerifyingKey::from_bytes(&key_bytes)?);
    }

    // Try as base64
    if let Ok(decoded) = BASE64.decode(hex_str.as_bytes()) {
        if decoded.len() == 32 {
            let key_bytes: [u8; 32] = decoded.try_into()
                .map_err(|_| anyhow::anyhow!("Invalid key length"))?;
            return Ok(VerifyingKey::from_bytes(&key_bytes)?);
        }
    }

    Err(anyhow::anyhow!("Could not parse public key (expected 32 bytes raw, 64 hex chars, or base64)"))
}

/// Verify Ed25519 signature
fn verify_signature(data_b64: &str, sig_b64: &str, key: &VerifyingKey) -> anyhow::Result<bool> {
    // Decode the signed data
    let data = BASE64.decode(data_b64)?;

    // Decode the signature
    let sig_bytes = BASE64.decode(sig_b64)?;
    if sig_bytes.len() != 64 {
        return Err(anyhow::anyhow!("Invalid signature length"));
    }

    let sig_array: [u8; 64] = sig_bytes.try_into()
        .map_err(|_| anyhow::anyhow!("Invalid signature length"))?;
    let signature = Signature::from_bytes(&sig_array);

    // Hash the data (pre-hash signing)
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hash = hasher.finalize();

    // Verify signature
    match key.verify(&hash, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Output verification result
fn output_result(args: &Args, result: &VerificationResult) -> anyhow::Result<()> {
    if args.quiet {
        return Ok(());
    }

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        if result.status == "VERIFIED" {
            println!("VERIFIED");
            println!("  Version: {}", result.manifest_version);
            println!("  Channel: {}", result.channel);
            println!("  Arch:    {}", result.arch);
            println!("  Key:     {}", result.key_id);
        } else {
            println!("FAILED");
            if let Some(error) = &result.error {
                println!("  Error: {}", error);
            }
        }
    }

    Ok(())
}
