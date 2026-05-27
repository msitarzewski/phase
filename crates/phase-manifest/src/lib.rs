// SPDX-License-Identifier: Apache-2.0

//! Signed manifests generic over payload type — [`SignedManifest<T>`]. One
//! signing/verification path serves every kind of Phase job or artifact.
//!
//! # Overview
//!
//! A Phase manifest is a typed payload wrapped in an Ed25519 signature that
//! anyone holding the signer's public key can verify. The payload type is
//! generic: `WasmJobSpec` for Plasm jobs, `InferenceJobSpec` for LUCID
//! jobs, `BootManifest` for Phase Boot, and so on. The crate doesn't know
//! about any of those concrete payloads — it just signs and verifies the
//! envelope they sit inside.
//!
//! # Canonical signing format
//!
//! The signed bytes are the concatenation of:
//!
//! ```text
//! "phase-manifest:v1:" || canonical_json(SigningEnvelope { schema_version, payload, created_at, expires_at })
//! ```
//!
//! `canonical_json` here is RFC 8785-style JCS: object keys sorted
//! lexicographically, no insignificant whitespace, numbers in shortest
//! round-trip form. The domain-separation prefix is stable across versions
//! (the PHP SDK shipped with phase-discover already depends on it).
//!
//! # Quick start
//!
//! ```
//! use phase_identity::NodeIdentity;
//! use phase_manifest::{ManifestBuilder, SignedManifest};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
//! struct MyPayload {
//!     job: String,
//!     cores: u32,
//! }
//!
//! let identity = NodeIdentity::generate();
//! let payload = MyPayload { job: "demo".into(), cores: 4 };
//!
//! let signed: SignedManifest<MyPayload> = ManifestBuilder::new(payload)
//!     .sign_with(&identity)
//!     .expect("sign");
//!
//! signed.verify().expect("verify");
//! ```

#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]

mod canonical;
mod error;
mod manifest;

pub use error::ManifestError;
pub use manifest::{ManifestBuilder, SignedManifest, SCHEMA_VERSION, SIGNING_DOMAIN};

/// Re-exported for callers that want to talk about Ed25519 types without
/// taking a direct dep on ed25519-dalek.
pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
