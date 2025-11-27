//! Boot artifact storage and serving

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tracing::warn;

/// Metadata for a boot artifact
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub hash: String,
}

/// Manages boot artifact storage and retrieval
pub struct ArtifactStore {
    base_dir: PathBuf,
    /// Cache of computed hashes: (channel, arch, name) -> hash
    hash_cache: RwLock<HashMap<(String, String, String), String>>,
}

impl ArtifactStore {
    /// Create new artifact store with base directory
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

    /// Get path to artifact if it exists
    pub fn get_artifact_path(&self, channel: &str, arch: &str, name: &str) -> Option<PathBuf> {
        // Validate inputs to prevent directory traversal
        if !Self::is_valid_name(channel) || !Self::is_valid_name(arch) || !Self::is_valid_name(name) {
            warn!("Invalid artifact path components: {}/{}/{}", channel, arch, name);
            return None;
        }

        // Try the exact arch and also aliases (arm64 <-> aarch64)
        let arch_variants = Self::arch_aliases(arch);

        for arch_variant in &arch_variants {
            let path = self.base_dir.join(channel).join(arch_variant).join(name);
            if path.exists() && path.is_file() {
                return Some(path);
            }

            // Try common alternative artifact names
            let alternatives = Self::artifact_alternatives(name);
            for alt in alternatives {
                let alt_path = self.base_dir.join(channel).join(arch_variant).join(&alt);
                if alt_path.exists() && alt_path.is_file() {
                    return Some(alt_path);
                }
            }
        }
        None
    }

    /// Get artifact with metadata including hash
    pub fn get_artifact(&self, channel: &str, arch: &str, name: &str) -> Result<Option<ArtifactMeta>> {
        let path = match self.get_artifact_path(channel, arch, name) {
            Some(p) => p,
            None => return Ok(None),
        };

        let metadata = fs::metadata(&path)
            .with_context(|| format!("Failed to read metadata: {:?}", path))?;

        let hash = self.get_or_compute_hash(channel, arch, name, &path)?;

        Ok(Some(ArtifactMeta {
            name: name.to_string(),
            path,
            size_bytes: metadata.len(),
            hash,
        }))
    }

    /// List all artifacts for a channel/arch (tries arch aliases)
    pub fn list_artifacts(&self, channel: &str, arch: &str) -> Result<Vec<ArtifactMeta>> {
        // Try arch aliases (arm64 <-> aarch64)
        for arch_variant in Self::arch_aliases(arch) {
            let dir = self.base_dir.join(channel).join(arch_variant);
            if dir.exists() {
                let mut artifacts = Vec::new();
                for entry in fs::read_dir(&dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        let name = path.file_name()
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

    /// List available channels
    pub fn list_channels(&self) -> Result<Vec<String>> {
        let mut channels = Vec::new();
        if self.base_dir.exists() {
            for entry in fs::read_dir(&self.base_dir)? {
                let entry = entry?;
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        channels.push(name.to_string());
                    }
                }
            }
        }
        Ok(channels)
    }

    /// Compute SHA256 hash of file
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

    /// Get cached hash or compute it
    fn get_or_compute_hash(&self, channel: &str, arch: &str, name: &str, path: &Path) -> Result<String> {
        let key = (channel.to_string(), arch.to_string(), name.to_string());

        // Check cache first
        if let Ok(cache) = self.hash_cache.read() {
            if let Some(hash) = cache.get(&key) {
                return Ok(hash.clone());
            }
        }

        // Compute and cache
        let hash = Self::compute_hash(path)?;
        if let Ok(mut cache) = self.hash_cache.write() {
            cache.insert(key, hash.clone());
        }

        Ok(hash)
    }

    /// Validate name component (prevent directory traversal)
    fn is_valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.contains('\\')
            && !name.contains("..")
            && name != "."
    }

    /// Get alternative filenames for artifacts
    fn artifact_alternatives(name: &str) -> Vec<String> {
        match name {
            "kernel" => vec!["vmlinuz".to_string(), "vmlinuz-arm64".to_string(), "bzImage".to_string()],
            "initramfs" => vec!["initramfs.img".to_string(), "initrd".to_string(), "initramfs-arm64.img".to_string()],
            "rootfs" => vec!["rootfs.img".to_string(), "rootfs.squashfs".to_string()],
            _ => vec![],
        }
    }

    /// Get architecture aliases to try (arm64 <-> aarch64, amd64 <-> x86_64)
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

        // Create test artifact
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
        assert!(store.get_artifact_path("stable", "arm64", "../../../etc/passwd").is_none());
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
        // SHA256 of "hello world" is known
        assert_eq!(hash, "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }
}
