use serde::{Deserialize, Serialize};

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

    /// Sign the receipt with an Ed25519 private key
    pub fn sign(&mut self, _private_key: &[u8]) -> Result<(), String> {
        // TODO: Implement signing in later task
        // For now, return placeholder
        self.node_pubkey = "placeholder_pubkey".to_string();
        self.signature = "placeholder_signature".to_string();
        Ok(())
    }

    /// Verify the receipt signature
    pub fn verify(&self, _public_key: &[u8]) -> Result<bool, String> {
        // TODO: Implement verification in later task
        Ok(true)
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
        let mut receipt = Receipt::new("sha256:abc123".to_string(), 0, 1500);
        receipt.sign(&[]).unwrap(); // Add placeholder signature

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
}
