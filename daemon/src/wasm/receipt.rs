use serde::{Deserialize, Serialize};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

/// Execution receipt proving work was done
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    /// Receipt version
    pub version: String,

    /// SHA-256 hash of the executed WASM module
    pub module_hash: String,

    /// Exit code (0 = success)
    pub exit_code: u32,

    /// Wall clock execution time (milliseconds)
    pub wall_time_ms: u64,

    /// Timestamp (Unix epoch seconds)
    pub timestamp: u64,

    /// Node public key (Ed25519, hex-encoded)
    pub node_pubkey: String,

    /// Signature over receipt fields (Ed25519, hex-encoded)
    pub signature: String,
}

impl Receipt {
    /// Create a new unsigned receipt
    pub fn new(module_hash: String, exit_code: u32, wall_time_ms: u64) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            version: "0.1".to_string(),
            module_hash,
            exit_code,
            wall_time_ms,
            timestamp,
            node_pubkey: String::new(), // Set when signing
            signature: String::new(),    // Set when signing
        }
    }

    /// Get canonical message to sign (deterministic JSON subset)
    fn canonical_message(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.version,
            self.module_hash,
            self.exit_code,
            self.wall_time_ms,
            self.timestamp
        )
    }

    /// Sign the receipt with an Ed25519 signing key
    pub fn sign(&mut self, signing_key: &SigningKey) -> Result<(), String> {
        // Get verifying (public) key
        let verifying_key = signing_key.verifying_key();
        self.node_pubkey = hex::encode(verifying_key.to_bytes());

        // Create canonical message to sign
        let message = self.canonical_message();

        // Hash the message (defense in depth)
        let mut hasher = Sha256::new();
        hasher.update(message.as_bytes());
        let message_hash = hasher.finalize();

        // Sign the hash
        let signature: Signature = signing_key.sign(&message_hash);
        self.signature = hex::encode(signature.to_bytes());

        Ok(())
    }

    /// Verify the receipt signature
    pub fn verify(&self, verifying_key: &VerifyingKey) -> Result<bool, String> {
        // Decode signature
        let signature_bytes = hex::decode(&self.signature)
            .map_err(|e| format!("Invalid signature hex: {}", e))?;

        let signature = Signature::from_bytes(
            signature_bytes.as_slice().try_into()
                .map_err(|_| "Invalid signature length".to_string())?
        );

        // Recreate canonical message
        let message = self.canonical_message();

        // Hash the message
        let mut hasher = Sha256::new();
        hasher.update(message.as_bytes());
        let message_hash = hasher.finalize();

        // Verify signature
        verifying_key.verify(&message_hash, &signature)
            .map(|_| true)
            .map_err(|e| format!("Signature verification failed: {}", e))
    }

    /// Verify using hex-encoded public key
    pub fn verify_with_pubkey_hex(&self, pubkey_hex: &str) -> Result<bool, String> {
        let pubkey_bytes = hex::decode(pubkey_hex)
            .map_err(|e| format!("Invalid pubkey hex: {}", e))?;

        let verifying_key = VerifyingKey::from_bytes(
            pubkey_bytes.as_slice().try_into()
                .map_err(|_| "Invalid pubkey length".to_string())?
        ).map_err(|e| format!("Invalid verifying key: {}", e))?;

        self.verify(&verifying_key)
    }

    /// Serialize to JSON
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize receipt: {}", e))
    }

    /// Deserialize from JSON
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map_err(|e| format!("Failed to deserialize receipt: {}", e))
    }

    /// Load from JSON file
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        Self::from_json(&content)
    }

    /// Save to JSON file
    pub fn to_file(&self, path: &std::path::Path) -> Result<(), String> {
        let json = self.to_json()?;
        std::fs::write(path, json)
            .map_err(|e| format!("Failed to write file: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receipt_creation() {
        let receipt = Receipt::new("sha256:abc123".to_string(), 0, 1500);
        assert_eq!(receipt.version, "0.1");
        assert_eq!(receipt.exit_code, 0);
        assert_eq!(receipt.wall_time_ms, 1500);
        assert!(receipt.timestamp > 0);
    }

    #[test]
    fn test_receipt_json_serialization() {
        use ed25519_dalek::SigningKey;
        use rand::RngCore;

        let mut receipt = Receipt::new("sha256:abc123".to_string(), 0, 1500);

        // Generate a signing key for testing
        let mut secret_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        receipt.sign(&signing_key).unwrap();

        let json = receipt.to_json().unwrap();

        // Verify JSON contains required fields
        assert!(json.contains("\"version\":"));
        assert!(json.contains("\"module_hash\":"));
        assert!(json.contains("\"signature\":"));

        // Deserialize and verify
        let loaded = Receipt::from_json(&json).unwrap();
        assert_eq!(loaded.module_hash, receipt.module_hash);
        assert_eq!(loaded.exit_code, receipt.exit_code);
        assert_eq!(loaded.wall_time_ms, receipt.wall_time_ms);
    }

    #[test]
    fn test_receipt_signing_and_verification() {
        use ed25519_dalek::SigningKey;
        use rand::RngCore;

        let mut receipt = Receipt::new("sha256:test123".to_string(), 0, 2000);

        // Generate signing key
        let mut secret_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();

        // Sign receipt
        receipt.sign(&signing_key).unwrap();

        // Verify signature is present
        assert!(!receipt.signature.is_empty());
        assert!(!receipt.node_pubkey.is_empty());

        // Verify with correct key
        assert!(receipt.verify(&verifying_key).unwrap());

        // Verify with hex pubkey
        assert!(receipt.verify_with_pubkey_hex(&receipt.node_pubkey).unwrap());

        // Verification should fail with wrong key
        let mut wrong_secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut wrong_secret);
        let wrong_key = SigningKey::from_bytes(&wrong_secret);
        let wrong_verifying = wrong_key.verifying_key();
        assert!(receipt.verify(&wrong_verifying).is_err());
    }
}
