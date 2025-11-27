//! Boot manifest types and validation
//!
//! Defines the Phase Boot Manifest format for distributing kernel, initramfs,
//! and other boot artifacts over libp2p with Ed25519 signatures.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Manifest validation errors
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid hash format: {0}")]
    InvalidHash(String),

    #[error("Invalid artifact: {0}")]
    InvalidArtifact(String),

    #[error("Missing required artifact: {0}")]
    MissingArtifact(String),

    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    #[error("Manifest expired at {0}")]
    Expired(String),
}

/// Boot manifest structure
///
/// This manifest describes all artifacts needed to boot Phase OS,
/// including cryptographic signatures for verification.
///
/// # Example
///
/// ```no_run
/// use plasm::provider::manifest::{BootManifest, ArtifactInfo, ManifestBuilder};
/// use std::collections::HashMap;
///
/// let manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
///     .version("0.1.0".to_string())
///     .artifact("kernel".to_string(), ArtifactInfo {
///         filename: "Image".to_string(),
///         size_bytes: 12345678,
///         hash: "sha256:abc123...".to_string(),
///         download_url: Some("kernel/Image".to_string()),
///     })
///     .build()
///     .expect("Failed to build manifest");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootManifest {
    /// Manifest schema version (always 1 for this format)
    pub manifest_version: u32,

    /// Software version (e.g., "0.1.0")
    pub version: String,

    /// Release channel: "stable", "testing", "dev"
    pub channel: String,

    /// Target architecture: "arm64", "x86_64"
    pub arch: String,

    /// ISO 8601 timestamp when manifest was created
    pub created_at: String,

    /// ISO 8601 timestamp when manifest expires
    pub expires_at: String,

    /// Map of artifact name to artifact information
    /// Required artifacts: "kernel", "initramfs"
    /// Optional: "rootfs", "dtb-*" entries
    pub artifacts: HashMap<String, ArtifactInfo>,

    /// Cryptographic signatures over the manifest
    pub signatures: Vec<Signature>,

    /// Optional provider information for libp2p discovery
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderInfo>,
}

impl BootManifest {
    /// Validate the manifest structure and contents
    ///
    /// Checks:
    /// - Required fields are non-empty
    /// - At least "kernel" artifact exists
    /// - All artifact hashes are valid format
    /// - Timestamps are valid ISO 8601
    pub fn validate(&self) -> Result<(), ManifestError> {
        // Check required fields
        if self.version.is_empty() {
            return Err(ManifestError::MissingField("version".to_string()));
        }
        if self.channel.is_empty() {
            return Err(ManifestError::MissingField("channel".to_string()));
        }
        if self.arch.is_empty() {
            return Err(ManifestError::MissingField("arch".to_string()));
        }
        if self.created_at.is_empty() {
            return Err(ManifestError::MissingField("created_at".to_string()));
        }
        if self.expires_at.is_empty() {
            return Err(ManifestError::MissingField("expires_at".to_string()));
        }

        // Validate manifest version
        if self.manifest_version != 1 {
            return Err(ManifestError::MissingField(
                format!("manifest_version must be 1, got {}", self.manifest_version)
            ));
        }

        // Check required artifacts
        if !self.artifacts.contains_key("kernel") {
            return Err(ManifestError::MissingArtifact("kernel".to_string()));
        }

        // Validate each artifact
        for (name, artifact) in &self.artifacts {
            artifact.validate()
                .map_err(|e| ManifestError::InvalidArtifact(
                    format!("{}: {}", name, e)
                ))?;
        }

        // Validate timestamps (basic ISO 8601 check)
        if !is_valid_iso8601(&self.created_at) {
            return Err(ManifestError::InvalidTimestamp(
                format!("created_at: {}", self.created_at)
            ));
        }
        if !is_valid_iso8601(&self.expires_at) {
            return Err(ManifestError::InvalidTimestamp(
                format!("expires_at: {}", self.expires_at)
            ));
        }

        Ok(())
    }

    /// Check if the manifest has expired
    pub fn is_expired(&self) -> bool {
        // Simple string comparison works for ISO 8601
        let now = chrono::Utc::now().to_rfc3339();
        self.expires_at < now
    }
}

