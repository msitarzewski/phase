// SPDX-License-Identifier: Apache-2.0

//! Typed errors for manifest construction, signing, and verification.

use thiserror::Error;

/// Errors produced while building, signing, or verifying a
/// [`crate::SignedManifest`].
#[derive(Debug, Error)]
pub enum ManifestError {
    /// Failed to serialize the payload + envelope to canonical JSON. This
    /// usually means the payload's `Serialize` impl returned an error
    /// (extremely rare for derived impls; possible for hand-rolled ones).
    #[error("failed to canonicalize manifest for signing: {0}")]
    Canonicalization(String),

    /// The bytes claimed to be a valid signature failed Ed25519 verification
    /// under the manifest's `signer_pubkey`.
    #[error("manifest signature failed verification")]
    BadSignature,

    /// The `signer_pubkey` bytes weren't a valid point on the Ed25519 curve.
    /// A correctly produced manifest cannot exhibit this; it indicates the
    /// manifest was hand-written or tampered with.
    #[error("manifest signer_pubkey is not a valid Ed25519 public key")]
    BadPublicKey,

    /// The manifest's `schema_version` is newer than this crate understands.
    /// Downgrade or upgrade the verifier; the manifest itself may be fine.
    #[error("manifest schema_version {found} is not supported (expected <= {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },

    /// The current wall-clock time is past `expires_at`. Manifests do not
    /// auto-discard themselves; callers decide whether expiry is fatal.
    #[error("manifest expired at {expires_at}")]
    Expired { expires_at: String },
}
