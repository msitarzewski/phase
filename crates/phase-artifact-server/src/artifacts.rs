// SPDX-License-Identifier: Apache-2.0

//! Artifact storage and retrieval.
//!
//! Supports two on-disk layouts under the same `artifacts_dir` root:
//!
//! 1. **Channel/arch layout** — `<channel>/<arch>/<filename>`. The legacy
//!    Phase Boot layout, preserved byte-for-byte so existing tooling
//!    (phase-discover, phase-fetch, phase-verify, USB images) keeps working.
//! 2. **Content-addressed layout** — `blobs/<aa>/<full_hex>.bin` where
//!    `<full_hex>` is the lowercase hex SHA-256 of the blob contents and
//!    `<aa>` is the first two hex characters. Added in M6 so the server can
//!    distribute any blob keyed by its hash, independent of channel/arch
//!    semantics.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tracing::warn;

/// Content-address for a blob. The wire form is the lowercase hex
/// SHA-256 of the blob's contents — no `sha256:` prefix because the
/// algorithm is implicit in the path layout.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(String);

impl BlobId {
    /// Compute the [`BlobId`] of `content` by hashing.
    pub fn from_content(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        Self(hex::encode(hasher.finalize()))
    }

    /// Build a [`BlobId`] from an already-computed hex digest. Returns
    /// `None` if the input is not a 64-character lowercase hex string.
    pub fn from_hex(hex_str: &str) -> Option<Self> {
        if hex_str.len() == 64 && hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(Self(hex_str.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// View the blob id as its hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The two-character prefix used as the bucket directory name.
    pub fn prefix(&self) -> &str {
        &self.0[..2]
    }

    /// Relative path of this blob under the artifacts root:
    /// `blobs/<aa>/<full_hex>.bin`.
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from("blobs").join(self.prefix()).join(format!("{}.bin", self.0))
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Metadata for an artifact on disk.
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// SHA-256 hash in the legacy `"sha256:<hexdigest>"` wire format used by
    /// the channel/arch path. Blob-id-keyed artifacts also populate this
    /// field for consistency.
    pub hash: String,
}

/// Manages artifact storage and retrieval across both the channel/arch and
/// content-addressed layouts.
#[derive(Debug)]
pub struct ArtifactStore {
    base_dir: PathBuf,
    /// Cache of computed hashes for channel/arch-keyed lookups:
    /// `(channel, arch, name) -> "sha256:<hex>"`.
    hash_cache: RwLock<HashMap<(String, String, String), String>>,
}

impl ArtifactStore {
    /// Create a store rooted at `base_dir`. Creates the directory if needed.
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        if !base_dir.exists() {
            fs::create_dir_all(&base_dir)
                .with_context(|| format!("Failed to create artifacts dir: {:?}", base_dir))?;
        }

        Ok(Self {
            base_dir,
            hash_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Root directory of this store.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    // ------------------------------------------------------------------
    // Channel / arch layout (legacy, byte-identical to pre-M6)
    // ------------------------------------------------------------------

    /// Resolve the on-disk path for an artifact in the channel/arch layout.
    ///
    /// Honours arch aliases (`arm64` <-> `aarch64`, `amd64` <-> `x86_64`)
    /// and well-known filename alternatives (`kernel` -> `vmlinuz`,
    /// `bzImage`, etc.).
    pub fn get_artifact_path(&self, channel: &str, arch: &str, name: &str) -> Option<PathBuf> {
        if !Self::is_valid_name(channel) || !Self::is_valid_name(arch) || !Self::is_valid_name(name)
        {
            warn!("Invalid artifact path components: {}/{}/{}", channel, arch, name);
            return None;
        }

        let arch_variants = Self::arch_aliases(arch);

        for arch_variant in &arch_variants {
            let path = self.base_dir.join(channel).join(arch_variant).join(name);
            if path.exists() && path.is_file() {
                return Some(path);
            }

            for alt in Self::artifact_alternatives(name) {
                let alt_path = self.base_dir.join(channel).join(arch_variant).join(&alt);
                if alt_path.exists() && alt_path.is_file() {
                    return Some(alt_path);
                }
            }
        }
        None
    }

    /// Look up an artifact in the channel/arch layout, returning metadata
    /// (size + cached hash). Returns `Ok(None)` when nothing exists at any
    /// of the path / alias / alternative combinations.
    pub fn get_artifact(
        &self,
        channel: &str,
        arch: &str,
        name: &str,
    ) -> Result<Option<ArtifactMeta>> {
        let path = match self.get_artifact_path(channel, arch, name) {
            Some(p) => p,
            None => return Ok(None),
        };

        let metadata =
            fs::metadata(&path).with_context(|| format!("Failed to read metadata: {:?}", path))?;

        let hash = self.get_or_compute_hash(channel, arch, name, &path)?;

        Ok(Some(ArtifactMeta {
            name: name.to_string(),
            path,
            size_bytes: metadata.len(),
            hash,
        }))
    }

    /// List every artifact present for a `(channel, arch)` pair. Honours
    /// arch aliases.
    pub fn list_artifacts(&self, channel: &str, arch: &str) -> Result<Vec<ArtifactMeta>> {
        for arch_variant in Self::arch_aliases(arch) {
            let dir = self.base_dir.join(channel).join(arch_variant);
            if dir.exists() {
                let mut artifacts = Vec::new();
                for entry in fs::read_dir(&dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let metadata = fs::metadata(&path)?;
                        let hash = self.get_or_compute_hash(channel, arch_variant, &name, &path)?;

                        artifacts.push(ArtifactMeta {
                            name,
                            path,
                            size_bytes: metadata.len(),
                            hash,
                        });
                    }
                }
                return Ok(artifacts);
            }
        }

        Ok(Vec::new())
    }

    /// List all channel directories under the base dir.
    pub fn list_channels(&self) -> Result<Vec<String>> {
        let mut channels = Vec::new();
        if self.base_dir.exists() {
            for entry in fs::read_dir(&self.base_dir)? {
                let entry = entry?;
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        // Skip the content-addressed bucket — it is not a channel.
                        if name == "blobs" {
                            continue;
                        }
                        channels.push(name.to_string());
                    }
                }
            }
        }
        Ok(channels)
    }

    /// Write `content` into the channel/arch layout at
    /// `<base>/<channel>/<arch>/<filename>`. Creates parent directories as
    /// needed.
    pub fn add_channel_artifact(
        &self,
        channel: &str,
        arch: &str,
        filename: &str,
        content: &[u8],
    ) -> Result<ArtifactMeta> {
        if !Self::is_valid_name(channel)
            || !Self::is_valid_name(arch)
            || !Self::is_valid_name(filename)
        {
            anyhow::bail!(
                "invalid channel/arch/filename: {}/{}/{}",
                channel,
                arch,
                filename
            );
        }
        let dir = self.base_dir.join(channel).join(arch);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create channel artifact dir {:?}", dir))?;
        let path = dir.join(filename);
        fs::write(&path, content)
            .with_context(|| format!("write channel artifact {:?}", path))?;

        let hash = format!("sha256:{}", BlobId::from_content(content).as_str());
        if let Ok(mut cache) = self.hash_cache.write() {
            cache.insert(
                (channel.to_string(), arch.to_string(), filename.to_string()),
                hash.clone(),
            );
        }

        Ok(ArtifactMeta {
            name: filename.to_string(),
            path,
            size_bytes: content.len() as u64,
            hash,
        })
    }

    // ------------------------------------------------------------------
    // Content-addressed (blob-id) layout
    // ------------------------------------------------------------------

    /// Write `content` into the blob layout under
    /// `blobs/<aa>/<full_hex>.bin` and return its [`BlobId`]. If a blob
    /// with the same id already exists, the write is skipped (content is
    /// guaranteed to match its hash).
    pub fn add_blob(&self, content: &[u8]) -> Result<BlobId> {
        let id = BlobId::from_content(content);
        let path = self.base_dir.join(id.relative_path());
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create blob bucket dir {:?}", parent))?;
            }
            fs::write(&path, content).with_context(|| format!("write blob {:?}", path))?;
        }
        Ok(id)
    }

