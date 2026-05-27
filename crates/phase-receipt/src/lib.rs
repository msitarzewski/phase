// SPDX-License-Identifier: Apache-2.0

//! Signed receipts generic over result type — [`SignedReceipt<T>`]. Verifiable
//! proof that a Phase worker executed a job and produced a result.
//!
//! # Overview
//!
//! A receipt carries the worker-attested *result* of executing a manifest
//! (output commitment, exit code, timing — whatever the workload defines)
//! plus an Ed25519 signature from the worker's identity. Verifiers
//! reconstruct the receipt's signing message and check the signature
//! against `worker_pubkey`.
//!
//! Like [`phase_manifest::SignedManifest`], the payload is generic. Plasm
//! signs `WasmJobResult` payloads; LUCID signs `InferenceJobResult`
//! payloads; future workers can pick their own. Cross-implementation
//! verifiers only need to know how to JSON-decode the payload they care
//! about.
//!
//! # Canonical signing format
//!
//! ```text
//! "phase-receipt:v1:" || canonical_json(SigningEnvelope { schema_version, result, job_id, completed_at })
//! ```
//!
//! Domain separation prevents a manifest signature from being replayed as a
//! receipt signature.

#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]

mod canonical;
mod error;
mod receipt;

pub use error::ReceiptError;
pub use receipt::{ReceiptBuilder, SignedReceipt, SCHEMA_VERSION, SIGNING_DOMAIN};

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
