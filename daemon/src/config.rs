use serde::{Deserialize, Serialize};
use std::path::Path;
use anyhow::Result;

/// Daemon configuration
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Listen address for P2P networking
    pub listen_addr: String,

    /// Bootstrap peer addresses
    pub bootstrap_peers: Vec<String>,

    /// Maximum concurrent jobs
    pub max_concurrent_jobs: usize,

    /// WASM execution limits
    pub limits: ExecutionLimits,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionLimits {
    /// Maximum memory per job (bytes)
    pub max_memory_bytes: u64,

    /// Maximum CPU cores per job
    pub max_cpu_cores: u32,

    /// Maximum execution time (seconds)
    pub max_timeout_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_string(),
            bootstrap_peers: vec![],
            max_concurrent_jobs: 4,
            limits: ExecutionLimits {
                max_memory_bytes: 128 * 1024 * 1024, // 128 MB
                max_cpu_cores: 1,
                max_timeout_seconds: 300, // 5 minutes
            },
        }
    }
}

impl Config {
    /// Load configuration from JSON file
    #[allow(dead_code)]
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Save configuration to JSON file
    #[allow(dead_code)]
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.max_concurrent_jobs, 4);
        assert_eq!(config.limits.max_memory_bytes, 128 * 1024 * 1024);
    }

    #[test]
    fn test_save_load_config() {
        let temp_file = NamedTempFile::new().unwrap();
        let config = Config::default();

        config.save(temp_file.path()).unwrap();
        let loaded = Config::load(temp_file.path()).unwrap();

        assert_eq!(config.max_concurrent_jobs, loaded.max_concurrent_jobs);
        assert_eq!(config.limits.max_memory_bytes, loaded.limits.max_memory_bytes);
    }
}
