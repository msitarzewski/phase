// SPDX-License-Identifier: AGPL-3.0-or-later

//! Persistent operator-owned node preferences (`~/.config/lucidd/node.toml`).
//!
//! Keeps node-level configuration (approved model folders, pinned runtime
//! binary + SHA-256, explicit providing permissions, onboarding state) cleanly
//! separate from [`crate::policy::PolicyConfig`] (runtime serving/thermal/authz
//! rules) and [`crate::consumer::EnrollmentStore`] (delegated invitation
//! ledger).
//!
//! Security invariants:
//! - Default-off: `providing_enabled` is `false` and `approved_provide_models`
//!   is empty until the user explicitly grants permission.
//! - Private storage: directories are created `0700` and `node.toml` is
//!   written atomically with mode `0600`.
//! - Sandboxed folder allowlist: refuses root, system, device, and sensitive
//!   credential directories (`~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/phase`)
//!   before adding any model search folder.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const NODE_CONFIG_SCHEMA: u32 = 1;
const MAX_MODEL_DIRS: usize = 16;
const MAX_APPROVED_MODELS: usize = 256;
const MAX_BOOTSTRAP_PEERS: usize = 8;

/// Persistent user-approved node configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    pub schema: u32,
    /// True once the user has reviewed the initial machine capability probe
    /// and explicitly made their first choice (use-only or provide models).
    pub onboarding_completed: bool,
    /// Explicit user consent to share local compute with authorized peers.
    /// Always `false` on a brand-new machine.
    pub providing_enabled: bool,
    /// Explicit allowlist of model names/CIDs the user granted permission to
    /// provide to peers. Never implicitly populated by probing disk.
    pub approved_provide_models: Vec<String>,
    /// Explicit list of models the user pinned for local app availability.
    pub approved_local_models: Vec<String>,
    /// Canonicalized local directories the user explicitly permitted LUCID to
    /// scan read-only for `.gguf` files.
    pub model_dirs: Vec<PathBuf>,
    /// Canonical absolute path to an operator-approved `llama-server` binary.
    /// Runtime `PATH` lookup is never performed.
    pub llama_server_binary: Option<PathBuf>,
    /// Lowercase hex SHA-256 digest of `llama_server_binary`, verified before
    /// every launch so external tampering fails closed.
    pub llama_server_sha256: Option<String>,
    /// Explicit opt-in to serve verified weight chunks over P2P content plane.
    pub serve_model_content: bool,
    /// Optional custom relay/bootstrap multiaddrs (empty = use default
    /// `bootstrap.phasebased.net` DNS TXT discovery).
    pub bootstrap_peers: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            schema: NODE_CONFIG_SCHEMA,
            onboarding_completed: false,
            providing_enabled: false,
            approved_provide_models: Vec::new(),
            approved_local_models: Vec::new(),
            model_dirs: Vec::new(),
            llama_server_binary: None,
            llama_server_sha256: None,
            serve_model_content: false,
            bootstrap_peers: Vec::new(),
        }
    }
}

/// Resolve the default platform-native `node.toml` path
/// (`~/.config/lucidd/node.toml` on Linux, `~/Library/Application Support/lucidd/node.toml` on macOS).
pub fn default_node_config_path() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .context("could not resolve platform config directory for lucidd node.toml")?;
    Ok(base.join("lucidd").join("node.toml"))
}

/// Thread-safe, atomically-persisted store for [`NodeConfig`].
pub struct NodeConfigStore {
    path: Option<PathBuf>,
    config: RwLock<NodeConfig>,
}

impl NodeConfigStore {
    /// Load `node.toml` from `path` (or platform default if `None`), creating a
    /// safe default config in memory (without mutating disk until first update)
    /// if absent.
    pub fn load_or_default(path: Option<PathBuf>) -> Result<Arc<Self>> {
        let resolved = match path {
            Some(p) => p,
            None => default_node_config_path()?,
        };
        let config = if resolved.exists() {
            let raw = fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read {}", resolved.display()))?;
            let parsed: NodeConfig = toml::from_str(&raw)
                .with_context(|| format!("failed to parse {}", resolved.display()))?;
            if parsed.schema != NODE_CONFIG_SCHEMA {
                bail!(
                    "unsupported node.toml schema {} (expected {})",
                    parsed.schema,
                    NODE_CONFIG_SCHEMA
                );
            }
            parsed
        } else {
            NodeConfig::default()
        };
        Ok(Arc::new(Self {
            path: Some(resolved),
            config: RwLock::new(config),
        }))
    }

