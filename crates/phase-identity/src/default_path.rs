// SPDX-License-Identifier: Apache-2.0

//! Platform-aware default location for the node identity file.
//!
//! Resolves via `dirs::config_dir()`, which yields the standard per-platform
//! configuration directory:
//!
//! - Linux:   `$XDG_CONFIG_HOME/phase/identity.key`
//!   (defaults to `~/.config/phase/identity.key`)
//! - macOS:   `~/Library/Application Support/phase/identity.key`
//! - Windows: `%APPDATA%\phase\identity.key`
//!
//! Callers that want a different location should construct a `PathBuf`
//! directly and pass it to `NodeIdentity::load_or_create`.

use std::path::PathBuf;

use crate::error::IdentityError;

/// Subdirectory under the platform config dir that holds Phase config.
const PHASE_CONFIG_SUBDIR: &str = "phase";

/// Filename for the persistent identity secret.
const IDENTITY_FILENAME: &str = "identity.key";

/// Resolve the default platform-appropriate path for the node identity
/// file. Returns `IdentityError::NoConfigDir` when no config directory can
/// be determined (typically because the user has no home directory).
pub fn default_identity_path() -> Result<PathBuf, IdentityError> {
    let base = dirs::config_dir().ok_or(IdentityError::NoConfigDir)?;
    Ok(base.join(PHASE_CONFIG_SUBDIR).join(IDENTITY_FILENAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_ends_with_phase_identity_key() {
        // Skip the test if the environment has no config dir at all (rare,
        // typically only in heavily sandboxed CI containers).
        let path = match default_identity_path() {
            Ok(p) => p,
            Err(IdentityError::NoConfigDir) => return,
            Err(e) => panic!("unexpected error resolving default path: {e}"),
        };

        // Regardless of platform, the last two components must be the
        // phase subdirectory and the identity.key filename.
        let components: Vec<_> = path
            .components()
            .rev()
            .take(2)
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();

        assert_eq!(components[0], IDENTITY_FILENAME);
        assert_eq!(components[1], PHASE_CONFIG_SUBDIR);
    }
}