/// Information about a single boot artifact
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactInfo {
    /// Filename of the artifact
    pub filename: String,

    /// Size in bytes
    pub size_bytes: u64,

    /// Cryptographic hash in format "algorithm:hexdigest"
    /// Example: "sha256:abc123..."
    pub hash: String,

    /// Optional relative download URL path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

impl ArtifactInfo {
    /// Validate artifact information
    ///
    /// Checks:
    /// - Filename is non-empty
    /// - Size is greater than zero
    /// - Hash format is "algorithm:hexdigest"
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.filename.is_empty() {
            return Err(ManifestError::InvalidArtifact(
                "filename cannot be empty".to_string()
            ));
        }

        if self.size_bytes == 0 {
            return Err(ManifestError::InvalidArtifact(
                "size_bytes must be greater than zero".to_string()
            ));
        }

        // Validate hash format: "algorithm:hexdigest"
        if !self.hash.contains(':') {
            return Err(ManifestError::InvalidHash(
                format!("expected 'algorithm:hexdigest', got '{}'", self.hash)
            ));
        }

        let parts: Vec<&str> = self.hash.split(':').collect();
        if parts.len() != 2 {
            return Err(ManifestError::InvalidHash(
                format!("expected exactly one ':', got '{}'", self.hash)
            ));
        }

        let algorithm = parts[0];
        let digest = parts[1];

        if algorithm.is_empty() || digest.is_empty() {
            return Err(ManifestError::InvalidHash(
                format!("algorithm and digest cannot be empty: '{}'", self.hash)
            ));
        }

        // Validate hex digest
        if !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ManifestError::InvalidHash(
                format!("digest must be hexadecimal: '{}'", digest)
            ));
        }

        Ok(())
    }
}

/// Cryptographic signature over the manifest
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Signature {
    /// Signature algorithm (e.g., "ed25519")
    pub algorithm: String,

    /// Hex-encoded public key identifier
    pub key_id: String,

    /// Hex-encoded signature
    pub signature: String,

    /// ISO 8601 timestamp when signature was created
    pub signed_at: String,
}

/// Provider information for libp2p discovery
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderInfo {
    /// libp2p peer ID
    pub peer_id: String,

    /// Multiaddresses where this provider can be reached
    pub addresses: Vec<String>,
}

/// Builder for creating BootManifest instances
///
/// # Example
///
/// ```no_run
/// use plasm::provider::manifest::{ManifestBuilder, ArtifactInfo};
///
/// let manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
///     .version("0.1.0".to_string())
///     .artifact("kernel".to_string(), ArtifactInfo {
///         filename: "Image".to_string(),
///         size_bytes: 12345678,
///         hash: "sha256:abc123...".to_string(),
///         download_url: Some("kernel/Image".to_string()),
///     })
///     .build()
///     .expect("Valid manifest");
/// ```
#[derive(Debug)]
pub struct ManifestBuilder {
    channel: String,
    arch: String,
    version: Option<String>,
    created_at: Option<String>,
    expires_at: Option<String>,
    artifacts: HashMap<String, ArtifactInfo>,
    signatures: Vec<Signature>,
    provider: Option<ProviderInfo>,
}

impl ManifestBuilder {
    /// Create a new manifest builder
    ///
    /// # Arguments
    ///
    /// * `channel` - Release channel ("stable", "testing", "dev")
    /// * `arch` - Target architecture ("arm64", "x86_64")
    pub fn new(channel: String, arch: String) -> Self {
        Self {
            channel,
            arch,
            version: None,
            created_at: None,
            expires_at: None,
            artifacts: HashMap::new(),
            signatures: Vec::new(),
            provider: None,
        }
    }

    /// Set the software version
    pub fn version(mut self, version: String) -> Self {
        self.version = Some(version);
        self
    }

    /// Set the creation timestamp (ISO 8601)
    pub fn created_at(mut self, created_at: String) -> Self {
        self.created_at = Some(created_at);
        self
    }

    /// Set the expiration timestamp (ISO 8601)
    pub fn expires_at(mut self, expires_at: String) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Add an artifact to the manifest
    pub fn artifact(mut self, name: String, info: ArtifactInfo) -> Self {
        self.artifacts.insert(name, info);
        self
    }

    /// Add a signature to the manifest
    pub fn signature(mut self, sig: Signature) -> Self {
        self.signatures.push(sig);
        self
    }

    /// Set the provider information
    pub fn provider(mut self, provider: ProviderInfo) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Build the manifest
    ///
    /// Returns an error if required fields are missing or validation fails.
    pub fn build(self) -> Result<BootManifest, ManifestError> {
        let version = self.version
            .ok_or_else(|| ManifestError::MissingField("version".to_string()))?;

        // Default timestamps if not provided
        let created_at = self.created_at.unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339()
        });

        let expires_at = self.expires_at.unwrap_or_else(|| {
            // Default: 30 days from now
            let expiry = chrono::Utc::now() + chrono::Duration::days(30);
            expiry.to_rfc3339()
        });

        let manifest = BootManifest {
            manifest_version: 1,
            version,
            channel: self.channel,
            arch: self.arch,
            created_at,
            expires_at,
            artifacts: self.artifacts,
            signatures: self.signatures,
            provider: self.provider,
        };

        // Validate before returning
        manifest.validate()?;

        Ok(manifest)
    }
}

