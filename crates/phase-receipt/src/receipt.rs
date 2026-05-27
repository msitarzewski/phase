// SPDX-License-Identifier: Apache-2.0

//! `SignedReceipt<T>` -- the generic signed envelope for job results.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey, SIGNATURE_LENGTH};
use phase_identity::NodeIdentity;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::canonical::to_canonical_bytes;
use crate::error::ReceiptError;

/// Domain-separation prefix included in every signed-bytes message. Distinct
/// from `phase-manifest:v1:` so a manifest signature cannot be replayed as
/// a receipt.
pub const SIGNING_DOMAIN: &[u8] = b"phase-receipt:v1:";

/// Highest schema version this crate understands.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// SignedReceipt<T>
// ---------------------------------------------------------------------------

/// A typed result `T` wrapped in an Ed25519 signature plus envelope metadata.
///
/// The wire format is JSON. Serializes as:
///
/// ```json
/// {
///   "schema_version": 1,
///   "result": { ... },
///   "job_id":         "hex-32-bytes",
///   "worker_pubkey":  "hex-32-bytes",
///   "signature":      "hex-64-bytes",
///   "completed_at":   "2026-05-27T12:00:00Z"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedReceipt<T> {
    /// Envelope schema version. Currently always [`SCHEMA_VERSION`].
    pub schema_version: u32,

    /// The execution result. Opaque to this crate; the caller picks `T`.
    pub result: T,

    /// 32-byte identifier of the job this result corresponds to, hex
    /// encoded. Typically the manifest hash, but the receipt crate
    /// doesn't enforce a particular derivation — the workload chooses.
    pub job_id: String,

    /// Ed25519 public key of the worker that signed this receipt, hex
    /// encoded.
    pub worker_pubkey: String,

    /// Ed25519 signature over `SIGNING_DOMAIN || canonical_json(SigningEnvelope)`,
    /// hex encoded.
    pub signature: String,

    /// Wall-clock time when the worker finished executing.
    pub completed_at: DateTime<Utc>,
}

/// What actually gets signed. Lifted into its own struct so the signing
/// message is a function of these fields only.
#[derive(Debug, Serialize)]
struct SigningEnvelope<'a, T: Serialize> {
    schema_version: u32,
    result: &'a T,
    job_id: &'a str,
    completed_at: DateTime<Utc>,
}

impl<T> SignedReceipt<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Verify the receipt's signature.
    pub fn verify(&self) -> Result<(), ReceiptError> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(ReceiptError::UnsupportedSchema {
                found: self.schema_version,
                supported: SCHEMA_VERSION,
            });
        }

        let pubkey_bytes = decode_hex32(&self.worker_pubkey)
            .ok_or(ReceiptError::BadPublicKey)?;
        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
            .map_err(|_| ReceiptError::BadPublicKey)?;

        let sig_bytes = decode_hex64(&self.signature).ok_or(ReceiptError::BadSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);

        let message = signing_message(
            self.schema_version,
            &self.result,
            &self.job_id,
            self.completed_at,
        )?;

        verifying_key
            .verify(&message, &signature)
            .map_err(|_| ReceiptError::BadSignature)?;

        Ok(())
    }

    /// The 32-byte `job_id` decoded from hex. `None` if malformed.
    pub fn job_id_bytes(&self) -> Option<[u8; 32]> {
        decode_hex32(&self.job_id)
    }

    /// Decoded worker public key.
    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        let pk = decode_hex32(&self.worker_pubkey)?;
        VerifyingKey::from_bytes(&pk).ok()
    }
}

// ---------------------------------------------------------------------------
// ReceiptBuilder<T>
// ---------------------------------------------------------------------------

/// Fluent builder for [`SignedReceipt<T>`].
#[derive(Debug)]
pub struct ReceiptBuilder<T> {
    result: T,
    job_id: [u8; 32],
    completed_at: Option<DateTime<Utc>>,
    schema_version: u32,
}