    /// On-disk path for a content-addressed blob.
    pub fn get_blob_path(&self, id: &BlobId) -> Option<PathBuf> {
        let path = self.base_dir.join(id.relative_path());
        if path.exists() && path.is_file() {
            Some(path)
        } else {
            None
        }
    }

    /// Metadata for a content-addressed blob.
    pub fn get_blob(&self, id: &BlobId) -> Result<Option<ArtifactMeta>> {
        let path = match self.get_blob_path(id) {
            Some(p) => p,
            None => return Ok(None),
        };
        let metadata =
            fs::metadata(&path).with_context(|| format!("Failed to read metadata: {:?}", path))?;
        Ok(Some(ArtifactMeta {
            name: format!("{}.bin", id.as_str()),
            path,
            size_bytes: metadata.len(),
            hash: format!("sha256:{}", id.as_str()),
        }))
    }

    // ------------------------------------------------------------------
    // Hashing helpers
    // ------------------------------------------------------------------

    /// Streaming SHA-256 of a file. Returns `"sha256:<hexdigest>"`.
    pub fn compute_hash(path: &Path) -> Result<String> {
        let mut file = fs::File::open(path)
            .with_context(|| format!("Failed to open file for hashing: {:?}", path))?;

        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let hash = hasher.finalize();
        Ok(format!("sha256:{}", hex::encode(hash)))
    }

