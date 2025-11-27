//! mDNS service advertisement for LAN discovery
//!
//! Advertises Phase Boot Provider as a DNS-SD service on the local network.
//! This is separate from libp2p's mDNS peer discovery - we use DNS-SD for
//! HTTP service advertisement so clients can discover boot images via:
//!   `avahi-browse _phase-image._tcp` or `dns-sd -B _phase-image._tcp`

use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, warn};

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

/// mDNS service advertiser
///
/// NOTE: This is a placeholder for the actual mDNS implementation.
/// Full DNS-SD service advertisement requires the `mdns-sd` crate,
/// which should be added to Cargo.toml:
///
/// ```toml
/// mdns-sd = "0.11"
/// ```
///
/// Then implement using mdns_sd::ServiceDaemon:
/// ```rust,ignore
/// use mdns_sd::{ServiceDaemon, ServiceInfo};
///
/// pub struct MdnsAdvertiser {
///     daemon: ServiceDaemon,
///     service_info: ServiceInfo,
/// }
///
/// impl MdnsAdvertiser {
///     pub fn new(config: MdnsConfig) -> Result<Self> {
///         let daemon = ServiceDaemon::new()?;
///
///         let service_info = ServiceInfo::new(
///             MDNS_SERVICE_TYPE,
///             &config.service_name,
///             &format!("{}.local.", hostname()),
///             "",
///             config.http_port,
///             config.txt_records(),
///         )?;
///
///         daemon.register(service_info.clone())?;
///
///         Ok(Self { daemon, service_info })
///     }
///
///     pub fn shutdown(self) -> Result<()> {
///         self.daemon.unregister(&self.service_info.get_fullname())?;
///         self.daemon.shutdown()?;
///         Ok(())
///     }
/// }
/// ```
pub struct MdnsAdvertiser {
    config: MdnsConfig,
}

impl MdnsAdvertiser {
    /// Create and start mDNS advertisement
    ///
    /// This currently logs the configuration but does not perform actual
    /// DNS-SD advertisement. To enable full functionality, add the `mdns-sd`
    /// crate to Cargo.toml and implement using ServiceDaemon (see above).
    pub fn new(config: MdnsConfig) -> Result<Self> {
        info!(
            "mDNS configuration: service={} port={} channel={} arch={}",
            config.service_name, config.http_port, config.channel, config.arch
        );

        warn!(
            "mDNS service advertisement not yet implemented - requires 'mdns-sd' crate"
        );
        warn!(
            "To enable: add 'mdns-sd = \"0.11\"' to Cargo.toml and implement ServiceDaemon"
        );

        info!(
            "Clients can discover via: avahi-browse {} or dns-sd -B {}",
            MDNS_SERVICE_TYPE, MDNS_SERVICE_TYPE
        );

        Ok(Self { config })
    }

    /// Shutdown advertisement
    pub fn shutdown(self) -> Result<()> {
        info!("mDNS advertiser shutdown (placeholder)");
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
    fn test_advertiser_creation() {
        let config = MdnsConfig::new(8080, "stable", "x86_64");
        let advertiser = MdnsAdvertiser::new(config.clone());

        // Should succeed even though it's a placeholder
        assert!(advertiser.is_ok());

        let adv = advertiser.unwrap();
        assert_eq!(adv.config().http_port, 8080);

        // Shutdown should succeed
        assert!(adv.shutdown().is_ok());
    }
}
