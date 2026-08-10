// SPDX-License-Identifier: Apache-2.0

//! mDNS service advertisement for LAN discovery.
//!
//! Advertises Phase Boot Provider as a DNS-SD service on the local network.
//! This is separate from libp2p's mDNS peer discovery - we use DNS-SD for
//! HTTP service advertisement so clients can discover boot images via:
//!   `avahi-browse _phase-image._tcp` or `dns-sd -B _phase-image._tcp`

use anyhow::{bail, Result};
use std::collections::HashMap;

/// mDNS service type for Phase boot providers
pub const MDNS_SERVICE_TYPE: &str = "_phase-image._tcp.local.";

/// TXT record keys
pub const TXT_CHANNEL: &str = "channel";
pub const TXT_ARCH: &str = "arch";
pub const TXT_VERSION: &str = "version";
pub const TXT_HTTP_PORT: &str = "http_port";

/// mDNS advertisement configuration
#[derive(Debug, Clone)]
pub struct MdnsConfig {
    pub service_name: String,
    pub http_port: u16,
    pub channel: String,
    pub arch: String,
    pub version: String,
}

impl MdnsConfig {
    /// Create new mDNS configuration
    ///
    /// # Arguments
    /// * `http_port` - Port where HTTP server is listening
    /// * `channel` - Update channel (e.g., "stable", "testing")
    /// * `arch` - Architecture (e.g., "x86_64", "arm64")
    pub fn new(http_port: u16, channel: &str, arch: &str) -> Self {
        Self {
            service_name: format!("plasmd-{}", hostname()),
            http_port,
            channel: channel.to_string(),
            arch: arch.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Generate TXT record entries for DNS-SD
    ///
    /// These records allow clients to filter providers by channel, architecture, etc.
    pub fn txt_records(&self) -> HashMap<String, String> {
        let mut records = HashMap::new();
        records.insert(TXT_CHANNEL.to_string(), self.channel.clone());
        records.insert(TXT_ARCH.to_string(), self.arch.clone());
        records.insert(TXT_VERSION.to_string(), self.version.clone());
        records.insert(TXT_HTTP_PORT.to_string(), self.http_port.to_string());
        records
    }
}

/// Get system hostname, fallback to "unknown"
fn hostname() -> String {
    // Use system hostname if available
    #[cfg(unix)]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("hostname").output() {
            if let Ok(name) = String::from_utf8(output.stdout) {
                return name.trim().to_string();
            }
        }
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("hostname").output() {
            if let Ok(name) = String::from_utf8(output.stdout) {
                return name.trim().to_string();
            }
        }
    }

    "unknown".to_string()
}

/// Legacy DNS-SD artifact advertiser surface.
///
/// Phase peer discovery is provided by the authenticated `phase-net` swarm.
/// This older HTTP DNS-SD surface has no implementation and therefore fails
/// closed instead of claiming that an advertisement was registered.
#[derive(Debug)]
pub struct MdnsAdvertiser {
    config: MdnsConfig,
}

impl MdnsAdvertiser {
    /// Return an explicit unsupported error. Callers must use authenticated
    /// Phase peer discovery or provide a real DNS-SD implementation before
    /// exposing this legacy surface.
    pub fn new(config: MdnsConfig) -> Result<Self> {
        let _ = config;
        bail!("artifact DNS-SD advertisement is unsupported; use authenticated phase-net discovery")
    }

    /// Shutdown advertisement
    pub fn shutdown(self) -> Result<()> {
        Ok(())
    }

    /// Get configuration
    pub fn config(&self) -> &MdnsConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdns_config_creation() {
        let config = MdnsConfig::new(8080, "stable", "x86_64");

        assert_eq!(config.http_port, 8080);
        assert_eq!(config.channel, "stable");
        assert_eq!(config.arch, "x86_64");
        assert_eq!(config.version, env!("CARGO_PKG_VERSION"));
        assert!(!config.service_name.is_empty());
    }

    #[test]
    fn test_txt_records() {
        let config = MdnsConfig::new(8080, "testing", "arm64");
        let records = config.txt_records();

        assert_eq!(records.get(TXT_CHANNEL).unwrap(), "testing");
        assert_eq!(records.get(TXT_ARCH).unwrap(), "arm64");
        assert_eq!(records.get(TXT_HTTP_PORT).unwrap(), "8080");
        assert_eq!(records.get(TXT_VERSION).unwrap(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_hostname() {
        let name = hostname();
        assert!(!name.is_empty());
        // Hostname should be reasonable length
        assert!(name.len() < 256);
    }

    #[test]
    fn test_advertiser_fails_closed_until_dns_sd_is_implemented() {
        let config = MdnsConfig::new(8080, "stable", "x86_64");
        let advertiser = MdnsAdvertiser::new(config.clone());
        assert!(advertiser.is_err());
    }
}
