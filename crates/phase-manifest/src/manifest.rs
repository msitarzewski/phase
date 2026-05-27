// SPDX-License-Identifier: Apache-2.0

//! `SignedManifest<T>` -- the generic signed envelope.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey, SIGNATURE_LENGTH};
use phase_identity::NodeIdentity;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::canonical::to_canonical_bytes;
use crate::error::ManifestError;

/// Domain-separation prefix included in every signed-bytes message. PHP and
/// future SDK implementations depend on this exact string.
pub const SIGNING_DOMAIN: &[u8] = b"phase-manifest:v1:";

/// Highest schema version this crate understands. Bump alongside any
/// breaking change to the envelope shape (not the payload — payload shape
/// is the caller's choice).
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// SignedManifest<T>
// ---------------------------------------------------------------------------

/// A typed payload `T` wrapped in an Ed25519 signature and a small bundle of
/// envelope metadata. Verifiable by anyone holding `signer_pubkey`.
///
/// The wire format is JSON. Serializes as:
///
/// ```json
/// {
///   "schema_version": 1,
///   "payload": { ... },
///   "signer_pubkey": "hex-32-bytes",
///   "signature":     "hex-64-bytes",
///   "created_at":    "2026-05-27T12:00:00Z",
///   "expires_at":    "2026-06-27T12:00:00Z"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedManifest<T> {
    /// Schema version of this envelope shape. Currently always
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,

    /// The thing being signed. Opaque to this crate; the caller chooses the
    /// concrete payload type.
    pub payload: T,

    /// Ed25519 public key the signature should be verified against, hex
    /// encoded (64 ascii chars). Stored as a hex string rather than `[u8;
    /// 32]` so the JSON wire form is human-readable and matches the
    /// existing PHP SDK contract.
    pub signer_pubkey: String,

    /// Ed25519 signature over `SIGNING_DOMAIN || canonical_json(SigningEnvelope)`,
    /// hex encoded (128 ascii chars).
    pub signature: String,

    /// ISO-8601 / RFC 3339 timestamp when this manifest was issued.
    pub created_at: DateTime<Utc>,

    /// Optional RFC 3339 expiry. Verification surfaces expiry with
    /// [`ManifestError::Expired`] if the current wall-clock is past this;
    /// callers are free to treat expiry as a warning rather than fatal.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// What actually gets hashed-and-signed. Lifted into its own type so the
/// signing message is a function of these fields only — not the public-key /
/// signature fields that aren't known yet at signing time.
#[derive(Debug, Serialize)]
struct SigningEnvelope<'a, T: Serialize> {
    schema_version: u32,
    payload: &'a T,
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime<Utc>>,
}

impl<T> SignedManifest<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Verify this manifest's signature.
    ///
    /// Returns `Ok(())` on a valid signature, recognised schema, and
    /// non-expired manifest. Otherwise a `ManifestError` describing what
    /// failed.
    pub fn verify(&self) -> Result<(), ManifestError> {
        // Schema compatibility first — newer envelopes might add fields a
        // pre-1.x verifier cannot parse without losing information.
        if self.schema_version > SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                found: self.schema_version,
                supported: SCHEMA_VERSION,
            });
        }

        // Decode pubkey + signature.
        let pubkey_bytes = decode_hex32(&self.signer_pubkey)
            .ok_or(ManifestError::BadPublicKey)?;
        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
            .map_err(|_| ManifestError::BadPublicKey)?;

        let sig_bytes = decode_hex64(&self.signature).ok_or(ManifestError::BadSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);

        // Reconstruct the exact bytes the signer signed.
        let message = signing_message(
            self.schema_version,
            &self.payload,
            self.created_at,
            self.expires_at,
        )?;

        // Verify the signature.
        verifying_key
            .verify(&message, &signature)
            .map_err(|_| ManifestError::BadSignature)?;

        // Expiry is the last check so a tampered-with manifest with a past
        // expiry surfaces as BadSignature, not Expired.
        if let Some(exp) = self.expires_at {
            let now = Utc::now();
            if now > exp {
                return Err(ManifestError::Expired {
                    expires_at: exp.to_rfc3339(),
                });
            }
        }

        Ok(())
    }

    /// The Ed25519 public key the signature was produced under, decoded
    /// from the hex string. Returns `None` if the stored value isn't valid
    /// hex / curve point.
    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        let pk = decode_hex32(&self.signer_pubkey)?;
        VerifyingKey::from_bytes(&pk).ok()
    }

    /// Deterministic 32-byte hash of this manifest's signing message —
    /// `SHA-256(SIGNING_DOMAIN || canonical_json(envelope))`. Use this as
    /// a `JobId` so the same manifest always lands on the same job
    /// identifier regardless of which signer produced it.
    pub fn manifest_hash(&self) -> Result<[u8; 32], ManifestError> {
        use sha2::{Digest, Sha256};
        let message = signing_message(
            self.schema_version,
            &self.payload,
            self.created_at,
            self.expires_at,
        )?;
        let mut h = Sha256::new();
        h.update(&message);
        Ok(h.finalize().into())
    }
}

// ---------------------------------------------------------------------------
// ManifestBuilder<T>
// ---------------------------------------------------------------------------

/// Fluent builder for [`SignedManifest<T>`]. The builder constructs the
/// envelope, then `sign_with(identity)` produces the final signed value.
#[derive(Debug)]
pub struct ManifestBuilder<T> {
    payload: T,
    created_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    schema_version: u32,
}

