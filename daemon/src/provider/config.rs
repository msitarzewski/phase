use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for the HTTP boot provider server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Whether the provider server is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Bind address for HTTP server
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// Port for HTTP server
    #[serde(default = "default_port")]
    pub port: u16,

    /// Directory containing boot artifacts
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: PathBuf,

    /// Release channel to serve (e.g., "stable", "beta", "dev")
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Target architecture (e.g., "x86_64", "aarch64")
    #[serde(default = "default_arch")]
    pub arch: String,
}

fn default_bind_addr() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_artifacts_dir() -> PathBuf {
    // Platform-specific default artifact directory
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/lib/plasm/artifacts")
    }

    #[cfg(target_os = "macos")]
    {
        dirs::data_local_dir()
            .map(|mut path| {
                path.push("plasm");
                path.push("artifacts");
                path
            })
            .unwrap_or_else(|| PathBuf::from("/usr/local/var/plasm/artifacts"))
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
            .map(|mut path| {
                path.push("plasm");
                path.push("artifacts");
                path
            })
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData\\plasm\\artifacts"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        PathBuf::from("/var/lib/plasm/artifacts")
    }
}

fn default_channel() -> String {
    "stable".to_string()
}

fn default_arch() -> String {
    // Auto-detect architecture
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64".to_string()
    }

    #[cfg(target_arch = "aarch64")]
    {
        "aarch64".to_string()
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        std::env::consts::ARCH.to_string()
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: default_bind_addr(),
            port: default_port(),
            artifacts_dir: default_artifacts_dir(),
            channel: default_channel(),
            arch: default_arch(),
        }
    }
}

impl ProviderConfig {
    /// Get the full bind address (addr:port)
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.bind_addr, self.port)
    }

    /// Get default artifacts directory (public for CLI)
    pub fn default_artifacts_dir() -> PathBuf {
        default_artifacts_dir()
    }

    /// Detect system architecture (public for CLI)
    pub fn detect_arch() -> String {
        default_arch()
    }

    /// Normalize architecture name (handle aliases like arm64 <-> aarch64)
    pub fn normalize_arch(arch: &str) -> &str {
        match arch {
            "arm64" => "aarch64",
            "amd64" => "x86_64",
            other => other,
        }
    }

    /// Get list of architecture aliases to try when searching for artifacts
    pub fn arch_aliases(arch: &str) -> Vec<&str> {
        match arch {
            "aarch64" => vec!["aarch64", "arm64"],
            "arm64" => vec!["arm64", "aarch64"],
            "x86_64" => vec!["x86_64", "amd64"],
            "amd64" => vec!["amd64", "x86_64"],
            other => vec![other],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ProviderConfig::default();
        assert_eq!(config.enabled, false);
        assert_eq!(config.bind_addr, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert_eq!(config.channel, "stable");
    }

    #[test]
    fn test_bind_address() {
        let config = ProviderConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 9090,
            ..Default::default()
        };
        assert_eq!(config.bind_address(), "127.0.0.1:9090");
    }

    #[test]
    fn test_arch_detection() {
        let config = ProviderConfig::default();
        // Should auto-detect to a non-empty string
        assert!(!config.arch.is_empty());
    }
}
