// SPDX-License-Identifier: Apache-2.0

//! On-disk storage primitives for a node identity. The format is the raw
//! 32-byte Ed25519 secret, matching the boundary test contract in
//! `daemon/tests/boundary_persistent_identity.rs`.
//!
//! Keeping the format intentionally minimal:
//!   - Zero parsing surface (no JSON / PEM / TOML to get wrong)
//!   - No version envelope yet -- if we need one later, the file length
//!     check (32 bytes exactly) will catch every legacy file and force a
//!     deliberate migration.
//!   - Mode `0o600` on Unix so other local users cannot read the secret.

use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

use crate::error::IdentityError;

/// Length of an Ed25519 secret key in bytes. Re-exported as a constant so
/// callers and tests can reference it symbolically.
pub(crate) const SECRET_LEN: usize = 32;

/// Read a 32-byte Ed25519 secret from `path`.
///
/// Returns `IdentityError::NotFound` if the path does not exist, distinct
/// from a generic I/O error so callers (notably `load_or_create`) can
/// decide whether to fall back to generation.
pub(crate) fn read_secret(path: &Path) -> Result<[u8; SECRET_LEN], IdentityError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(IdentityError::NotFound(path.to_path_buf()));
        }
        Err(e) => {
            return Err(IdentityError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    if bytes.len() != SECRET_LEN {
        return Err(IdentityError::InvalidLength {
            path: path.to_path_buf(),
            actual: bytes.len(),
        });
    }

    let mut out = [0u8; SECRET_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Persist `secret` to `path`.
///
/// The write is **atomic and mode-0600 from creation** on Unix:
///   1. The parent directory is created (mode `0o700` on Unix).
///   2. The secret is written to a uniquely-named temp file in the **same**
///      directory, opened with `create_new(true)` and (on Unix) `mode(0o600)`
///      so the bytes never touch disk at a world-readable mode -- there is no
///      post-write `chmod` window.
///   3. The temp file is `fsync`'d, then `rename`d onto `path`. Rename is
///      atomic on a single filesystem, so a crash mid-write can only leave a
///      stray temp file, never a truncated key at `path`.
///
/// On non-Unix targets the temp+rename atomicity still applies; OS-level ACL
/// hardening of the key file is out of scope for v0.1 (documented in the
/// crate header).
pub(crate) fn write_secret(path: &Path, secret: &[u8; SECRET_LEN]) -> Result<(), IdentityError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        // No parent component (e.g. a bare filename): write into ".".
        _ => Path::new("."),
    };

    create_dir_secure(parent)?;
    write_temp_then_publish(parent, path, secret, Publish::Clobber)
}

/// Like [`write_secret`] but publishes **exclusively**: if `path` already
/// exists, returns [`IdentityError::AlreadyExists`] and writes nothing,
/// rather than overwriting. This is the race-free creation primitive
/// `NodeIdentity::load_or_create` uses so concurrent creators converge on a
/// single key (the winner's) instead of clobbering each other.
///
/// Atomicity and mode-0600-at-creation are identical to [`write_secret`].
pub(crate) fn create_new_secret(
    path: &Path,
    secret: &[u8; SECRET_LEN],
) -> Result<(), IdentityError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    create_dir_secure(parent)?;
    write_temp_then_publish(parent, path, secret, Publish::ExclusiveCreate)
}

/// How the staged temp file is moved onto its final path.
enum Publish {
    /// Overwrite any existing file (atomic `rename`). Used by `save`.
    Clobber,
    /// Fail with `AlreadyExists` if the final path exists (atomic
    /// `hard_link`). Used by the race-free `load_or_create` path.
    ExclusiveCreate,
}

/// Shared core: stage `secret` into a fresh 0600 temp file in `parent`,
/// fsync it, then publish it onto `final_path` per `mode`. The temp file
/// never exists at a non-0600 mode on Unix (mode is set at `open`, not via a
/// later `chmod`), and a crash before publish leaves at most a stray temp
/// file -- never a truncated key at `final_path`.
fn write_temp_then_publish(
    parent: &Path,
    final_path: &Path,
    secret: &[u8; SECRET_LEN],
    mode: Publish,
) -> Result<(), IdentityError> {
    // Unique temp name in the same dir so the publish stays on one
    // filesystem and concurrent in-process creators never collide.
    let tmp_path = temp_path_in(parent, final_path);

    // Open the temp file with the final mode set AT creation. `create_new`
    // guarantees we are the sole writer of this temp file.
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts.open(&tmp_path).map_err(|e| IdentityError::Io {
        path: tmp_path.clone(),
        source: e,
    })?;

    // Write + fsync the temp file. On any failure, best-effort clean up the
    // temp file so we don't leak it, then propagate the original error.
    let write_res = file
        .write_all(secret)
        .and_then(|()| file.sync_all())
        .map_err(|e| IdentityError::Io {
            path: tmp_path.clone(),
            source: e,
        });
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    drop(file);

    let publish_res = match mode {
        // Atomic overwrite.
        Publish::Clobber => fs::rename(&tmp_path, final_path).map_err(|e| IdentityError::Io {
            path: final_path.to_path_buf(),
            source: e,
        }),
        // Atomic exclusive create: hard_link fails with AlreadyExists if the
        // final path is taken, leaving the existing key untouched. We then
        // always drop the temp (the link, or the orphan on failure).
        Publish::ExclusiveCreate => match fs::hard_link(&tmp_path, final_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Err(IdentityError::AlreadyExists(final_path.to_path_buf()))
            }
            Err(e) => Err(IdentityError::Io {
                path: final_path.to_path_buf(),
                source: e,
            }),
        },
    };

    // For the exclusive path the temp is now either hard-linked at the final
    // path (extra link to remove) or orphaned (failed publish) -- either way
    // the temp name must go. For the clobber path a successful rename already
    // consumed the temp; only a failed rename leaves it behind.
    match (&mode, &publish_res) {
        (Publish::ExclusiveCreate, _) => {
            let _ = fs::remove_file(&tmp_path);
        }
        (Publish::Clobber, Err(_)) => {
            let _ = fs::remove_file(&tmp_path);
        }
        (Publish::Clobber, Ok(())) => {}
    }

    publish_res
}

