//! Manifest generation for boot artifacts
//!
//! This module generates BootManifest instances from available artifacts
//! in the artifact store, with optional signing support.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::sync::Arc;

use super::artifacts::ArtifactStore;
use super::manifest::{ArtifactInfo, BootManifest, ManifestBuilder};
use super::signing::sign_manifest;

/// Generates boot manifests from available artifacts
pub struct ManifestGenerator {
    artifacts: Arc<ArtifactStore>,
    signing_key: Option<SigningKey>,
    default_version: String,
}

impl ManifestGenerator {
    /// Create a new manifest generator
    ///
    /// # Arguments
    ///
    /// * `artifacts` - Artifact store to read from
    /// * `signing_key` - Optional signing key for manifest signatures
    pub fn new(artifacts: Arc<ArtifactStore>, signing_key: Option<SigningKey>) -> Self {
        Self {
            artifacts,
            signing_key,
            default_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Set the default version string for generated manifests
    pub fn with_version(mut self, version: String) -> Self {
        self.default_version = version;
        self
    }

    /// Generate manifest for channel/arch from available artifacts
    ///
    /// This scans the artifact store and creates a manifest listing all
    /// available artifacts with their hashes and metadata.
    ///
    /// # Arguments
    ///
    /// * `channel` - Release channel (e.g., "stable", "testing", "dev")
    /// * `arch` - Target architecture (e.g., "arm64", "x86_64")
    pub fn generate(&self, channel: &str, arch: &str) -> Result<BootManifest> {
        // List all artifacts for this channel/arch
        let artifacts_meta = self
            .artifacts
            .list_artifacts(channel, arch)
            .with_context(|| format!("Failed to list artifacts for {}/{}", channel, arch))?;

        if artifacts_meta.is_empty() {
            anyhow::bail!("No artifacts found for channel {} arch {}", channel, arch);
        }

        // Convert artifact metadata to manifest format
        let mut builder = ManifestBuilder::new(channel.to_string(), arch.to_string())
            .version(self.default_version.clone());

        for meta in artifacts_meta {
            let artifact_info = ArtifactInfo {
                filename: meta.name.clone(),
                size_bytes: meta.size_bytes,
                hash: meta.hash,
                download_url: Some(format!("/{}/{}/{}", channel, arch, meta.name)),
            };

            // Use the filename as the artifact name (e.g., "kernel", "initramfs")
            let artifact_name = Self::normalize_artifact_name(&meta.name);
            builder = builder.artifact(artifact_name, artifact_info);
        }

        builder
            .build()
            .context("Failed to build manifest")
    }

    /// Generate and sign manifest
    ///
    /// This generates a manifest and signs it if a signing key is available.
    /// If no signing key is configured, returns an unsigned manifest.
    ///
    /// # Arguments
    ///
    /// * `channel` - Release channel
    /// * `arch` - Target architecture
    pub fn generate_signed(&self, channel: &str, arch: &str) -> Result<BootManifest> {
        let mut manifest = self.generate(channel, arch)?;

        if let Some(ref key) = self.signing_key {
            sign_manifest(&mut manifest, key)
                .context("Failed to sign manifest")?;
        }

        Ok(manifest)
    }

    /// Normalize artifact filename to canonical name
    ///
    /// Maps common artifact filenames to standard names:
    /// - "vmlinuz*", "bzImage" -> "kernel"
    /// - "initramfs*", "initrd*" -> "initramfs"
    /// - "rootfs*" -> "rootfs"
    /// - Others remain unchanged
    fn normalize_artifact_name(filename: &str) -> String {
        if filename.starts_with("vmlinuz") || filename.starts_with("bzImage") || filename == "kernel" {
            "kernel".to_string()
        } else if filename.starts_with("initramfs") || filename.starts_with("initrd") {
            "initramfs".to_string()
        } else if filename.starts_with("rootfs") {
            "rootfs".to_string()
        } else if filename.starts_with("dtb-") {
            // Keep DTB files with their specific names
            filename.to_string()
        } else {
            filename.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_artifacts() -> (TempDir, Arc<ArtifactStore>) {
        let temp = TempDir::new().unwrap();
        let artifact_dir = temp.path().join("stable").join("arm64");
        fs::create_dir_all(&artifact_dir).unwrap();

        // Create test artifacts
        fs::write(artifact_dir.join("kernel"), b"test kernel").unwrap();
        fs::write(artifact_dir.join("initramfs"), b"test initramfs").unwrap();

        let store = Arc::new(ArtifactStore::new(temp.path().to_path_buf()).unwrap());
        (temp, store)
    }

    #[test]
    fn test_generate_manifest() {
        let (_temp, store) = setup_test_artifacts();
        let generator = ManifestGenerator::new(store, None);

        let manifest = generator.generate("stable", "arm64").unwrap();

        assert_eq!(manifest.channel, "stable");
        assert_eq!(manifest.arch, "arm64");
        assert_eq!(manifest.manifest_version, 1);
        assert!(manifest.artifacts.contains_key("kernel"));
        assert!(manifest.artifacts.contains_key("initramfs"));
    }

    #[test]
    fn test_generate_manifest_no_artifacts() {
        let temp = TempDir::new().unwrap();
        let store = Arc::new(ArtifactStore::new(temp.path().to_path_buf()).unwrap());
        let generator = ManifestGenerator::new(store, None);

        let result = generator.generate("stable", "arm64");
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_signed_manifest() {
        use crate::provider::signing::generate_signing_key;

        let (_temp, store) = setup_test_artifacts();
        let signing_key = generate_signing_key();
        let generator = ManifestGenerator::new(store, Some(signing_key));

        let manifest = generator.generate_signed("stable", "arm64").unwrap();

        assert_eq!(manifest.signatures.len(), 1);
        assert_eq!(manifest.signatures[0].algorithm, "ed25519");
    }

    #[test]
    fn test_normalize_artifact_name() {
        assert_eq!(ManifestGenerator::normalize_artifact_name("vmlinuz"), "kernel");
        assert_eq!(ManifestGenerator::normalize_artifact_name("vmlinuz-arm64"), "kernel");
        assert_eq!(ManifestGenerator::normalize_artifact_name("bzImage"), "kernel");
        assert_eq!(ManifestGenerator::normalize_artifact_name("kernel"), "kernel");
        assert_eq!(ManifestGenerator::normalize_artifact_name("initramfs"), "initramfs");
        assert_eq!(ManifestGenerator::normalize_artifact_name("initramfs.img"), "initramfs");
        assert_eq!(ManifestGenerator::normalize_artifact_name("initrd"), "initramfs");
        assert_eq!(ManifestGenerator::normalize_artifact_name("rootfs.img"), "rootfs");
        assert_eq!(ManifestGenerator::normalize_artifact_name("dtb-rpi4"), "dtb-rpi4");
        assert_eq!(ManifestGenerator::normalize_artifact_name("custom-file"), "custom-file");
    }

    #[test]
    fn test_download_urls() {
        let (_temp, store) = setup_test_artifacts();
        let generator = ManifestGenerator::new(store, None);

        let manifest = generator.generate("stable", "arm64").unwrap();

        let kernel = manifest.artifacts.get("kernel").unwrap();
        assert_eq!(kernel.download_url, Some("/stable/arm64/kernel".to_string()));

        let initramfs = manifest.artifacts.get("initramfs").unwrap();
        assert_eq!(initramfs.download_url, Some("/stable/arm64/initramfs".to_string()));
    }

    #[test]
    fn test_with_version() {
        let (_temp, store) = setup_test_artifacts();
        let generator = ManifestGenerator::new(store, None)
            .with_version("1.2.3".to_string());

        let manifest = generator.generate("stable", "arm64").unwrap();
        assert_eq!(manifest.version, "1.2.3");
    }
}
