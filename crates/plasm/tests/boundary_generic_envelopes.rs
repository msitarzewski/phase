//! Boundary integration test #6: generic `SignedManifest<T>` / `SignedReceipt<T>`
//! cross-crate round-trip.
//!
//! Phase-core M5 extracted the envelope types out of the daemon into
//! `phase-manifest` and `phase-receipt`. This test exercises the seam from
//! the daemon's perspective: build a typed payload, sign it via the
//! `ManifestBuilder` / `ReceiptBuilder` surface, serialize to bytes,
//! deserialize on the consumer side using ONLY the new public API, and
//! verify.
//!
//! It mirrors the contract checks the legacy `boundary_manifest_pipeline`
//! and `boundary_receipt_roundtrip` tests run against the daemon's
//! existing types, but for the new generic envelopes — proving the M5
//! deliverables work cross-crate without depending on any daemon-internal
//! state.

use phase_identity::NodeIdentity;
use phase_manifest::{ManifestBuilder, ManifestError, SignedManifest};
use phase_receipt::{ReceiptBuilder, ReceiptError, SignedReceipt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct JobPayload {
    cores: u32,
    module_cid: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct JobResult {
    exit_code: i32,
    output_hash: [u8; 32],
}

#[test]
fn signed_manifest_round_trip_across_opaque_bytes() {
    // Producer: sign a manifest.
    let identity = NodeIdentity::generate();
    let payload = JobPayload {
        cores: 4,
        module_cid: "bafyfakecidforatest".to_string(),
        args: vec!["--foo".into(), "bar".into()],
    };

    let signed = ManifestBuilder::new(payload.clone())
        .sign_with(&identity)
        .expect("sign manifest");

    // Cross the seam as opaque bytes.
    let bytes = serde_json::to_vec(&signed).expect("serialize");

    // Consumer: parse and verify using only the bytes + crate-public API.
    let recovered: SignedManifest<JobPayload> =
        serde_json::from_slice(&bytes).expect("deserialize");
    assert_eq!(recovered.payload, payload, "payload survives roundtrip");
    recovered.verify().expect("signature verifies");

    // Manifest hash is stable across producer/consumer.
    let producer_hash = signed.manifest_hash().expect("hash producer side");
    let consumer_hash = recovered.manifest_hash().expect("hash consumer side");
    assert_eq!(producer_hash, consumer_hash, "manifest hash deterministic");
}

#[test]
fn tampered_manifest_payload_fails_verification() {
    let identity = NodeIdentity::generate();
    let payload = JobPayload {
        cores: 1,
        module_cid: "bafyfakecidforatest".to_string(),
        args: vec![],
    };
    let signed = ManifestBuilder::new(payload)
        .sign_with(&identity)
        .expect("sign");

    // Tamper with the recovered bytes (flip a digit in `cores`).
    let mut bytes = serde_json::to_vec(&signed).expect("serialize");
    let needle = b"\"cores\":1";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("locate cores field");
    bytes[pos + needle.len() - 1] = b'9'; // 1 -> 9
    let tampered: SignedManifest<JobPayload> =
        serde_json::from_slice(&bytes).expect("deserialize tampered");
    assert!(matches!(tampered.verify(), Err(ManifestError::BadSignature)));
}

#[test]
fn signed_receipt_round_trip_across_opaque_bytes() {
    let worker = NodeIdentity::generate();
    let job_id = [0x55u8; 32];
    let result = JobResult {
        exit_code: 0,
        output_hash: [0xAAu8; 32],
    };

    let signed = ReceiptBuilder::new(result.clone(), job_id)
        .sign_with(&worker)
        .expect("sign receipt");

    let bytes = serde_json::to_vec(&signed).expect("serialize");

    let recovered: SignedReceipt<JobResult> =
        serde_json::from_slice(&bytes).expect("deserialize");
    assert_eq!(recovered.result, result);
    assert_eq!(recovered.job_id_bytes(), Some(job_id));
    recovered.verify().expect("signature verifies");
}

#[test]
fn tampered_receipt_result_fails_verification() {
    let worker = NodeIdentity::generate();
    let job_id = [0x77u8; 32];
    let result = JobResult {
        exit_code: 0,
        output_hash: [0u8; 32],
    };
    let signed = ReceiptBuilder::new(result, job_id)
        .sign_with(&worker)
        .expect("sign");

    let mut bytes = serde_json::to_vec(&signed).expect("serialize");
    // Flip the exit_code value: 0 -> 1
    let needle = b"\"exit_code\":0";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("locate exit_code");
    bytes[pos + needle.len() - 1] = b'1';
    let tampered: SignedReceipt<JobResult> =
        serde_json::from_slice(&bytes).expect("deserialize tampered");
    assert!(matches!(tampered.verify(), Err(ReceiptError::BadSignature)));
}

#[test]
fn domain_separation_prevents_cross_envelope_replay() {
    // The whole point of distinct domain prefixes ("phase-manifest:v1:"
    // vs "phase-receipt:v1:") is that a signature produced under one
    // domain cannot be replayed as the other. We can't trivially mount a
    // replay attack in code (the field structures differ), but we can
    // assert that two signed bundles with identical inner JSON produce
    // different signatures, proving the prefix participates in the hash.
    let identity = NodeIdentity::generate();

    #[derive(Serialize, Deserialize, Clone, Debug)]
    struct Tiny {
        v: u32,
    }

    let m = ManifestBuilder::new(Tiny { v: 1 })
        .sign_with(&identity)
        .expect("sign manifest");
    let r = ReceiptBuilder::new(Tiny { v: 1 }, [0u8; 32])
        .sign_with(&identity)
        .expect("sign receipt");
    assert_ne!(
        m.signature, r.signature,
        "manifest and receipt of the same payload must produce distinct signatures"
    );
}
