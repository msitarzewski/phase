// SPDX-License-Identifier: Apache-2.0

//! `NodeIdentity` -- the persistent Ed25519 keypair for a Phase node.
//!
//! Wraps `ed25519_dalek::SigningKey` and adds:
//!   - generate / load / save / load_or_create helpers
//!   - typed `IdentityError` instead of `anyhow`
//!   - `peer_id_bytes()` -- the 32-byte public key, which is also the raw
//!     material from which a libp2p `PeerId` is derived
//!
//! Persistence format is delegated to `storage` (raw 32-byte secret,
//! `0o600` on Unix).

use std::fmt;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand_core::OsRng;

use crate::error::IdentityError;
use crate::storage::{create_new_secret, read_secret, write_secret, SECRET_LEN};

/// A persistent Ed25519 node identity. Cheap to clone (`SigningKey` is a
/// 32-byte secret + cached scalar internally), so callers may freely pass
/// it where they need to sign.
#[derive(Clone)]
pub struct NodeIdentity {
    signing_key: SigningKey,
}

/// Manual `Debug` so a `NodeIdentity` can never leak its secret key. We print
/// only the public key (the same 32 bytes from which the libp2p `PeerId` is
/// derived), never the secret scalar. The derived `Debug` was replaced as a
/// defense-in-depth measure: dalek 2.x already redacts the secret, but this
/// guarantees that even a future `debug!("{identity:?}")` cannot regress into
/// a key leak.
impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut public_key_hex = String::with_capacity(64);
        for byte in self.peer_id_bytes() {
            // Lower-case hex, two digits per byte. No `hex` crate dependency.
            use std::fmt::Write as _;
            let _ = write!(public_key_hex, "{byte:02x}");
        }
        f.debug_struct("NodeIdentity")
            .field("public_key", &public_key_hex)
            .finish_non_exhaustive()
    }
}

impl NodeIdentity {
    /// Generate a fresh random identity. Does not touch disk.
    ///
    /// Every call produces an independent keypair.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    /// Load an existing identity from `path`. Returns
    /// `IdentityError::NotFound` if the file does not exist; use
    /// `load_or_create` if you want creation on absence.
    pub fn load(path: &Path) -> Result<Self, IdentityError> {
        let bytes = read_secret(path)?;
        Ok(Self::from_secret_bytes(bytes))
    }

    /// Load the identity at `path` if it exists, otherwise generate a new
    /// one and persist it to that path. This is the entry point the daemon
    /// uses on startup.
    ///
    /// Race-free: when several callers (threads, processes) hit a fresh path
    /// at once, the persistence layer uses `O_CREAT|O_EXCL` so exactly one
    /// creator wins and writes its key. Every loser observes `AlreadyExists`
    /// and falls back to `load`, so all callers converge on the **same** key
    /// and only one generation is ever published. This guarantees the
    /// daemon's "stable peer id across restart" property even under
    /// concurrent startup.
    pub fn load_or_create(path: &Path) -> Result<Self, IdentityError> {
        match Self::load(path) {
            Ok(id) => Ok(id),
            Err(IdentityError::NotFound(_)) => {
                let candidate = Self::generate();
                let secret = candidate.signing_key.to_bytes();
                match create_new_secret(path, &secret) {
                    // We won the create race: our generated key is the one on
                    // disk.
                    Ok(()) => Ok(candidate),
                    // Someone else created it first; adopt the winner's key.
                    Err(IdentityError::AlreadyExists(_)) => Self::load(path),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Persist this identity to `path`. Creates parent directories if
    /// needed and sets file mode `0o600` on Unix.
    pub fn save(&self, path: &Path) -> Result<(), IdentityError> {
        let secret = self.signing_key.to_bytes();
        write_secret(path, &secret)
    }

    /// The underlying Ed25519 signing key. Exposed by reference so callers
    /// can pass it to libraries that already work with `SigningKey`
    /// (e.g. `phase-manifest` once M5 lands).
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// The Ed25519 public key. Cheap to compute (cached inside
    /// `SigningKey`).
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// The 32 bytes that libp2p uses to derive a `PeerId` from an
    /// Ed25519 public key. Returned as an owned array so callers do not
    /// have to thread lifetimes through their code.
    ///
    /// Two `NodeIdentity` instances that share these bytes will produce
    /// identical libp2p `PeerId`s; this is the operational guarantee the
    /// daemon's "persistent peer id across restart" requirement depends on.
    pub fn peer_id_bytes(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    /// Sign an arbitrary message with this identity.
    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing_key.sign(msg)
    }

    /// Construct from a raw 32-byte secret. Private: external callers
    /// should go through `load`/`load_or_create`/`generate`.
    fn from_secret_bytes(bytes: [u8; SECRET_LEN]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;
    use tempfile::TempDir;

    fn fresh_path() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("identity").join("node.key");
        (tmp, path)
    }

    #[test]
    fn generate_is_nondeterministic() {
        // Each call must produce an independent keypair. Vanishingly small
        // probability of collision, but we still assert inequality so a
        // bug that hard-codes the key would be caught.
        let a = NodeIdentity::generate();
        let b = NodeIdentity::generate();
        assert_ne!(a.peer_id_bytes(), b.peer_id_bytes());
    }

    #[test]
    fn save_then_load_round_trips_public_key() {
        let (_tmp, path) = fresh_path();
        let original = NodeIdentity::generate();
        original.save(&path).expect("save");

        let reloaded = NodeIdentity::load(&path).expect("load");
        assert_eq!(original.peer_id_bytes(), reloaded.peer_id_bytes());
        assert_eq!(
            original.signing_key().to_bytes(),
            reloaded.signing_key().to_bytes()
        );
    }

    #[test]
    fn load_missing_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does/not/exist.key");
        let err = NodeIdentity::load(&missing).expect_err("must error");
        assert!(matches!(err, IdentityError::NotFound(_)));
    }

    #[test]
    fn load_or_create_persists_new_key_when_missing() {
        let (_tmp, path) = fresh_path();
        assert!(!path.exists());
        let id = NodeIdentity::load_or_create(&path).expect("create");
        assert!(path.exists(), "load_or_create must write the new key");
        // Reloading must yield the same key.
        let again = NodeIdentity::load(&path).expect("reload");
        assert_eq!(id.peer_id_bytes(), again.peer_id_bytes());
    }

    #[test]
    fn load_or_create_returns_existing_key_when_present() {
        let (_tmp, path) = fresh_path();
        let first = NodeIdentity::load_or_create(&path).expect("create");
        let second = NodeIdentity::load_or_create(&path).expect("reload");
        assert_eq!(first.peer_id_bytes(), second.peer_id_bytes());
        assert_eq!(
            first.signing_key().to_bytes(),
            second.signing_key().to_bytes()
        );
    }

    #[test]
    fn save_creates_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("deeply").join("nested").join("id.key");
        assert!(!path.parent().unwrap().exists());
        NodeIdentity::generate().save(&path).expect("save");
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, path) = fresh_path();
        NodeIdentity::generate().save(&path).expect("save");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "identity file must be 0600, got {mode:o}");
    }