impl<T> ManifestBuilder<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Start a new manifest builder around `payload`.
    pub fn new(payload: T) -> Self {
        Self {
            payload,
            created_at: None,
            expires_at: None,
            schema_version: SCHEMA_VERSION,
        }
    }

    /// Override the `created_at` timestamp. Default: `Utc::now()` at the
    /// moment `sign_with` is called.
    pub fn created_at(mut self, ts: DateTime<Utc>) -> Self {
        self.created_at = Some(ts);
        self
    }

    /// Set an optional expiry.
    pub fn expires_at(mut self, ts: DateTime<Utc>) -> Self {
        self.expires_at = Some(ts);
        self
    }

    /// Override the schema version (rarely needed — defaults to
    /// [`SCHEMA_VERSION`]).
    pub fn schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    /// Sign with the given identity and produce a [`SignedManifest<T>`].
    pub fn sign_with(self, identity: &NodeIdentity) -> Result<SignedManifest<T>, ManifestError> {
        let created_at = self.created_at.unwrap_or_else(Utc::now);
        let message = signing_message(
            self.schema_version,
            &self.payload,
            created_at,
            self.expires_at,
        )?;
        let signature: Signature = identity.signing_key().sign(&message);

        Ok(SignedManifest {
            schema_version: self.schema_version,
            payload: self.payload,
            signer_pubkey: hex_encode(&identity.verifying_key().to_bytes()),
            signature: hex_encode(&signature.to_bytes()),
            created_at,
            expires_at: self.expires_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn signing_message<T: Serialize>(
    schema_version: u32,
    payload: &T,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Vec<u8>, ManifestError> {
    let envelope = SigningEnvelope {
        schema_version,
        payload,
        created_at,
        expires_at,
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
    struct DemoPayload {
        // Out-of-alphabetical-order on purpose: forces the canonical
        // serialization to do real work.
        zeta: u32,
        alpha: String,
        beta: Vec<u8>,
    }

    fn sample_payload() -> DemoPayload {
        DemoPayload {
            zeta: 7,
            alpha: "hello".to_string(),
            beta: vec![1, 2, 3],
        }
    }

    #[test]
    fn round_trip_sign_then_verify() {
        let id = NodeIdentity::generate();
        let signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");
        signed.verify().expect("verify");
    }

    #[test]
    fn json_round_trip_preserves_signature() {
        let id = NodeIdentity::generate();
        let signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");

        let json = serde_json::to_string(&signed).expect("serialize");
        let recovered: SignedManifest<DemoPayload> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(signed, recovered);
        recovered.verify().expect("verify after roundtrip");
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let id = NodeIdentity::generate();
        let signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");

        let mut tampered = signed.clone();
        tampered.payload.alpha = "tampered".into();
        assert!(matches!(
            tampered.verify(),
            Err(ManifestError::BadSignature)
        ));
    }

    #[test]
    fn tampered_timestamp_fails_verification() {
        let id = NodeIdentity::generate();
        let signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");

        let mut tampered = signed.clone();
        tampered.created_at += chrono::Duration::seconds(1);
        assert!(matches!(
            tampered.verify(),
            Err(ManifestError::BadSignature)
        ));
    }

    #[test]
    fn wrong_pubkey_fails_verification() {
        let id = NodeIdentity::generate();
        let other = NodeIdentity::generate();

        let mut signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");

        // Swap signer_pubkey to a different (valid) key; signature won't
        // verify under it.
        signed.signer_pubkey = hex_encode(&other.verifying_key().to_bytes());
        assert!(matches!(signed.verify(), Err(ManifestError::BadSignature)));
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let id = NodeIdentity::generate();
        let mut signed = ManifestBuilder::new(sample_payload())
            .sign_with(&id)
            .expect("sign");
        signed.schema_version = SCHEMA_VERSION + 1;
        assert!(matches!(
            signed.verify(),
            Err(ManifestError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn expired_manifest_surfaces_expired() {
        let id = NodeIdentity::generate();
        let past = Utc::now() - chrono::Duration::seconds(60);
        let signed = ManifestBuilder::new(sample_payload())
            .expires_at(past)
            .sign_with(&id)
            .expect("sign");
        assert!(matches!(signed.verify(), Err(ManifestError::Expired { .. })));
    }

    #[test]
    fn future_expiry_passes() {
        let id = NodeIdentity::generate();
        let future = Utc::now() + chrono::Duration::days(1);
        let signed = ManifestBuilder::new(sample_payload())
            .expires_at(future)
            .sign_with(&id)
            .expect("sign");
        signed.verify().expect("verify pre-expiry");
    }

    #[test]
    fn canonical_signing_is_field_order_independent() {
        // Two payloads with identical *contents* should produce identical
        // canonical bytes — even if their Rust struct fields land in a
        // different order. We can't reorder struct fields at compile time,
        // but we can re-serialise via Value and back, then compare signing
        // messages.
        let p = sample_payload();
        let bytes_a = signing_message(SCHEMA_VERSION, &p, Utc::now(), None).unwrap();
        // Round-trip via JSON Value to lose any field-order information.
        let as_value: serde_json::Value = serde_json::to_value(&p).unwrap();
        let bytes_b = signing_message(SCHEMA_VERSION, &as_value, {
            // Both messages must share the same timestamp for the comparison.
            // Tie them with the same input.
            let parsed = bytes_a.clone();
            // Extract created_at from canonical bytes: skip the domain
            // prefix and locate the substring after `"created_at":`.
            let s = std::str::from_utf8(&parsed[SIGNING_DOMAIN.len()..]).unwrap();
            let needle = "\"created_at\":\"";
            let i = s.find(needle).unwrap() + needle.len();
            let end = s[i..].find('"').unwrap();
            DateTime::parse_from_rfc3339(&s[i..i + end])
                .unwrap()
                .with_timezone(&Utc)
        }, None).unwrap();
        assert_eq!(bytes_a, bytes_b);
    }
}
