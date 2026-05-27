//! Boundary integration test #5: persistent identity load/save.
//!
//! This test originally used a hand-rolled stdlib shim because the
//! `phase-identity` crate did not yet exist. With M3 of phase-core now
//! landed, it calls the real `phase_identity::NodeIdentity::*` API
//! directly. The four behavioural assertions are unchanged -- they are the
//! contract `phase-identity` must preserve:
//!
//!   (a) generate + persist + reload returns the same key bytes
//!   (b) loading from a path that does not exist creates a new key on disk
//!   (c) two consecutive loads from the same existing path return the same key
//!   (d) signatures produced before and after a reload verify under the same
//!       public key (this is the actual operational guarantee callers need)
//!
//! The on-disk format is the raw 32-byte ed25519 secret with 0600
//! permissions on Unix -- exactly what the shim used, so the file layout
//! is also unchanged.

use ed25519_dalek::Verifier;
use phase_identity::NodeIdentity;
use std::path::PathBuf;
use tempfile::TempDir;

fn fresh_temp() -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("identity").join("node.key");
    (temp, path)
}

#[test]
fn persistent_identity_simulated_restart() {
    // (b) Loading from a nonexistent path must generate AND persist a key.
    let (temp_b, path_b) = fresh_temp();
    assert!(!path_b.exists(), "precondition: identity file must not exist");
    let created = NodeIdentity::load_or_create(&path_b).expect("create new identity");
    assert!(
        path_b.exists(),
        "load_or_create must persist a new key to disk"
    );
    let created_pub = created.verifying_key().to_bytes();
    drop(temp_b); // keep TempDir alive until here

    // (a) Persist + reload returns the same key bytes.
    let (_temp_a_guard, path_a) = fresh_temp();
    let first = NodeIdentity::load_or_create(&path_a).expect("create identity at path_a");
    let first_secret = first.signing_key().to_bytes();
    let first_pub = first.verifying_key().to_bytes();

    let reloaded = NodeIdentity::load_or_create(&path_a).expect("reload identity at path_a");
    assert_eq!(
        first_secret,
        reloaded.signing_key().to_bytes(),
        "reloaded key secret must match originally written secret"
    );
    assert_eq!(
        first_pub,
        reloaded.verifying_key().to_bytes(),
        "reloaded public key must match"
    );

    // (c) Two consecutive loads from the same existing path return the same key.
    let again = NodeIdentity::load_or_create(&path_a).expect("third load at path_a");
    assert_eq!(
        reloaded.signing_key().to_bytes(),
        again.signing_key().to_bytes(),
        "consecutive loads from same path must be deterministic"
    );

    // (d) Operational guarantee: a signature produced before reload must
    //     verify under the public key derived after reload.
    let message = b"phase boundary test #5 -- restart simulation";
    let sig_before = first.sign(message);
    let pub_after = reloaded.verifying_key();
    assert!(
        pub_after.verify(message, &sig_before).is_ok(),
        "pre-restart signature must verify under post-restart public key"
    );

    // peer_id_bytes() and verifying_key().to_bytes() must agree, since
    // libp2p derives the PeerId from these 32 bytes.
    assert_eq!(
        reloaded.peer_id_bytes(),
        reloaded.verifying_key().to_bytes(),
        "peer_id_bytes must match the verifying key bytes"
    );

    // (b) sanity: the key created from the nonexistent-file branch must be
    // a valid 32-byte pubkey distinct from the path_a key.
    assert_eq!(created_pub.len(), 32);
    assert_ne!(
        created_pub, first_pub,
        "independent identity files must produce independent keys"
    );
}
