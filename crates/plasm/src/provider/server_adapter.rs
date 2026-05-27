//! Daemon-side adapter that wires `phase_artifact_server::ArtifactServer`
//! to the BootManifest-aware `ManifestGenerator` that still lives in this
//! crate.
//!
//! Pre-M6 the boot HTTP server, the artifact store, and the BootManifest
//! generator all sat in `daemon/src/provider/`. After M6 the server +
//! artifact store + DHT + mDNS pieces are crate `phase-artifact-server`;
//! only the BootManifest payload (which the PHP SDK depends on byte-for-
//! byte) stayed in daemon/. This adapter glues the two halves back together
//! so `plasm serve` keeps producing the same wire format and the existing
//! boundary tests against `plasm::provider::ProviderServer` keep passing.

use anyhow::Result;
use phase_artifact_server::{
    ArtifactServer, ArtifactServerConfig, ManifestProvider,
};
use std::sync::Arc;

use super::config::ProviderConfig;
use super::generator::ManifestGenerator;

/// Pre-M6-compatible HTTP provider server.
///
/// Constructs a `phase_artifact_server::ArtifactServer`, attaches a
/// BootManifest-aware [`ManifestProvider`], and exposes the same `new` +
/// `run` surface the daemon (and the boundary tests) historically used.
pub struct ProviderServer {
    inner: ArtifactServer,
}

impl ProviderServer {
    /// Build a new provider server from the daemon-side [`ProviderConfig`].
    /// The channel/arch defaults from the config become the default channel
    /// + default arch the inner artifact server reports on `/manifest.json`.
    pub fn new(config: ProviderConfig) -> Self {
        let inner_config = ArtifactServerConfig {
            bind_addr: config.bind_addr.clone(),
            port: config.port,
            artifacts_dir: config.artifacts_dir.clone(),
        };

        // The artifact store inside ArtifactServer owns its own copy of the
        // root dir. We construct another ArtifactStore here so the manifest
        // generator can read it; both stores share the same on-disk layout
        // and hash cache eviction is acceptable across them.
        let inner = ArtifactServer::new(inner_config).expect("create artifact server");
        let store = inner.store().clone();

        let provider: Arc<dyn ManifestProvider> = Arc::new(BootManifestProvider {
            generator: Arc::new(ManifestGenerator::new(store, None)),
            default_channel: config.channel.clone(),
            default_arch: config.arch.clone(),
        });

        let inner = inner
            .with_info_name("plasmd-provider")
            .with_info_version(env!("CARGO_PKG_VERSION").to_string())
            .with_manifest_provider(provider);

        Self { inner }
    }

    /// Run the underlying HTTP server. Blocks until the listener errors or
    /// the task is cancelled.
    pub async fn run(self) -> Result<()> {
        self.inner.run().await
    }
}

/// Implementation of [`ManifestProvider`] backed by the BootManifest-
/// specific [`ManifestGenerator`]. Bridges between the generic JSON Value
/// the trait talks in and the typed `BootManifest` the generator returns.
#[derive(Debug)]
struct BootManifestProvider {
    generator: Arc<ManifestGenerator>,
    default_channel: String,
    default_arch: String,
}

impl ManifestProvider for BootManifestProvider {
    fn manifest(&self, channel: &str, arch: &str) -> Result<serde_json::Value> {
        let manifest = self.generator.generate_signed(channel, arch)?;
        Ok(serde_json::to_value(manifest)?)
    }

    fn defaults(&self) -> Option<(String, String)> {
        Some((self.default_channel.clone(), self.default_arch.clone()))
    }
}
