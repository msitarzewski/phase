use serde::{Deserialize, Serialize};

/// Job manifest describing resource requirements and execution constraints
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobManifest {
    /// Manifest version
    pub version: String,

    /// SHA-256 hash of the WASM module
    pub module_hash: String,

    /// CPU cores required (1 for MVP)
    pub cpu_cores: u32,

    /// Memory limit in megabytes
    pub memory_mb: u64,

    /// Maximum execution time in seconds
    pub timeout_seconds: u64,
}

impl JobManifest {
    /// Create a new manifest with default values
    #[allow(dead_code)]
    pub fn new(module_hash: String) -> Self {
        Self {
            version: "0.1".to_string(),
            module_hash,
            cpu_cores: 1,
            memory_mb: 128,
            timeout_seconds: 300,
        }
    }

    /// Validate manifest constraints
    #[allow(dead_code)]
    pub fn validate(&self) -> Result<(), String> {
        if self.cpu_cores < 1 {
            return Err("cpu_cores must be at least 1".to_string());
        }
        if self.memory_mb < 1 {
            return Err("memory_mb must be at least 1".to_string());
        }
        if self.timeout_seconds < 1 {
            return Err("timeout_seconds must be at least 1".to_string());
        }
        Ok(())
    }

    /// Serialize to JSON
    #[allow(dead_code)]
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize manifest: {}", e))
    }

    /// Deserialize from JSON
    #[allow(dead_code)]
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map_err(|e| format!("Failed to deserialize manifest: {}", e))
    }

    /// Load from JSON file
    #[allow(dead_code)]
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        Self::from_json(&content)
    }

    /// Save to JSON file
    #[allow(dead_code)]
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
    fn test_manifest_creation() {
        let manifest = JobManifest::new("sha256:abc123".to_string());
        assert_eq!(manifest.version, "0.1");
        assert_eq!(manifest.cpu_cores, 1);
        assert_eq!(manifest.memory_mb, 128);
    }

    #[test]
    fn test_manifest_validation() {
        let manifest = JobManifest::new("sha256:abc123".to_string());
        assert!(manifest.validate().is_ok());

        let mut invalid = manifest.clone();
        invalid.cpu_cores = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn test_manifest_json_serialization() {
        let manifest = JobManifest::new("sha256:abc123".to_string());
        let json = manifest.to_json().unwrap();

        // Verify JSON contains required fields
        assert!(json.contains("\"version\":"));
        assert!(json.contains("\"module_hash\":"));
        assert!(json.contains("\"cpu_cores\":"));

        // Deserialize and verify
        let loaded = JobManifest::from_json(&json).unwrap();
        assert_eq!(loaded.module_hash, manifest.module_hash);
        assert_eq!(loaded.cpu_cores, manifest.cpu_cores);
    }
}
