// SPDX-License-Identifier: Apache-2.0

//! Content-addressed HTTP server for distributing any signed blob across the
//! Phase network. Generic over artifact payload type so the same server can
//! ship boot images, WASM modules, model weights, or any other Phase
//! artifact.
//!
//! # Background
//!
//! Phase Boot's original `provider/` module under `daemon/` exposed a
//! channel-and-architecture keyed HTTP layout: `/<channel>/<arch>/<filename>`.
//! That layout is preserved here byte-for-byte so existing phase-boot USB
//! images keep working, but it is now joined by a second, content-addressed
//! layout keyed by the SHA-256 of the blob: `/blobs/<aa>/<full_hex>.bin`.
//!
//! # Two layouts, one server
//!
//! ```text
//! artifacts/
//! ├── stable/
//! │   └── x86_64/
//! │       ├── kernel               <- legacy channel/arch path
//! │       └── initramfs.img        <- legacy channel/arch path
//! └── blobs/
//!     └── ab/
//!         └── ab8723...c4.bin     <- content-addressed blob (SHA-256 hex)
//! ```
//!
//! Both layouts share the same `ArtifactStore`, the same Range-request
//! aware HTTP handler, and the same metrics counters.
//!
//! # Quick start
//!
//! ```no_run
//! use phase_artifact_server::{ArtifactServer, ArtifactServerConfig};
//! use std::path::PathBuf;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let config = ArtifactServerConfig {
//!     bind_addr: "127.0.0.1".to_string(),
//!     port: 8080,
//!     artifacts_dir: PathBuf::from("/var/lib/phase/artifacts"),
//! };
//! let server = ArtifactServer::new(config)?;
//!
//! // Add a content-addressed blob — returned BlobId is the SHA-256 hex.
//! let blob_id = server.add_blob(b"hello world").await?;
//! println!("Blob available at /blobs/{}/{}.bin", &blob_id.as_str()[..2], blob_id.as_str());
//!
//! // Or add a legacy channel/arch artifact.
//! server.add_channel_artifact("stable", "x86_64", "kernel", b"...kernel bytes...").await?;
//!
//! // Serve.
//! server.serve_on(([127, 0, 0, 1], 8080).into()).await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]

pub mod artifacts;
pub mod config;
pub mod dht;
pub mod mdns;
pub mod metrics;
pub mod server;

pub use artifacts::{ArtifactMeta, ArtifactStore, BlobId};
pub use config::ArtifactServerConfig;
pub use dht::{ManifestRecord, DEFAULT_MANIFEST_TTL, MANIFEST_REFRESH_INTERVAL};
pub use mdns::{MdnsAdvertiser, MdnsConfig, MDNS_SERVICE_TYPE};
pub use metrics::{
    perform_health_check, HealthCheck, HealthChecks, MetricsSnapshot, ProviderMetrics,
};
pub use server::{ArtifactServer, ManifestProvider, ServerHandle};
