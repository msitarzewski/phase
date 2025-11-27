//! DHT advertisement for boot manifests

use anyhow::{Context, Result};
use libp2p::kad::RecordKey;
use serde::{Deserialize, Serialize};

/// DHT record for boot manifest advertisement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRecord {
    /// Channel (stable, testing)
    pub channel: String,
    /// Architecture (arm64, x86_64)
    pub arch: String,
    /// HTTP URL where manifest can be fetched
    pub manifest_url: String,
    /// Provider's HTTP address (ip:port)
    pub http_addr: String,
    /// Manifest version for cache invalidation
    pub manifest_version: String,
    /// Record creation timestamp (ISO 8601)
    pub created_at: String,
    /// Record TTL in seconds
    pub ttl_secs: u64,
}

/// Default TTL for manifest records (1 hour)
pub const DEFAULT_MANIFEST_TTL: u64 = 3600;

/// Refresh interval (half of TTL)
pub const MANIFEST_REFRESH_INTERVAL: u64 = DEFAULT_MANIFEST_TTL / 2;

impl ManifestRecord {
    /// Create new manifest record
    pub fn new(
        channel: String,
        arch: String,
        http_addr: String,
        manifest_version: String,
    ) -> Self {
        let manifest_url = format!("http://{}/{}/{}/manifest.json", http_addr, channel, arch);
        Self {
            channel,
            arch,
            manifest_url,
            http_addr,
            manifest_version,
            created_at: chrono::Utc::now().to_rfc3339(),
            ttl_secs: DEFAULT_MANIFEST_TTL,
        }
    }

    /// Create DHT key for boot manifest lookup
    /// Format: /phase/{channel}/{arch}/manifest
    pub fn dht_key(channel: &str, arch: &str) -> RecordKey {
        let key_str = format!("/phase/{}/{}/manifest", channel, arch);
        RecordKey::new(&key_str.into_bytes())
    }

    /// Get the DHT key for this record
    pub fn key(&self) -> RecordKey {
        Self::dht_key(&self.channel, &self.arch)
    }

    /// Serialize record to bytes for DHT storage
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("Failed to serialize manifest record")
    }

    /// Deserialize record from DHT bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("Failed to deserialize manifest record")
    }

    /// Check if record has expired
    pub fn is_expired(&self) -> bool {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&self.created_at) {
            let now = chrono::Utc::now();
            let age = now.signed_duration_since(created);
            age.num_seconds() as u64 > self.ttl_secs
        } else {
            true // Invalid timestamp = expired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_record_new() {
        let record = ManifestRecord::new(
            "stable".to_string(),
            "arm64".to_string(),
            "192.168.1.100:8080".to_string(),
            "0.1.0".to_string(),
        );

        assert_eq!(record.channel, "stable");
        assert_eq!(record.arch, "arm64");
        assert_eq!(record.manifest_url, "http://192.168.1.100:8080/stable/arm64/manifest.json");
        assert_eq!(record.ttl_secs, DEFAULT_MANIFEST_TTL);
    }

    #[test]
    fn test_dht_key_format() {
        let key = ManifestRecord::dht_key("stable", "arm64");
        // Key should contain the path
        let key_bytes = key.as_ref();
        let key_str = String::from_utf8_lossy(key_bytes);
        assert!(key_str.contains("/phase/stable/arm64/manifest"));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let record = ManifestRecord::new(
            "testing".to_string(),
            "x86_64".to_string(),
            "10.0.0.1:9000".to_string(),
            "0.2.0".to_string(),
        );

        let bytes = record.to_bytes().unwrap();
        let restored = ManifestRecord::from_bytes(&bytes).unwrap();

        assert_eq!(restored.channel, record.channel);
        assert_eq!(restored.arch, record.arch);
        assert_eq!(restored.http_addr, record.http_addr);
    }

    #[test]
    fn test_not_expired_initially() {
        let record = ManifestRecord::new(
            "stable".to_string(),
            "arm64".to_string(),
            "localhost:8080".to_string(),
            "0.1.0".to_string(),
        );

        assert!(!record.is_expired());
    }
}