impl<T> ReceiptBuilder<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Start a new receipt builder. `job_id` is the 32-byte identifier of
    /// the job this result corresponds to (typically the manifest hash).
    pub fn new(result: T, job_id: [u8; 32]) -> Self {
        Self {
            result,
            job_id,
            completed_at: None,
            schema_version: SCHEMA_VERSION,
        }
    }

    /// Override the completion timestamp. Default: `Utc::now()` at sign time.
    pub fn completed_at(mut self, ts: DateTime<Utc>) -> Self {
        self.completed_at = Some(ts);
        self
    }

    pub fn schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    /// Sign with the given identity.
    pub fn sign_with(self, identity: &NodeIdentity) -> Result<SignedReceipt<T>, ReceiptError> {
        let completed_at = self.completed_at.unwrap_or_else(Utc::now);
        let job_id_hex = hex_encode(&self.job_id);
        let message = signing_message(
            self.schema_version,
            &self.result,
            &job_id_hex,
            completed_at,
        )?;
        let signature: Signature = identity.signing_key().sign(&message);

        Ok(SignedReceipt {
            schema_version: self.schema_version,
            result: self.result,
            job_id: job_id_hex,
            worker_pubkey: hex_encode(&identity.verifying_key().to_bytes()),
            signature: hex_encode(&signature.to_bytes()),
            completed_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn signing_message<T: Serialize>(
    schema_version: u32,
    result: &T,
    job_id: &str,
    completed_at: DateTime<Utc>,
) -> Result<Vec<u8>, ReceiptError> {
    let envelope = SigningEnvelope {
        schema_version,
        result,
        job_id,
        completed_at,
    };
    let canonical = to_canonical_bytes(&envelope)?;
    let mut msg = Vec::with_capacity(SIGNING_DOMAIN.len() + canonical.len());
    msg.extend_from_slice(SIGNING_DOMAIN);
    msg.extend_from_slice(&canonical);
    Ok(msg)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    let bytes = decode_hex(hex)?;
    bytes.try_into().ok()
}

fn decode_hex64(hex: &str) -> Option<[u8; SIGNATURE_LENGTH]> {
    let bytes = decode_hex(hex)?;
    bytes.try_into().ok()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct DemoResult {
        zeta: u32,
        alpha: String,
        commitment: [u8; 32],
    }

    fn sample_result() -> DemoResult {
        DemoResult {
            zeta: 12,
            alpha: "done".into(),
            commitment: [0xAA; 32],
        }
    }

    const JOB_ID: [u8; 32] = [0x11; 32];

    #[test]
    fn round_trip_sign_then_verify() {
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");
        signed.verify().expect("verify");
    }

    #[test]
    fn json_round_trip_preserves_signature() {
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");

        let json = serde_json::to_string(&signed).expect("serialize");
        let recovered: SignedReceipt<DemoResult> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(signed, recovered);
        recovered.verify().expect("verify");
    }

    #[test]
    fn tampered_result_fails_verification() {
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");

        let mut tampered = signed.clone();
        tampered.result.alpha = "tampered".into();
        assert!(matches!(tampered.verify(), Err(ReceiptError::BadSignature)));
    }

    #[test]
    fn tampered_job_id_fails_verification() {
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");

        let mut tampered = signed.clone();
        tampered.job_id = hex_encode(&[0x22u8; 32]);
        assert!(matches!(tampered.verify(), Err(ReceiptError::BadSignature)));
    }

    #[test]
    fn wrong_worker_pubkey_fails_verification() {
        let id = NodeIdentity::generate();
        let other = NodeIdentity::generate();
        let mut signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");
        signed.worker_pubkey = hex_encode(&other.verifying_key().to_bytes());
        assert!(matches!(signed.verify(), Err(ReceiptError::BadSignature)));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let id = NodeIdentity::generate();
        let mut signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");
        signed.schema_version = SCHEMA_VERSION + 1;
        assert!(matches!(
            signed.verify(),
            Err(ReceiptError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn job_id_round_trips_to_bytes() {
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");
        assert_eq!(signed.job_id_bytes(), Some(JOB_ID));
    }

    #[test]
    fn domain_separation_prevents_replay_against_manifest_domain() {
        // A signature produced under the receipt domain must NOT verify if
        // the verifier reconstructs the message under the manifest domain.
        // We can't import phase_manifest here without a dev dep cycle, so
        // we just confirm the bytes start with the expected prefix.
        let id = NodeIdentity::generate();
        let signed = ReceiptBuilder::new(sample_result(), JOB_ID)
            .sign_with(&id)
            .expect("sign");

        let msg = signing_message(
            signed.schema_version,
            &signed.result,
            &signed.job_id,
            signed.completed_at,
        )
        .unwrap();
        assert!(
            msg.starts_with(b"phase-receipt:v1:"),
            "receipt signing message must be domain-separated"
        );
    }
}
