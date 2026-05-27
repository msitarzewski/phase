//! Boundary integration test #4: receipt round-trip across an opaque-bytes seam.
//!
//! The existing `test_receipt_signing_and_verification` runs sign + verify
//! inside one scope, with shared in-memory types. After M5 the `Receipt`
//! type moves into `phase-receipt`, and consumers will only get bytes
//! plus a hex public key. This test models that contract: we sign in one
//! function, hand only `(&[u8], &str)` to a second function that has no
//! visibility into how the receipt was produced, and verify there.

use ed25519_dalek::SigningKey;
use plasm::wasm::receipt::Receipt;
use rand::RngCore;

/// Producer side: build, sign, and serialise. Returns (json_bytes, pubkey_hex).
fn produce_signed_receipt() -> (Vec<u8>, String) {
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let signing_key = SigningKey::from_bytes(&secret);

    let mut receipt = Receipt::new(
        "sha256:cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe".to_string(),
        0,
        4242,
    );
    receipt.sign(&signing_key).expect("sign receipt");
    let pubkey_hex = receipt.node_pubkey.clone();

    let json = receipt.to_json().expect("serialise receipt");
    (json.into_bytes(), pubkey_hex)
}

/// Consumer side: takes only the JSON bytes and the hex pubkey. This is
/// what the seam looks like once `Receipt` moves into `phase-receipt`.
fn verify_opaque_receipt_bytes(json_bytes: &[u8], pubkey_hex: &str) -> Result<Receipt, String> {
    let json_str = std::str::from_utf8(json_bytes)
        .map_err(|e| format!("receipt bytes not utf-8: {}", e))?;
    let receipt = Receipt::from_json(json_str)?;
    let ok = receipt.verify_with_pubkey_hex(pubkey_hex)?;
    if !ok {
        return Err("signature did not verify".to_string());
    }
    Ok(receipt)
}

#[test]
fn receipt_roundtrip_opaque_json() {
    let (json_bytes, pubkey_hex) = produce_signed_receipt();

    // Consumer has only bytes + pubkey -- no shared in-memory Receipt instance.
    let recovered = verify_opaque_receipt_bytes(&json_bytes, &pubkey_hex)
        .expect("verify recovered receipt");

    assert_eq!(recovered.exit_code, 0);
    assert_eq!(recovered.wall_time_ms, 4242);
    assert_eq!(
        recovered.module_hash,
        "sha256:cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe"
    );
    assert_eq!(recovered.node_pubkey, pubkey_hex);
    assert!(!recovered.signature.is_empty(), "signature must survive round-trip");

    // Negative: tampering a signed field of the JSON must break verification.
    // `Receipt::canonical_message` signs (version, module_hash, exit_code,
    // wall_time_ms, timestamp). We mutate `wall_time_ms` so the recovered
    // receipt's canonical message no longer matches the signed hash.
    let needle = b"\"wall_time_ms\":";
    let pos = json_bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("wall_time_ms field must appear in JSON");
    let mut tampered = json_bytes.clone();
    // Walk past colon + optional whitespace, find first ascii digit, replace it.
    let mut idx = pos + needle.len();
    while idx < tampered.len() && !tampered[idx].is_ascii_digit() {
        idx += 1;
    }
    assert!(idx < tampered.len(), "could not find numeric wall_time_ms");
    tampered[idx] = if tampered[idx] == b'9' { b'1' } else { b'9' };
    assert_ne!(
        tampered, json_bytes,
        "tampering must have mutated the JSON bytes"
    );

    let bad = verify_opaque_receipt_bytes(&tampered, &pubkey_hex);
    assert!(
        bad.is_err(),
        "tampered receipt JSON must fail signature verification (got: {:?})",
        bad
    );

    // Negative: a wrong pubkey must reject. Generate a fresh unrelated key.
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let other_pub = hex::encode(SigningKey::from_bytes(&secret).verifying_key().to_bytes());
    let wrong_key_result = verify_opaque_receipt_bytes(&json_bytes, &other_pub);
    assert!(
        wrong_key_result.is_err(),
        "verification must fail under an unrelated pubkey"
    );
}
