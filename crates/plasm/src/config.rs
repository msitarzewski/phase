use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use anyhow::{Result, Context};

/// Daemon configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Listen addresses for P2P networking (can specify multiple)
    /// Example: ["/ip4/0.0.0.0/tcp/8000", "/ip4/0.0.0.0/tcp/9000"]
    #[serde(default)]
    pub listen_addrs: Vec<String>,

    /// Peer addresses to connect to on startup
    /// Example: ["/ip4/192.168.1.25/tcp/8000"]
    #[serde(default)]
    pub peer_addrs: Vec<String>,

    /// Bootstrap peer addresses (DHT bootstrap nodes)
    #[serde(default)]
    pub bootstrap_peers: Vec<String>,

    /// Maximum concurrent jobs
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: usize,

    /// WASM execution limits
    #[serde(default)]
    pub limits: ExecutionLimits,
}

fn default_max_concurrent_jobs() -> usize {
    4
}

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
            listen_addrs: vec![],  // Empty = use default /ip4/0.0.0.0/tcp/0
            peer_addrs: vec![],
            bootstrap_peers: vec![],
            max_concurrent_jobs: 4,
            limits: ExecutionLimits::default(),
        }
    }
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 128 * 1024 * 1024, // 128 MB
            max_cpu_cores: 1,
            max_timeout_seconds: 300, // 5 minutes
        }
    }
}

impl Config {
    /// Get platform-independent user config directory
    /// Linux: ~/.config/plasm/
    /// macOS: ~/Library/Application Support/plasm/ (or ~/.config/plasm/)
    /// Windows: %APPDATA%\plasm\
    pub fn user_config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|mut path| {
            path.push("plasm");
            path
        })
    }

    /// Get default user config file path
    /// Returns: ~/.config/plasm/config.json (or platform equivalent)
    pub fn default_user_config_path() -> Option<PathBuf> {
        Self::user_config_dir().map(|mut path| {
            path.push("config.json");
            path
        })
    }

    /// Get system-wide config path
    /// Returns: /etc/plasm/config.json on Unix-like systems
    pub fn system_config_path() -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from("/etc/plasm/config.json")
        }
        #[cfg(not(unix))]
        {
            // On Windows, use ProgramData
            PathBuf::from("C:\\ProgramData\\plasm\\config.json")
        }
    }

    /// Try to load config from various locations in order:
    /// 1. Specified path (if provided)
    /// 2. User config: ~/.config/plasm/config.json
    /// 3. System config: /etc/plasm/config.json
    /// 4. Default config if none found
    pub fn load_or_default(path: Option<&str>) -> Result<Self> {
        // If path specified, try loading it
        if let Some(p) = path {
            if Path::new(p).exists() {
                return Self::load(p)
                    .with_context(|| format!("Failed to load config from: {}", p));
            }
        }

        // Try user config
        if let Some(user_path) = Self::default_user_config_path() {
            if user_path.exists() {
                return Self::load(&user_path)
                    .with_context(|| format!("Failed to load user config from: {}", user_path.display()));
            }
        }

        // Try system config
        let sys_path = Self::system_config_path();
        if sys_path.exists() {
            return Self::load(&sys_path)
                .with_context(|| format!("Failed to load system config from: {}", sys_path.display()));
        }

        // Return default config
        Ok(Self::default())
    }

    /// Load configuration from JSON file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Save configuration to JSON file
    /// Creates parent directories if they don't exist
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Initialize a default config file in user's config directory
    pub fn init_user_config() -> Result<PathBuf> {
        let config_path = Self::default_user_config_path()
            .context("Could not determine user config directory")?;

        if config_path.exists() {
            anyhow::bail!("Config already exists at: {}", config_path.display());
        }

        let config = Self::default();
        config.save(&config_path)?;

        Ok(config_path)
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
