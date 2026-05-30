// SPDX-License-Identifier: Apache-2.0

#![deny(unsafe_code)]

//! Ed25519 keypair management with persistent on-disk storage. Provides a
//! stable cryptographic identity that survives Phase node restarts.
//!
//! The previous daemon regenerated an ephemeral Ed25519 key on every
//! `Discovery::new()`, so a node's peer-id changed on every restart and any
//! reputation, trust, or discovery signal that depended on identity
//! continuity was effectively unusable. This crate fixes that by giving the
//! daemon a persistent on-disk keypair with platform-aware default storage.
//!
//! # Quick start
//!
//! ```no_run
//! use phase_identity::{NodeIdentity, default_identity_path};
//!
//! # fn main() -> Result<(), phase_identity::IdentityError> {
//! let path = default_identity_path()?;
//! let identity = NodeIdentity::load_or_create(&path)?;
//! let peer_id_bytes = identity.peer_id_bytes();
//! # let _ = peer_id_bytes;
//! # Ok(())
//! # }
//! ```
//!
//! # On-disk format
//!
//! The identity file is the raw 32-byte Ed25519 secret. On Unix the file is
//! written with permissions `0o600`. Parent directories are created on save
//! if they do not already exist. The format intentionally matches the
//! boundary test contract documented in
//! `daemon/tests/boundary_persistent_identity.rs`.

mod default_path;
mod error;
mod keypair;
mod storage;

pub use default_path::default_identity_path;
pub use error::IdentityError;
pub use keypair::NodeIdentity;

// Re-export the underlying ed25519-dalek types so downstream crates do not
// need to add ed25519-dalek directly just to talk about identities.
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