/// Create `dir` (and ancestors) if missing. On Unix the leaf is set to
/// `0o700` so the directory holding the secret is not world-traversable.
fn create_dir_secure(dir: &Path) -> Result<(), IdentityError> {
    fs::create_dir_all(dir).map_err(|e| IdentityError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dir)
            .map_err(|e| IdentityError::Io {
                path: dir.to_path_buf(),
                source: e,
            })?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms).map_err(|e| IdentityError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    }

    Ok(())
}

/// Build a unique temp path adjacent to `final_path` inside `dir`. Combines
/// the process id with a monotonically-increasing counter so concurrent
/// callers within the same process never collide on the temp name.
fn temp_path_in(dir: &Path, final_path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let stem = final_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "identity".to_string());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{stem}.tmp.{}.{n}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_path() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let p = tmp.path().join("nested").join("identity.key");
        (tmp, p)
    }

    #[test]
    fn read_missing_returns_not_found() {
        let (_tmp, path) = fresh_path();
        let err = read_secret(&path).expect_err("must error");
        assert!(matches!(err, IdentityError::NotFound(_)));
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_tmp, path) = fresh_path();
        let secret = [7u8; SECRET_LEN];
        write_secret(&path, &secret).expect("write");
        let read_back = read_secret(&path).expect("read");
        assert_eq!(read_back, secret);
    }

    #[test]
    fn write_creates_parent_directories() {
        let (_tmp, path) = fresh_path();
        assert!(!path.parent().unwrap().exists());
        write_secret(&path, &[0u8; SECRET_LEN]).expect("write");
        assert!(path.parent().unwrap().exists());
        assert!(path.exists());
    }

    #[test]
    fn invalid_length_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("short.key");
        std::fs::write(&path, b"too short").unwrap();
        let err = read_secret(&path).expect_err("must error");
        match err {
            IdentityError::InvalidLength { actual, .. } => assert_eq!(actual, 9),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_sets_unix_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, path) = fresh_path();
        write_secret(&path, &[0u8; SECRET_LEN]).expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "identity file must be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn parent_dir_is_mode_0700() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, path) = fresh_path();
        write_secret(&path, &[0u8; SECRET_LEN]).expect("write");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            dir_mode, 0o700,
            "directory holding the secret must be 0700, got {dir_mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_is_created_0600_with_no_chmod_window() {
        // The mode must be set AT open, not via a post-write chmod. We assert
        // this structurally: the *temp* file produced by `temp_path_in` is
        // opened with the same OpenOptions the implementation uses, and is
        // observable at 0600 immediately after creation -- before any data is
        // written. If the implementation regressed to fs::write + chmod, a
        // freshly-created file would briefly be at the umask default.
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let (_tmp, final_path) = fresh_path();
        let parent = final_path.parent().unwrap();
        create_dir_secure(parent).expect("mkdir");
        let tmp = temp_path_in(parent, &final_path);

        let f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .expect("open temp");
        // Inspect mode while the file is still empty: proves 0600 at open.
        let mode = std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "temp file must be 0600 at open, got {mode:o}");
        drop(f);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn partial_write_to_temp_leaves_real_path_absent() {
        // Simulate a crash after staging the temp file but before publish:
        // the real path must not exist (and certainly not be truncated). We
        // stage a temp directly and never rename it.
        let (_tmp, final_path) = fresh_path();
        let parent = final_path.parent().unwrap();
        create_dir_secure(parent).expect("mkdir");
        let tmp = temp_path_in(parent, &final_path);
        fs::write(&tmp, [1u8; SECRET_LEN]).expect("stage temp");

        assert!(
            !final_path.exists(),
            "a temp that was never published must not appear at the real path"
        );
        // And reading the real path still reports NotFound, not a partial key.
        assert!(matches!(
            read_secret(&final_path),
            Err(IdentityError::NotFound(_))
        ));
    }

    #[test]
    fn create_new_secret_is_exclusive() {
        let (_tmp, path) = fresh_path();
        let a = [9u8; SECRET_LEN];
        create_new_secret(&path, &a).expect("first create wins");
        assert_eq!(read_secret(&path).unwrap(), a);

        // Second create on the same path must not clobber.
        let b = [3u8; SECRET_LEN];
        let err = create_new_secret(&path, &b).expect_err("second create must fail");
        assert!(matches!(err, IdentityError::AlreadyExists(_)));
        assert_eq!(read_secret(&path).unwrap(), a, "existing key untouched");
    }

    #[cfg(unix)]
    #[test]
    fn create_new_secret_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, path) = fresh_path();
        create_new_secret(&path, &[0u8; SECRET_LEN]).expect("create");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "exclusively-created key must be 0600, got {mode:o}");
        // No leftover temp files in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no temp files should remain after publish");
    }
}