    #[test]
    fn concurrent_load_or_create_converges_on_one_key() {
        // Two threads race load_or_create on a fresh path. Exactly one
        // creator may win the O_CREAT|O_EXCL publish; both threads must
        // return the SAME key (the winner's), and the loser must adopt it
        // via fallback load rather than generating a second key.
        use std::sync::{Arc, Barrier};

        // Repeat to make the race more likely to surface.
        for _ in 0..32 {
            let tmp = TempDir::new().unwrap();
            let path = Arc::new(tmp.path().join("nested").join("race.key"));
            let barrier = Arc::new(Barrier::new(2));

            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = Arc::clone(&path);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        NodeIdentity::load_or_create(&path)
                            .expect("load_or_create")
                            .peer_id_bytes()
                    })
                })
                .collect();

            let results: Vec<[u8; 32]> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert_eq!(
                results[0], results[1],
                "both concurrent callers must converge on the same key"
            );
            // And that converged key is exactly what is persisted on disk:
            // only one generation was published.
            let on_disk = NodeIdentity::load(&path).expect("load").peer_id_bytes();
            assert_eq!(on_disk, results[0], "disk must hold the single winner key");
        }
    }

    #[test]
    fn debug_prints_public_key_and_never_the_secret() {
        // Defense-in-depth: the manual Debug impl must surface only public
        // material. A future `debug!("{identity:?}")` must never leak the
        // secret scalar.
        let id = NodeIdentity::generate();
        let rendered = format!("{id:?}");

        // The public key (peer-id material) appears as lower-case hex.
        let pub_hex: String = id
            .peer_id_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(
            rendered.contains(&pub_hex),
            "Debug output must contain the public key hex; got: {rendered}"
        );

        // The secret bytes must NOT appear in any common encoding.
        let secret = id.signing_key().to_bytes();
        let secret_hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        assert!(
            !rendered.contains(&secret_hex),
            "Debug output must not contain the secret key hex"
        );
        // Also reject the raw debug-array form of the secret bytes.
        let secret_array_dbg = format!("{secret:?}");
        assert!(
            !rendered.contains(&secret_array_dbg),
            "Debug output must not contain the raw secret byte array"
        );
    }

    #[test]
    fn signature_survives_save_load_cycle() {
        // The operational guarantee the daemon depends on: a signature
        // produced before "restart" verifies under the public key derived
        // after restart.
        let (_tmp, path) = fresh_path();
        let before = NodeIdentity::load_or_create(&path).expect("create");
        let msg = b"phase node identity persistence";
        let sig = before.sign(msg);

        let after = NodeIdentity::load(&path).expect("reload");
        assert!(
            after.verifying_key().verify(msg, &sig).is_ok(),
            "pre-restart signature must verify under post-restart public key"
        );
    }
}
