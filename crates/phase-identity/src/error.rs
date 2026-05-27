// SPDX-License-Identifier: Apache-2.0

//! Typed error variants returned by `phase-identity` operations. All public
//! fallible APIs return `Result<T, IdentityError>` (not `anyhow::Result`) so
//! callers can match on specific failure modes (e.g. "key file not found"
//! vs "key file corrupted").

use std::io;
use std::path::PathBuf;

/// Errors that can occur while loading, saving, or generating a node
/// identity.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The requested identity file does not exist. Distinct from generic
    /// I/O errors so callers can decide whether to fall back to generation.
    #[error("identity file not found: {0}")]
    NotFound(PathBuf),

    /// The identity file exists but its length does not match the expected
    /// 32-byte raw Ed25519 secret. Indicates either a different on-disk
    /// format or a corrupted file.
    #[error("identity file at {path:?} has wrong length: expected 32 bytes, got {actual}")]
    InvalidLength { path: PathBuf, actual: usize },

    /// Filesystem I/O failed for some reason other than NotFound. The
    /// originating `io::Error` is preserved as the source.
    #[error("identity file I/O error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// No platform-appropriate config directory could be resolved (typically
    /// because the user has no home directory). Returned by
    /// `default_identity_path()`.
    #[error("could not resolve a platform-appropriate config directory for the identity file")]
    NoConfigDir,
}
