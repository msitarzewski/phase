# Task 2 — Hash Computation

**Agent**: Security Agent
**Estimated**: 1 day

## 2.1 Implement SHA256 hashing

- [ ] Create `daemon/src/provider/hash.rs`:
  ```rust
  use sha2::{Sha256, Digest};
  use std::path::Path;
  use tokio::fs::File;
  use tokio::io::AsyncReadExt;

  /// Compute SHA256 hash of a file
  pub async fn compute_sha256(path: &Path) -> Result<String, HashError> {
      let mut file = File::open(path).await
          .map_err(|e| HashError::IoError(e.to_string()))?;

      let mut hasher = Sha256::new();
      let mut buffer = vec![0u8; 64 * 1024];  // 64KB buffer

      loop {
          let bytes_read = file.read(&mut buffer).await
              .map_err(|e| HashError::IoError(e.to_string()))?;

          if bytes_read == 0 {
              break;
          }

          hasher.update(&buffer[..bytes_read]);
      }

      let hash = hasher.finalize();
      Ok(format!("sha256:{}", hex::encode(hash)))
  }

  /// Verify a file matches expected hash
  pub async fn verify_hash(path: &Path, expected: &str) -> Result<bool, HashError> {
      let actual = compute_sha256(path).await?;
      Ok(actual == expected)
  }

  #[derive(Debug, thiserror::Error)]
  pub enum HashError {
      #[error("IO error: {0}")]
      IoError(String),
  }
  ```

**Dependencies**: M2/Task 1
**Output**: Hash computation module

---

## 2.2 Add hex dependency

- [ ] Update `daemon/Cargo.toml`:
  ```toml
  [dependencies]
  hex = "0.4"
  # sha2 should already be present for ed25519
  ```

**Dependencies**: None
**Output**: Hex crate added

---

## 2.3 Implement hash caching

- [ ] Add hash cache to ProviderState:
  ```rust
  use std::collections::HashMap;
  use std::time::SystemTime;

  #[derive(Debug, Clone)]
  pub struct CachedHash {
      pub hash: String,
      pub size: u64,
      pub mtime: SystemTime,
  }

  // In ProviderState
  pub struct ProviderState {
      // ... existing fields
      pub hash_cache: HashMap<PathBuf, CachedHash>,
  }

  impl ProviderState {
      /// Get hash for artifact, computing if not cached or stale
      pub async fn get_artifact_hash(&mut self, path: &Path) -> Result<CachedHash, HashError> {
          // Check cache
          if let Some(cached) = self.hash_cache.get(path) {
              // Verify file hasn't changed
              let metadata = tokio::fs::metadata(path).await
                  .map_err(|e| HashError::IoError(e.to_string()))?;

              if let Ok(mtime) = metadata.modified() {
                  if mtime == cached.mtime {
                      return Ok(cached.clone());
                  }
              }
          }

          // Compute hash
          let hash = compute_sha256(path).await?;
          let metadata = tokio::fs::metadata(path).await
              .map_err(|e| HashError::IoError(e.to_string()))?;

          let cached = CachedHash {
              hash,
              size: metadata.len(),
              mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
          };

          self.hash_cache.insert(path.to_path_buf(), cached.clone());
          Ok(cached)
      }
  }
  ```

**Dependencies**: Task 2.1
**Output**: Hash caching

---

## 2.4 Implement batch hash computation

- [ ] Add method to hash all artifacts:
  ```rust
  impl ProviderState {
      /// Compute hashes for all artifacts in a channel/arch
      pub async fn compute_all_hashes(
          &mut self,
          channel: &str,
          arch: &str,
      ) -> Result<HashMap<String, CachedHash>, HashError> {
          let base = self.config.artifacts_dir.join(channel).join(arch);
          let mut results = HashMap::new();

          let artifacts = [
              ("kernel", "vmlinuz"),
              ("initramfs", "initramfs.img"),
              ("rootfs", "rootfs.sqfs"),
          ];

          for (name, filename) in artifacts {
              let path = base.join(filename);
              if path.exists() {
                  let cached = self.get_artifact_hash(&path).await?;
                  results.insert(name.to_string(), cached);
              }
          }

          Ok(results)
      }
  }
  ```

**Dependencies**: Task 2.3
**Output**: Batch hashing

---

## 2.5 Add progress reporting for large files

- [ ] For large files (rootfs), report progress:
  ```rust
  pub async fn compute_sha256_with_progress<F>(
      path: &Path,
      progress_callback: F,
  ) -> Result<String, HashError>
  where
      F: Fn(u64, u64),  // (bytes_processed, total_bytes)
  {
      let metadata = tokio::fs::metadata(path).await
          .map_err(|e| HashError::IoError(e.to_string()))?;
      let total = metadata.len();

      let mut file = File::open(path).await
          .map_err(|e| HashError::IoError(e.to_string()))?;

      let mut hasher = Sha256::new();
      let mut buffer = vec![0u8; 1024 * 1024];  // 1MB buffer for large files
      let mut processed = 0u64;

      loop {
          let bytes_read = file.read(&mut buffer).await
              .map_err(|e| HashError::IoError(e.to_string()))?;

          if bytes_read == 0 {
              break;
          }

          hasher.update(&buffer[..bytes_read]);
          processed += bytes_read as u64;
          progress_callback(processed, total);
      }

      let hash = hasher.finalize();
      Ok(format!("sha256:{}", hex::encode(hash)))
  }
  ```

**Dependencies**: Task 2.4
**Output**: Progress reporting

---

## 2.6 Test hash computation

- [ ] Create tests:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use tempfile::NamedTempFile;
      use std::io::Write;

      #[tokio::test]
      async fn test_compute_sha256() {
          let mut file = NamedTempFile::new().unwrap();
          file.write_all(b"hello world").unwrap();

          let hash = compute_sha256(file.path()).await.unwrap();

          // Known SHA256 of "hello world"
          assert_eq!(
              hash,
              "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
          );
      }

      #[tokio::test]
      async fn test_verify_hash() {
          let mut file = NamedTempFile::new().unwrap();
          file.write_all(b"test data").unwrap();

          let hash = compute_sha256(file.path()).await.unwrap();
          assert!(verify_hash(file.path(), &hash).await.unwrap());
          assert!(!verify_hash(file.path(), "sha256:invalid").await.unwrap());
      }
  }
  ```

**Dependencies**: Task 2.5
**Output**: Hash tests

---

## Validation Checklist

- [ ] SHA256 computation produces correct hashes
- [ ] Hash format is "sha256:<64 hex chars>"
- [ ] Large files (500MB+) hash without memory issues
- [ ] Hash caching works (subsequent calls faster)
- [ ] Cache invalidates when file changes
- [ ] Progress callback works for large files
- [ ] All tests pass
