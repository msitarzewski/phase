//! Provider module for Phase Boot manifest generation and HTTP serving
//!
//! This module handles:
//! - Manifest creation, validation, and signing for boot artifacts
//! - HTTP server for serving boot artifacts over the network
//! - mDNS service advertisement for local network discovery

pub mod manifest;
pub mod config;
pub mod server;
pub mod artifacts;
pub mod signing;
pub mod metrics;
pub mod generator;
pub mod mdns;
pub mod dht;

// Re-export manifest types
pub use manifest::{
    BootManifest,
    ArtifactInfo,
    Signature,
    ProviderInfo,
    ManifestBuilder,
};

// Re-export HTTP server types
pub use config::ProviderConfig;
pub use server::ProviderServer;

// Re-export artifact types
pub use artifacts::{ArtifactStore, ArtifactMeta};

// Re-export signing functions
pub use signing::{
    compute_file_hash,
    compute_manifest_hash,
    key_id,
    sign_manifest,
    verify_manifest_signature,
    generate_signing_key,
};

// Re-export metrics types
pub use metrics::{ProviderMetrics, MetricsSnapshot, HealthCheck, HealthChecks};

// Re-export generator types
pub use generator::ManifestGenerator;

// Re-export mDNS types
pub use mdns::{MdnsConfig, MdnsAdvertiser, MDNS_SERVICE_TYPE};

// Re-export DHT types
pub use dht::{ManifestRecord, DEFAULT_MANIFEST_TTL, MANIFEST_REFRESH_INTERVAL};
