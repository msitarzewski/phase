// SPDX-License-Identifier: Apache-2.0

//! Minimal configuration shared between the HTTP server and the
//! [`crate::artifacts::ArtifactStore`].
//!
//! The historical daemon-side `ProviderConfig` carried channel / arch
//! defaults so it could synthesize a `default_manifest_handler` answer. Those
//! fields are still meaningful for Phase Boot's boot-image use case, but
//! they are not intrinsic to "an HTTP server that serves content-addressed
//! blobs". They now live on the daemon side. This crate's
//! [`ArtifactServerConfig`] only carries fields the server itself needs:
//! where to bind and where the artifacts live on disk.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for [`crate::server::ArtifactServer`].
///
/// Identifies the bind address, port, and on-disk artifact root. The
/// daemon-side caller is responsible for resolving any environment-dependent
/// defaults (platform-specific paths, channel/arch labels, etc.) before
/// constructing this value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactServerConfig {
    /// Interface to bind, typically `"0.0.0.0"` or `"127.0.0.1"`.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// TCP port for the HTTP server.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Root directory holding artifacts. Both the channel/arch layout and
    /// the content-addressed `blobs/` layout live underneath this directory.
    pub artifacts_dir: PathBuf,
}

impl ArtifactServerConfig {
    /// Format the bind address as `host:port` for [`tokio::net::TcpListener`].
    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.bind_addr, self.port)
    }
}

fn default_bind_addr() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bind_address_format() {
        let config = ArtifactServerConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 9090,
            artifacts_dir: PathBuf::from("/tmp/x"),
        };
        assert_eq!(config.bind_address(), "127.0.0.1:9090");
    }
}
