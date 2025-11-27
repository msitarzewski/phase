# Task 3 — Ed25519 Signing

**Agent**: Security Agent
**Estimated**: 2 days

## 3.1 Reuse existing signing infrastructure

- [ ] Locate existing signing code in plasmd:
  ```
  daemon/src/network/signing.rs  # or similar
  ```
- [ ] Identify how job receipts are signed
- [ ] Document the signing key location and format

**Dependencies**: None
**Output**: Understanding of existing signing

---

## 3.2 Create manifest signing module

- [ ] Create `daemon/src/provider/signing.rs`:
  ```rust
  use ed25519_dalek::{SigningKey, Signature, Signer, VerifyingKey};
  use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

  use super::manifest::{BootManifest, Signature as ManifestSignature};

  /// Sign a manifest with the given signing key
  pub fn sign_manifest(
      manifest: &mut BootManifest,
      signing_key: &SigningKey,
  ) -> Result<(), SigningError> {
      // Get bytes to sign (manifest without signatures)
      let signable = manifest.signable_bytes()
          .map_err(|e| SigningError::SerializationError(e.to_string()))?;

      // Sign
      let signature: Signature = signing_key.sign(&signable);

      // Get public key for keyid
      let verifying_key = signing_key.verifying_key();
      let keyid = format!("ed25519:{}", hex::encode(verifying_key.as_bytes()));

      // Add signature to manifest
      manifest.signatures.push(ManifestSignature {
          keyid,
          sig: BASE64.encode(signature.to_bytes()),
      });

      Ok(())
  }

  /// Verify a manifest signature
  pub fn verify_manifest_signature(
      manifest: &BootManifest,
      verifying_key: &VerifyingKey,
  ) -> Result<bool, SigningError> {
      // Find signature matching this key
      let keyid = format!("ed25519:{}", hex::encode(verifying_key.as_bytes()));

      let sig = manifest.signatures.iter()
          .find(|s| s.keyid == keyid)
          .ok_or(SigningError::SignatureNotFound)?;

      // Decode signature
      let sig_bytes = BASE64.decode(&sig.sig)
          .map_err(|_| SigningError::InvalidSignature)?;

      let signature = Signature::from_bytes(&sig_bytes.try_into()
          .map_err(|_| SigningError::InvalidSignature)?);

      // Get signable bytes
      let mut unsigned = manifest.clone();
      unsigned.signatures = Vec::new();
      let signable = serde_json::to_vec(&unsigned)
          .map_err(|e| SigningError::SerializationError(e.to_string()))?;

      // Verify
      Ok(verifying_key.verify_strict(&signable, &signature).is_ok())
  }

  #[derive(Debug, thiserror::Error)]
  pub enum SigningError {
      #[error("Serialization error: {0}")]
      SerializationError(String),

      #[error("Signature not found for key")]
      SignatureNotFound,

      #[error("Invalid signature format")]
      InvalidSignature,
  }
  ```

**Dependencies**: Task 3.1
**Output**: Signing module

---

## 3.3 Add base64 dependency

- [ ] Update `daemon/Cargo.toml`:
  ```toml
  [dependencies]
  base64 = "0.22"
  # ed25519-dalek should already be present
  ```

**Dependencies**: None
**Output**: Base64 crate added

---

## 3.4 Integrate with ProviderState

- [ ] Add signing key to provider state:
  ```rust
  use ed25519_dalek::SigningKey;

  pub struct ProviderState {
      // ... existing fields
      pub signing_key: SigningKey,
  }

  impl ProviderState {
      /// Create provider state with signing key from discovery module
      pub fn new(config: ProviderConfig, signing_key: SigningKey) -> Self {
          Self {
              config,
              started_at: std::time::Instant::now(),
              requests_served: 0,
              bytes_served: 0,
              hash_cache: HashMap::new(),
              signing_key,
          }
      }

      /// Get public key ID for manifests
      pub fn keyid(&self) -> String {
          let verifying_key = self.signing_key.verifying_key();
          format!("ed25519:{}", hex::encode(verifying_key.as_bytes()))
      }
  }
  ```

**Dependencies**: Task 3.2
**Output**: Signing key in state

---

## 3.5 Share key with discovery module

- [ ] The signing key should come from the same source as the DHT identity
- [ ] Option A: Pass key from main when creating both Discovery and Provider
- [ ] Option B: Load key once, share via Arc
  ```rust
  // In main.rs
  let signing_key = load_or_generate_signing_key(&config)?;

  // Share with discovery
  let discovery = Discovery::new(config, signing_key.clone())?;

  // Share with provider
  let provider_state = ProviderState::new(provider_config, signing_key);
  ```

**Dependencies**: Task 3.4
**Output**: Key sharing implemented

---

## 3.6 Test signing

- [ ] Create tests:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use ed25519_dalek::SigningKey;
      use rand::rngs::OsRng;

      fn sample_manifest() -> BootManifest {
          // ... from Task 1 tests
      }

      #[test]
      fn test_sign_manifest() {
          let signing_key = SigningKey::generate(&mut OsRng);
          let mut manifest = sample_manifest();
          manifest.signatures.clear();

          sign_manifest(&mut manifest, &signing_key).unwrap();

          assert_eq!(manifest.signatures.len(), 1);
          assert!(manifest.signatures[0].keyid.starts_with("ed25519:"));
      }

      #[test]
      fn test_verify_signature() {
          let signing_key = SigningKey::generate(&mut OsRng);
          let mut manifest = sample_manifest();
          manifest.signatures.clear();

          sign_manifest(&mut manifest, &signing_key).unwrap();

          let verifying_key = signing_key.verifying_key();
          assert!(verify_manifest_signature(&manifest, &verifying_key).unwrap());
      }

      #[test]
      fn test_verify_tampered_fails() {
          let signing_key = SigningKey::generate(&mut OsRng);
          let mut manifest = sample_manifest();
          manifest.signatures.clear();

          sign_manifest(&mut manifest, &signing_key).unwrap();

          // Tamper with manifest
          manifest.version = "tampered".to_string();

          let verifying_key = signing_key.verifying_key();
          assert!(!verify_manifest_signature(&manifest, &verifying_key).unwrap());
      }
  }
  ```

**Dependencies**: Task 3.5
**Output**: Signing tests

---

## Validation Checklist

- [ ] Can sign manifest with Ed25519 key
- [ ] Signature format matches phase-verify expectations
- [ ] keyid is "ed25519:<hex public key>"
- [ ] sig is base64-encoded signature
- [ ] Verification succeeds for valid signatures
- [ ] Verification fails for tampered manifests
- [ ] Signing key shared between discovery and provider
- [ ] All tests pass