    /// Ephemeral in-memory store for unit tests and isolated fixtures.
    pub fn in_memory(config: NodeConfig) -> Arc<Self> {
        Arc::new(Self {
            path: None,
            config: RwLock::new(config),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn snapshot(&self) -> NodeConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Apply a validated mutation and atomically persist `node.toml` (`0600`)
    /// before swapping the in-memory copy.
    pub fn update<F>(&self, mutate: F) -> Result<NodeConfig>
    where
        F: FnOnce(&mut NodeConfig) -> Result<()>,
    {
        let mut guard = self.config.write().unwrap_or_else(|e| e.into_inner());
        let mut next = guard.clone();
        mutate(&mut next)?;
        validate_node_config(&next)?;
        if let Some(path) = &self.path {
            write_private_toml(path, &next)?;
        }
        *guard = next.clone();
        Ok(next)
    }
}

/// Validate a candidate user-selected model directory against filesystem
/// traversal and sensitive OS/credential locations.
pub fn validate_model_dir(candidate: &Path) -> Result<PathBuf> {
    if !candidate.is_absolute() {
        bail!("model folder must be an absolute path");
    }
    for component in candidate.components() {
        if matches!(component, Component::ParentDir) {
            bail!("model folder path must not contain '..'");
        }
    }
    let meta = fs::symlink_metadata(candidate)
        .with_context(|| format!("cannot inspect model folder {}", candidate.display()))?;
    if meta.file_type().is_symlink() {
        bail!("model folder must be a real directory, not a symlink");
    }
    if !meta.is_dir() {
        bail!("model folder must be an existing directory");
    }
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("cannot canonicalize {}", candidate.display()))?;

    if is_forbidden_model_dir(&canonical) {
        bail!(
            "refusing to add sensitive or system directory as a model folder: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

fn is_forbidden_model_dir(path: &Path) -> bool {
    if path.parent().is_none() {
        return true; // Root directory `/`
    }
    let forbidden_prefixes = [
        "/etc",
        "/private/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/proc",
        "/sys",
        "/dev",
        "/boot",
        "/root",
    ];
    for prefix in forbidden_prefixes {
        if path == Path::new(prefix) || path.starts_with(prefix) {
            return true;
        }
    }
    if let Some(home) = dirs::home_dir().and_then(|h| h.canonicalize().ok()) {
        if path == home {
            return true; // Never scan the entire home directory root.
        }
        let sensitive_home_dirs = [
            ".ssh",
            ".gnupg",
            ".aws",
            ".kube",
            ".docker",
            ".config/phase",
            ".config/lucidd",
            "Library/Keychains",
            "Library/Application Support/LUCID",
            "Library/Application Support/phase",
        ];
        for rel in sensitive_home_dirs {
            let sensitive = home.join(rel);
            if path == sensitive || path.starts_with(&sensitive) {
                return true;
            }
        }
    }
    false
}

fn validate_node_config(cfg: &NodeConfig) -> Result<()> {
    if cfg.schema != NODE_CONFIG_SCHEMA {
        bail!("invalid node.toml schema {}", cfg.schema);
    }
    if cfg.model_dirs.len() > MAX_MODEL_DIRS {
        bail!("at most {MAX_MODEL_DIRS} model folders may be configured");
    }
    if cfg.approved_provide_models.len() > MAX_APPROVED_MODELS
        || cfg.approved_local_models.len() > MAX_APPROVED_MODELS
    {
        bail!("model allowlist exceeds maximum of {MAX_APPROVED_MODELS} entries");
    }
    if cfg.bootstrap_peers.len() > MAX_BOOTSTRAP_PEERS {
        bail!("at most {MAX_BOOTSTRAP_PEERS} bootstrap peers may be configured");
    }
    if let Some(sha) = &cfg.llama_server_sha256 {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("llama_server_sha256 must be a 64-character hex SHA-256 digest");
        }
    }
    if let Some(bin) = &cfg.llama_server_binary {
        if !bin.is_absolute() {
            bail!("llama_server_binary must be an absolute path");
        }
    }
    Ok(())
}

pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set 0700 on {}", dir.display()))?;
    }
    Ok(())
}

fn write_private_toml(path: &Path, cfg: &NodeConfig) -> Result<()> {
    let parent = path
        .parent()
        .context("node config path has no parent directory")?;
    ensure_private_dir(parent)?;
    let serialized = toml::to_string_pretty(cfg).context("failed to serialize node.toml")?;
    let tmp_name = format!(".node-{}.tmp", uuid::Uuid::new_v4().simple());
    let tmp_path = parent.join(tmp_name);
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp_path)
            .with_context(|| format!("failed to create temp file {}", tmp_path.display()))?;
        file.write_all(serialized.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path).with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config_is_strictly_opt_in() {
        let cfg = NodeConfig::default();
        assert!(!cfg.onboarding_completed);
        assert!(!cfg.providing_enabled);
        assert!(cfg.approved_provide_models.is_empty());
        assert!(!cfg.serve_model_content);
        assert!(cfg.llama_server_binary.is_none());
    }

    #[test]
    fn store_roundtrips_privately_and_enforces_validation() {
        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("lucidd").join("node.toml");
        let store = NodeConfigStore::load_or_default(Some(cfg_path.clone())).unwrap();
        assert!(!cfg_path.exists());

        let updated = store
            .update(|cfg| {
                cfg.onboarding_completed = true;
                cfg.providing_enabled = true;
                cfg.approved_provide_models.push("qwen2.5:7b".to_string());
                Ok(())
            })
            .unwrap();
        assert!(updated.onboarding_completed);
        assert!(cfg_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let reloaded = NodeConfigStore::load_or_default(Some(cfg_path)).unwrap();
        assert_eq!(reloaded.snapshot(), updated);

        // Reject malformed SHA-256 digest without mutating persisted state.
        let err = reloaded.update(|cfg| {
            cfg.llama_server_sha256 = Some("not-a-sha".to_string());
            Ok(())
        });
        assert!(err.is_err());
        assert!(reloaded.snapshot().llama_server_sha256.is_none());
    }

    #[test]
    fn validate_model_dir_blocks_system_and_relative_paths() {
        assert!(validate_model_dir(Path::new("relative/models")).is_err());
        assert!(validate_model_dir(Path::new("/")).is_err());
        assert!(validate_model_dir(Path::new("/etc")).is_err());

        let dir = TempDir::new().unwrap();
        let valid = validate_model_dir(dir.path()).unwrap();
        assert_eq!(valid, dir.path().canonicalize().unwrap());
    }
}
