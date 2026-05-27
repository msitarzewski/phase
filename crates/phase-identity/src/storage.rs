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

/// Persist `secret` to `path`. Creates parent directories if missing. On
/// Unix the file is created with mode `0o600` (owner read/write only).
pub(crate) fn write_secret(path: &Path, secret: &[u8; SECRET_LEN]) -> Result<(), IdentityError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| IdentityError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    fs::write(path, secret).map_err(|e| IdentityError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| IdentityError::Io {
                path: path.to_path_buf(),
                source: e,
            })?
            .permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms).map_err(|e| IdentityError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }

    Ok(())
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
}