    fn get_or_compute_hash(
        &self,
        channel: &str,
        arch: &str,
        name: &str,
        path: &Path,
    ) -> Result<String> {
        let key = (channel.to_string(), arch.to_string(), name.to_string());

        if let Ok(cache) = self.hash_cache.read() {
            if let Some(hash) = cache.get(&key) {
                return Ok(hash.clone());
            }
        }

        let hash = Self::compute_hash(path)?;
        if let Ok(mut cache) = self.hash_cache.write() {
            cache.insert(key, hash.clone());
        }

        Ok(hash)
    }

    // ------------------------------------------------------------------
    // Path-component validation
    // ------------------------------------------------------------------

    fn is_valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.contains('\\')
            && !name.contains("..")
            && name != "."
    }

    fn artifact_alternatives(name: &str) -> Vec<String> {
        match name {
            "kernel" => vec![
                "vmlinuz".to_string(),
                "vmlinuz-arm64".to_string(),
                "bzImage".to_string(),
            ],
            "initramfs" => vec![
                "initramfs.img".to_string(),
                "initrd".to_string(),
                "initramfs-arm64.img".to_string(),
            ],
            "rootfs" => vec!["rootfs.img".to_string(), "rootfs.squashfs".to_string()],
            _ => vec![],
        }
    }

    fn arch_aliases(arch: &str) -> Vec<&str> {
        match arch {
            "aarch64" => vec!["aarch64", "arm64"],
            "arm64" => vec!["arm64", "aarch64"],
            "x86_64" => vec!["x86_64", "amd64"],
            "amd64" => vec!["amd64", "x86_64"],
            other => vec![other],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_store() -> (TempDir, ArtifactStore) {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf()).unwrap();
        (temp, store)
    }

    #[test]
    fn test_artifact_store_new() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf());
        assert!(store.is_ok());
    }

    #[test]
    fn test_get_artifact_not_found() {
        let (_temp, store) = setup_test_store();
        let result = store.get_artifact("stable", "arm64", "kernel").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_artifact_found() {
        let (temp, store) = setup_test_store();

        let artifact_dir = temp.path().join("stable").join("arm64");
        fs::create_dir_all(&artifact_dir).unwrap();
        let kernel_path = artifact_dir.join("kernel");
        fs::write(&kernel_path, b"test kernel content").unwrap();

        let result = store.get_artifact("stable", "arm64", "kernel").unwrap();
        assert!(result.is_some());
        let meta = result.unwrap();
        assert_eq!(meta.name, "kernel");
        assert_eq!(meta.size_bytes, 19);
        assert!(meta.hash.starts_with("sha256:"));
    }

    #[test]
    fn test_path_traversal_prevention() {
        let (_temp, store) = setup_test_store();
        assert!(store.get_artifact_path("../etc", "passwd", "file").is_none());
        assert!(store
            .get_artifact_path("stable", "arm64", "../../../etc/passwd")
            .is_none());
    }

    #[test]
    fn test_list_artifacts() {
        let (temp, store) = setup_test_store();

        let artifact_dir = temp.path().join("stable").join("arm64");
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(artifact_dir.join("kernel"), b"kernel").unwrap();
        fs::write(artifact_dir.join("initramfs"), b"initramfs").unwrap();

        let artifacts = store.list_artifacts("stable", "arm64").unwrap();
        assert_eq!(artifacts.len(), 2);
    }

    #[test]
    fn test_compute_hash() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.bin");
        fs::write(&file_path, b"hello world").unwrap();

        let hash = ArtifactStore::compute_hash(&file_path).unwrap();
        assert!(hash.starts_with("sha256:"));
        assert_eq!(
            hash,
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    // ------------------------------------------------------------------
    // Blob-id layout
    // ------------------------------------------------------------------

    #[test]
    fn test_blob_id_from_content_is_sha256() {
        let id = BlobId::from_content(b"hello world");
        assert_eq!(
            id.as_str(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(id.prefix(), "b9");
    }

    #[test]
    fn test_blob_id_relative_path_layout() {
        let id = BlobId::from_content(b"hello world");
        let rel = id.relative_path();
        let rel_str = rel.to_string_lossy();
        assert!(rel_str.starts_with("blobs/b9/"));
        assert!(rel_str.ends_with(
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9.bin"
        ));
    }

    #[test]
    fn test_blob_id_from_hex_rejects_invalid() {
        assert!(BlobId::from_hex("not-hex").is_none());
        assert!(BlobId::from_hex("abcdef").is_none()); // too short
        let valid = BlobId::from_hex(
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
        )
        .unwrap();
        assert_eq!(valid.prefix(), "b9");
    }

    #[test]
    fn test_add_blob_and_get_blob_roundtrip() {
        let (temp, store) = setup_test_store();
        let id = store.add_blob(b"hello world").unwrap();
        assert_eq!(
            id.as_str(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        // File written under blobs/b9/<hex>.bin
        let blob_path = temp.path().join(id.relative_path());
        assert!(blob_path.exists());

        // Metadata roundtrip
        let meta = store.get_blob(&id).unwrap().unwrap();
        assert_eq!(meta.size_bytes, 11);
        assert_eq!(meta.hash, format!("sha256:{}", id.as_str()));
    }

    #[test]
    fn test_add_blob_is_idempotent_for_same_content() {
        let (_temp, store) = setup_test_store();
        let a = store.add_blob(b"same content").unwrap();
        let b = store.add_blob(b"same content").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_list_channels_excludes_blobs_bucket() {
        let (temp, store) = setup_test_store();
        fs::create_dir_all(temp.path().join("stable").join("x86_64")).unwrap();
        fs::create_dir_all(temp.path().join("blobs").join("ab")).unwrap();
        let channels = store.list_channels().unwrap();
        assert!(channels.contains(&"stable".to_string()));
        assert!(!channels.contains(&"blobs".to_string()));
    }

    #[test]
    fn test_add_channel_artifact_writes_and_caches_hash() {
        let (temp, store) = setup_test_store();
        let meta = store
            .add_channel_artifact("stable", "x86_64", "kernel", b"kernel-bytes")
            .unwrap();
        assert_eq!(meta.size_bytes, 12);
        assert!(meta.hash.starts_with("sha256:"));
        // File should be at <base>/stable/x86_64/kernel
        let path = temp.path().join("stable").join("x86_64").join("kernel");
        assert!(path.exists());
    }
}
