//! Provider module — Phase Boot manifest generation and HTTP serving.
//!
//! Most of this module's old surface area has migrated to the
//! `phase-artifact-server` crate (M6 of phase-core). What remains in daemon/
//! is the Phase Boot-specific `BootManifest` payload type, its
//! `ManifestBuilder` / `ManifestGenerator`, and the Ed25519 signing helpers
//! whose wire format the PHP SDK still depends on. M7 will collapse those
//! the same way once Plasm repositions and the PHP SDK migrates.
//!
//! For new code, depend on `phase-artifact-server` directly. The re-exports
//! below preserve `plasm::provider::*` paths for backwards compat with the
//! CLI, the boundary tests, and downstream consumers.

pub mod manifest;
pub mod config;
pub mod signing;
pub mod generator;

// Re-export the BootManifest payload type and its builder. These stay in
// daemon/ until M7 because phase-boot's USB images regression-test on the
// byte-identical wire format.
pub use manifest::{
    BootManifest,
    ArtifactInfo,
    Signature,
    ProviderInfo,
    ManifestBuilder,
};

// Re-export the daemon-side ProviderConfig (carries channel/arch on top of
// what phase-artifact-server needs).
pub use config::ProviderConfig;

// Re-export signing helpers (Ed25519 over the BootManifest hash).
pub use signing::{
    compute_file_hash,
    compute_manifest_hash,
    key_id,
    sign_manifest,
    verify_manifest_signature,
    generate_signing_key,
};

// Re-export the BootManifest generator.
pub use generator::ManifestGenerator;

// Re-export the HTTP server pieces from phase-artifact-server. These used to
// live in daemon/src/provider/{server,artifacts,metrics,dht,mdns}.rs; they
// moved out in M6. The names are preserved so the CLI, the boundary tests,
// and any external consumer of `plasm::provider::*` keeps compiling.
pub use phase_artifact_server::{
    ArtifactMeta,
    ArtifactServer,
    ArtifactServerConfig,
    ArtifactStore,
    BlobId,
    HealthCheck,
    HealthChecks,
    ManifestProvider,
    ManifestRecord,
    MdnsAdvertiser,
    MdnsConfig,
    MetricsSnapshot,
    ProviderMetrics,
    ServerHandle,
    DEFAULT_MANIFEST_TTL,
    MANIFEST_REFRESH_INTERVAL,
    MDNS_SERVICE_TYPE,
};

mod server_adapter;
pub use server_adapter::ProviderServer;
