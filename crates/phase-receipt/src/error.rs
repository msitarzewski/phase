// SPDX-License-Identifier: Apache-2.0

//! Typed errors for receipt construction, signing, and verification.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReceiptError {
    /// Failed to serialize the payload + envelope to canonical JSON.
    #[error("failed to canonicalize receipt for signing: {0}")]
    Canonicalization(String),

    /// Ed25519 verification failed for the recorded `signature` /
    /// `worker_pubkey` pair.
    #[error("receipt signature failed verification")]
    BadSignature,

    /// `worker_pubkey` bytes aren't a valid Ed25519 point.
    #[error("receipt worker_pubkey is not a valid Ed25519 public key")]
    BadPublicKey,

    /// Receipt schema is newer than this verifier understands.
    #[error("receipt schema_version {found} is not supported (expected <= {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
}
