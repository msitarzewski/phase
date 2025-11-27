# Task 1 — Manifest Schema

**Agent**: Security Agent
**Estimated**: 2 days

## 1.1 Define Rust types

- [ ] Create `daemon/src/provider/manifest.rs`:
  ```rust
  use serde::{Deserialize, Serialize};
  use std::collections::HashMap;

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct BootManifest {
      /// Manifest format version (currently "1")
      pub manifest_version: u32,

      /// Image version (e.g., "2025.11.26")
      pub version: String,

      /// Release channel: "stable", "testing"
      pub channel: String,

      /// Target architecture: "arm64", "x86_64"
      pub arch: String,

      /// ISO 8601 creation timestamp
      pub created_at: String,

      /// ISO 8601 expiration timestamp
      pub expires_at: String,

      /// Boot artifacts
      pub artifacts: ArtifactMap,

      /// Ed25519 signatures
      pub signatures: Vec<Signature>,

      /// Provider information
      #[serde(skip_serializing_if = "Option::is_none")]
      pub provider: Option<ProviderInfo>,
  }

  pub type ArtifactMap = HashMap<String, Artifact>;

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Artifact {
      /// SHA256 hash prefixed with "sha256:"
      pub hash: String,

      /// File size in bytes
      pub size: u64,

      /// URL path relative to provider (e.g., "/stable/arm64/kernel")
      pub path: String,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Signature {
      /// Key identifier: "ed25519:<public_key_hex>"
      pub keyid: String,

      /// Base64-encoded signature
      pub sig: String,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct ProviderInfo {
      /// libp2p peer ID
      pub peer_id: String,

      /// Human-readable provider name
      #[serde(skip_serializing_if = "Option::is_none")]
      pub name: Option<String>,
  }
  ```

**Dependencies**: None
**Output**: Manifest type definitions

---

## 1.2 Implement serialization helpers

- [ ] Add serialization methods:
  ```rust
  impl BootManifest {
      /// Serialize manifest to JSON (for signing)
      pub fn to_json(&self) -> Result<String, serde_json::Error> {
          serde_json::to_string_pretty(self)
      }

      /// Serialize manifest to bytes (for signing)
      pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
          serde_json::to_vec(self)
      }

      /// Parse manifest from JSON
      pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
          serde_json::from_str(json)
      }

      /// Get the canonical bytes to sign (manifest without signatures)
      pub fn signable_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
          let mut unsigned = self.clone();
          unsigned.signatures = Vec::new();
          serde_json::to_vec(&unsigned)
      }
  }
  ```

**Dependencies**: Task 1.1
**Output**: Serialization helpers

---

## 1.3 Add validation methods

- [ ] Implement manifest validation:
  ```rust
  impl BootManifest {
      /// Validate manifest structure
      pub fn validate(&self) -> Result<(), ManifestError> {
          // Check manifest version
          if self.manifest_version != 1 {
              return Err(ManifestError::UnsupportedVersion(self.manifest_version));
          }

          // Check required artifacts
          for required in &["kernel", "initramfs"] {
              if !self.artifacts.contains_key(*required) {
                  return Err(ManifestError::MissingArtifact(required.to_string()));
              }
          }

          // Validate artifact hashes
          for (name, artifact) in &self.artifacts {
              if !artifact.hash.starts_with("sha256:") {
                  return Err(ManifestError::InvalidHash(name.clone()));
              }
              if artifact.hash.len() != 71 {  // "sha256:" + 64 hex chars
                  return Err(ManifestError::InvalidHash(name.clone()));
              }
          }

          // Check expiration
          let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
              .map_err(|_| ManifestError::InvalidTimestamp)?;
          if expires < chrono::Utc::now() {
              return Err(ManifestError::Expired);
          }

          // Check signatures present
          if self.signatures.is_empty() {
              return Err(ManifestError::NoSignatures);
          }

          Ok(())
      }
  }

  #[derive(Debug, thiserror::Error)]
  pub enum ManifestError {
      #[error("Unsupported manifest version: {0}")]
      UnsupportedVersion(u32),

      #[error("Missing required artifact: {0}")]
      MissingArtifact(String),

      #[error("Invalid hash for artifact: {0}")]
      InvalidHash(String),

      #[error("Invalid timestamp format")]
      InvalidTimestamp,

      #[error("Manifest has expired")]
      Expired,

      #[error("No signatures present")]
      NoSignatures,
  }
  ```

**Dependencies**: Task 1.2
**Output**: Validation methods

---

## 1.4 Add chrono dependency

- [ ] Update `daemon/Cargo.toml`:
  ```toml
  [dependencies]
  chrono = { version = "0.4", features = ["serde"] }
  ```

**Dependencies**: None
**Output**: Chrono added

---

## 1.5 Write schema tests

- [ ] Create `daemon/src/provider/manifest_test.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      fn sample_manifest() -> BootManifest {
          let mut artifacts = HashMap::new();
          artifacts.insert("kernel".to_string(), Artifact {
              hash: "sha256:".to_string() + &"a".repeat(64),
              size: 1024,
              path: "/stable/arm64/kernel".to_string(),
          });
          artifacts.insert("initramfs".to_string(), Artifact {
              hash: "sha256:".to_string() + &"b".repeat(64),
              size: 512,
              path: "/stable/arm64/initramfs".to_string(),
          });

          BootManifest {
              manifest_version: 1,
              version: "2025.11.26".to_string(),
              channel: "stable".to_string(),
              arch: "arm64".to_string(),
              created_at: "2025-11-26T12:00:00Z".to_string(),
              expires_at: "2025-12-26T12:00:00Z".to_string(),
              artifacts,
              signatures: vec![Signature {
                  keyid: "ed25519:test".to_string(),
                  sig: "base64sig".to_string(),
              }],
              provider: None,
          }
      }

      #[test]
      fn test_serialize_deserialize() {
          let manifest = sample_manifest();
          let json = manifest.to_json().unwrap();
          let parsed = BootManifest::from_json(&json).unwrap();
          assert_eq!(manifest.version, parsed.version);
      }

      #[test]
      fn test_validate_success() {
          let manifest = sample_manifest();
          assert!(manifest.validate().is_ok());
      }

      #[test]
      fn test_validate_missing_artifact() {
          let mut manifest = sample_manifest();
          manifest.artifacts.remove("kernel");
          assert!(matches!(
              manifest.validate(),
              Err(ManifestError::MissingArtifact(_))
          ));
      }
  }
  ```

**Dependencies**: Task 1.3
**Output**: Schema tests

---

## Validation Checklist

- [ ] BootManifest struct matches phase-verify expectations
- [ ] JSON serialization produces valid JSON
- [ ] Round-trip serialization works correctly
- [ ] Validation catches missing artifacts
- [ ] Validation catches invalid hashes
- [ ] Validation catches expired manifests
- [ ] All tests pass
