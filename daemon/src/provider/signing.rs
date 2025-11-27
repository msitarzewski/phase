//! Manifest signing and verification using Ed25519

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::{Signature as Ed25519Sig, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::manifest::{BootManifest, Signature};

/// Compute SHA256 hash of a file, returns "sha256:hexdigest"
pub fn compute_file_hash(path: &Path) -> Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(path)
        .with_context(|| format!("Failed to open file: {:?}", path))?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let hash = hasher.finalize();
    Ok(format!("sha256:{}", hex::encode(hash)))
}

/// Compute SHA256 hash of manifest content (excluding signatures)
pub fn compute_manifest_hash(manifest: &BootManifest) -> Result<String> {
    // Create a copy without signatures for hashing
    let mut manifest_for_hash = manifest.clone();
    manifest_for_hash.signatures.clear();

    let json = serde_json::to_string(&manifest_for_hash)
        .context("Failed to serialize manifest for hashing")?;

    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let hash = hasher.finalize();

    Ok(format!("sha256:{}", hex::encode(hash)))
}

/// Get key ID from signing key (hex-encoded public key)
pub fn key_id(signing_key: &SigningKey) -> String {
    hex::encode(signing_key.verifying_key().as_bytes())
}

/// Sign manifest with Ed25519 signing key
pub fn sign_manifest(manifest: &mut BootManifest, signing_key: &SigningKey) -> Result<()> {
    // Compute hash of manifest without signatures
    let hash = compute_manifest_hash(manifest)?;
    let hash_bytes = hex::decode(hash.strip_prefix("sha256:").unwrap_or(&hash))
        .context("Invalid hash format")?;

    // Sign the hash
    let signature = signing_key.sign(&hash_bytes);

    // Create signature entry
    let sig = Signature {
        algorithm: "ed25519".to_string(),
        key_id: key_id(signing_key),
        signature: hex::encode(signature.to_bytes()),
        signed_at: chrono::Utc::now().to_rfc3339(),
    };

    manifest.signatures.push(sig);

    Ok(())
}

/// Verify manifest signature with public key
pub fn verify_manifest_signature(
    manifest: &BootManifest,
    public_key: &VerifyingKey,
) -> Result<bool> {
    let key_id_hex = hex::encode(public_key.as_bytes());

    // Find signature matching this key
    let sig = manifest
        .signatures
        .iter()
        .find(|s| s.key_id == key_id_hex)
        .ok_or_else(|| anyhow!("No signature found for key {}", key_id_hex))?;

    // Compute manifest hash
    let hash = compute_manifest_hash(manifest)?;
    let hash_bytes = hex::decode(hash.strip_prefix("sha256:").unwrap_or(&hash))
        .context("Invalid hash format")?;

    // Decode and verify signature
    let sig_bytes = hex::decode(&sig.signature)
        .context("Invalid signature hex encoding")?;

    let signature = Ed25519Sig::from_slice(&sig_bytes)
        .map_err(|e| anyhow!("Invalid signature format: {}", e))?;

    Ok(public_key.verify(&hash_bytes, &signature).is_ok())
}

/// Generate a new random signing key
pub fn generate_signing_key() -> SigningKey {
    use rand::RngCore;
    use rand::rngs::OsRng;

    // Generate 32 random bytes for the secret key
    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut secret_bytes);

    // Create signing key from the random bytes
    SigningKey::from_bytes(&secret_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::manifest::{ArtifactInfo, ManifestBuilder};
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_compute_file_hash() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.bin");
        fs::write(&file_path, b"hello world").unwrap();

        let hash = compute_file_hash(&file_path).unwrap();
        assert_eq!(hash, "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }

    #[test]
    fn test_key_id() {
        let signing_key = generate_signing_key();
        let id = key_id(&signing_key);
        assert_eq!(id.len(), 64); // 32 bytes = 64 hex chars
    }

    #[test]
    fn test_sign_and_verify_manifest() {
        let signing_key = generate_signing_key();
        let verifying_key = signing_key.verifying_key();

        let kernel_info = ArtifactInfo {
            filename: "kernel".to_string(),
            size_bytes: 1024,
            hash: "sha256:abc123".to_string(),
            download_url: Some("/stable/arm64/kernel".to_string()),
        };

        let mut manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), kernel_info)
            .build()
            .unwrap();

        // Sign
        sign_manifest(&mut manifest, &signing_key).unwrap();
        assert_eq!(manifest.signatures.len(), 1);
        assert_eq!(manifest.signatures[0].algorithm, "ed25519");

        // Verify
        let verified = verify_manifest_signature(&manifest, &verifying_key).unwrap();
        assert!(verified);
    }

    #[test]
    fn test_verify_tampered_manifest() {
        let signing_key = generate_signing_key();
        let verifying_key = signing_key.verifying_key();

        let kernel_info = ArtifactInfo {
            filename: "kernel".to_string(),
            size_bytes: 1024,
            hash: "sha256:abc123".to_string(),
            download_url: None,
        };

        let mut manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), kernel_info)
            .build()
            .unwrap();

        sign_manifest(&mut manifest, &signing_key).unwrap();

        // Tamper with manifest
        manifest.version = "0.2.0".to_string();

        // Verification should fail
        let verified = verify_manifest_signature(&manifest, &verifying_key).unwrap();
        assert!(!verified);
    }

    #[test]
    fn test_verify_wrong_key() {
        let signing_key = generate_signing_key();
        let wrong_key = generate_signing_key();

        let kernel_info = ArtifactInfo {
            filename: "kernel".to_string(),
            size_bytes: 1024,
            hash: "sha256:abc123".to_string(),
            download_url: None,
        };

        let mut manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), kernel_info)
            .build()
            .unwrap();

        sign_manifest(&mut manifest, &signing_key).unwrap();

        // Verification with wrong key should fail (no matching signature)
        let result = verify_manifest_signature(&manifest, &wrong_key.verifying_key());
        assert!(result.is_err());
    }

    #[test]
    fn test_manifest_hash_deterministic() {
        let kernel_info = ArtifactInfo {
            filename: "kernel".to_string(),
            size_bytes: 1024,
            hash: "sha256:abc123".to_string(),
            download_url: None,
        };

        let manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), kernel_info)
            .build()
            .unwrap();

        let hash1 = compute_manifest_hash(&manifest).unwrap();
        let hash2 = compute_manifest_hash(&manifest).unwrap();
        assert_eq!(hash1, hash2);
    }
}
