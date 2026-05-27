// Manual verification: peer-id must survive a "restart".
// Compiled against daemon/ so we use the SAME libp2p version the daemon does.
use libp2p::{identity::Keypair, PeerId};
use phase_identity::NodeIdentity;
use std::path::Path;

fn peer_id_for(id: &NodeIdentity) -> PeerId {
    let mut secret = id.signing_key().to_bytes();
    let kp = Keypair::ed25519_from_bytes(&mut secret).expect("derive libp2p keypair");
    PeerId::from(kp.public())
}

fn main() {
    let path = Path::new("/tmp/phase-test-key");
    let _ = std::fs::remove_file(path); // start fresh

    let first = NodeIdentity::load_or_create(path).expect("first load");
    let first_peer = peer_id_for(&first);
    println!("first  peer-id: {}", first_peer);

    // Drop the value to simulate "process exit", then reload from disk.
    drop(first);

    let second = NodeIdentity::load_or_create(path).expect("second load");
    let second_peer = peer_id_for(&second);
    println!("second peer-id: {}", second_peer);

    assert_eq!(first_peer, second_peer, "peer-id must be stable across restarts");
    println!("OK: peer-id stable across simulated restart");
}