/// Basic ISO 8601 timestamp validation
fn is_valid_iso8601(s: &str) -> bool {
    // Very basic check - just ensure it looks like an ISO timestamp
    // Real validation would use chrono parsing
    if s.is_empty() {
        return false;
    }

    // Check for basic ISO 8601 format patterns
    // YYYY-MM-DDTHH:MM:SS or variations
    s.contains('-') && (s.contains('T') || s.contains('t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_artifact() -> ArtifactInfo {
        ArtifactInfo {
            filename: "Image".to_string(),
            size_bytes: 12345678,
            hash: "sha256:abcdef0123456789".to_string(),
            download_url: Some("kernel/Image".to_string()),
        }
    }

    fn sample_manifest() -> BootManifest {
        let mut artifacts = HashMap::new();
        artifacts.insert("kernel".to_string(), sample_artifact());

        BootManifest {
            manifest_version: 1,
            version: "0.1.0".to_string(),
            channel: "stable".to_string(),
            arch: "arm64".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            expires_at: "2025-12-31T23:59:59Z".to_string(),
            artifacts,
            signatures: vec![],
            provider: None,
        }
    }

    #[test]
    fn test_artifact_validation_valid() {
        let artifact = sample_artifact();
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn test_artifact_validation_empty_filename() {
        let mut artifact = sample_artifact();
        artifact.filename = "".to_string();
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn test_artifact_validation_zero_size() {
        let mut artifact = sample_artifact();
        artifact.size_bytes = 0;
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn test_artifact_validation_invalid_hash_no_colon() {
        let mut artifact = sample_artifact();
        artifact.hash = "sha256abcdef".to_string();
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn test_artifact_validation_invalid_hash_non_hex() {
        let mut artifact = sample_artifact();
        artifact.hash = "sha256:xyz123".to_string();
        assert!(artifact.validate().is_err());
    }

    #[test]
    fn test_manifest_validation_valid() {
        let manifest = sample_manifest();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_manifest_validation_missing_kernel() {
        let mut manifest = sample_manifest();
        manifest.artifacts.remove("kernel");
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::MissingArtifact(_))
        ));
    }

    #[test]
    fn test_manifest_validation_invalid_version() {
        let mut manifest = sample_manifest();
        manifest.manifest_version = 2;
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let manifest = sample_manifest();

        // Serialize to JSON
        let json = serde_json::to_string(&manifest).expect("Failed to serialize");

        // Deserialize back
        let deserialized: BootManifest = serde_json::from_str(&json)
            .expect("Failed to deserialize");

        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn test_builder_minimal() {
        let manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), sample_artifact())
            .build()
            .expect("Failed to build manifest");

        assert_eq!(manifest.manifest_version, 1);
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.channel, "stable");
        assert_eq!(manifest.arch, "arm64");
        assert!(manifest.artifacts.contains_key("kernel"));
    }

    #[test]
    fn test_builder_missing_version() {
        let result = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .artifact("kernel".to_string(), sample_artifact())
            .build();

        assert!(matches!(result, Err(ManifestError::MissingField(_))));
    }

    #[test]
    fn test_builder_missing_kernel() {
        let result = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .build();

        assert!(matches!(result, Err(ManifestError::MissingArtifact(_))));
    }

    #[test]
    fn test_builder_with_provider() {
        let provider = ProviderInfo {
            peer_id: "12D3KooTest".to_string(),
            addresses: vec!["/ip4/127.0.0.1/tcp/4001".to_string()],
        };

        let manifest = ManifestBuilder::new("stable".to_string(), "arm64".to_string())
            .version("0.1.0".to_string())
            .artifact("kernel".to_string(), sample_artifact())
            .provider(provider.clone())
            .build()
            .expect("Failed to build manifest");

        assert_eq!(manifest.provider, Some(provider));
    }

    #[test]
    fn test_iso8601_validation() {
        assert!(is_valid_iso8601("2025-01-01T00:00:00Z"));
        assert!(is_valid_iso8601("2025-01-01T00:00:00+00:00"));
        assert!(!is_valid_iso8601(""));
        assert!(!is_valid_iso8601("2025-01-01"));
        assert!(!is_valid_iso8601("invalid"));
    }
}
