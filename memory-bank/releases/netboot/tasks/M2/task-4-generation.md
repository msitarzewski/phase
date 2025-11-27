# Task 4 — Manifest Generation

**Agent**: Security Agent
**Estimated**: 2 days

## 4.1 Implement manifest builder

- [ ] Add builder pattern to `manifest.rs`:
  ```rust
  pub struct ManifestBuilder {
      version: String,
      channel: String,
      arch: String,
      artifacts: ArtifactMap,
      provider: Option<ProviderInfo>,
      expires_in_days: u32,
  }

  impl ManifestBuilder {
      pub fn new(channel: &str, arch: &str) -> Self {
          Self {
              version: chrono::Utc::now().format("%Y.%m.%d").to_string(),
              channel: channel.to_string(),
              arch: arch.to_string(),
              artifacts: HashMap::new(),
              provider: None,
              expires_in_days: 30,
          }
      }

      pub fn version(mut self, version: &str) -> Self {
          self.version = version.to_string();
          self
      }

      pub fn expires_in_days(mut self, days: u32) -> Self {
          self.expires_in_days = days;
          self
      }

      pub fn provider(mut self, peer_id: &str, name: Option<&str>) -> Self {
          self.provider = Some(ProviderInfo {
              peer_id: peer_id.to_string(),
              name: name.map(|s| s.to_string()),
          });
          self
      }

      pub fn artifact(mut self, name: &str, hash: &str, size: u64, path: &str) -> Self {
          self.artifacts.insert(name.to_string(), Artifact {
              hash: hash.to_string(),
              size,
              path: path.to_string(),
          });
          self
      }

      pub fn build(self) -> BootManifest {
          let now = chrono::Utc::now();
          let expires = now + chrono::Duration::days(self.expires_in_days as i64);

          BootManifest {
              manifest_version: 1,
              version: self.version,
              channel: self.channel,
              arch: self.arch,
              created_at: now.to_rfc3339(),
              expires_at: expires.to_rfc3339(),
              artifacts: self.artifacts,
              signatures: Vec::new(),
              provider: self.provider,
          }
      }
  }
  ```

**Dependencies**: M2/Task 1
**Output**: Manifest builder

---

## 4.2 Generate manifest from artifact directory

- [ ] Add to ProviderState:
  ```rust
  impl ProviderState {
      /// Generate manifest for a channel/arch from artifact directory
      pub async fn generate_manifest(
          &mut self,
          channel: &str,
          arch: &str,
      ) -> Result<BootManifest, ManifestGenerationError> {
          // Compute hashes for all artifacts
          let hashes = self.compute_all_hashes(channel, arch).await
              .map_err(ManifestGenerationError::HashError)?;

          if hashes.is_empty() {
              return Err(ManifestGenerationError::NoArtifacts);
          }

          // Build manifest
          let mut builder = ManifestBuilder::new(channel, arch)
              .provider(&self.peer_id, self.config.provider_name.as_deref());

          for (name, cached) in hashes {
              let path = format!("/{}/{}/{}", channel, arch, name);
              builder = builder.artifact(&name, &cached.hash, cached.size, &path);
          }

          let mut manifest = builder.build();

          // Sign manifest
          sign_manifest(&mut manifest, &self.signing_key)
              .map_err(ManifestGenerationError::SigningError)?;

          Ok(manifest)
      }
  }

  #[derive(Debug, thiserror::Error)]
  pub enum ManifestGenerationError {
      #[error("Hash computation error: {0}")]
      HashError(#[from] HashError),

      #[error("No artifacts found in directory")]
      NoArtifacts,

      #[error("Signing error: {0}")]
      SigningError(#[from] SigningError),
  }
  ```

**Dependencies**: M2/Tasks 2, 3
**Output**: Manifest generation from directory

---

## 4.3 Cache generated manifests

- [ ] Add manifest cache:
  ```rust
  pub struct ProviderState {
      // ... existing fields
      pub manifest_cache: HashMap<(String, String), CachedManifest>,
  }

  #[derive(Debug, Clone)]
  pub struct CachedManifest {
      pub manifest: BootManifest,
      pub json: String,
      pub generated_at: std::time::Instant,
  }

  impl ProviderState {
      /// Get or generate manifest for channel/arch
      pub async fn get_manifest(
          &mut self,
          channel: &str,
          arch: &str,
          max_age_secs: u64,
      ) -> Result<&CachedManifest, ManifestGenerationError> {
          let key = (channel.to_string(), arch.to_string());

          // Check cache
          let needs_refresh = match self.manifest_cache.get(&key) {
              Some(cached) => {
                  cached.generated_at.elapsed().as_secs() > max_age_secs
              }
              None => true,
          };

          if needs_refresh {
              let manifest = self.generate_manifest(channel, arch).await?;
              let json = manifest.to_json()
                  .map_err(|e| ManifestGenerationError::SerializationError(e.to_string()))?;

              self.manifest_cache.insert(key.clone(), CachedManifest {
                  manifest,
                  json,
                  generated_at: std::time::Instant::now(),
              });
          }

          Ok(self.manifest_cache.get(&key).unwrap())
      }
  }
  ```

**Dependencies**: Task 4.2
**Output**: Manifest caching

---

## 4.4 Add auto-refresh on file change

- [ ] Option: Use notify crate for filesystem watching
- [ ] Simpler: Invalidate cache when hash changes detected
  ```rust
  impl ProviderState {
      /// Invalidate manifest cache for channel/arch
      pub fn invalidate_manifest(&mut self, channel: &str, arch: &str) {
          let key = (channel.to_string(), arch.to_string());
          self.manifest_cache.remove(&key);
      }

      /// Check if artifacts have changed since manifest was generated
      pub async fn check_artifacts_changed(
          &self,
          channel: &str,
          arch: &str,
      ) -> bool {
          let key = (channel.to_string(), arch.to_string());

          if let Some(cached) = self.manifest_cache.get(&key) {
              // Re-check file mtimes
              for (name, artifact) in &cached.manifest.artifacts {
                  let path = self.config.artifacts_dir
                      .join(channel)
                      .join(arch)
                      .join(artifact_filename(name));

                  if let Ok(metadata) = std::fs::metadata(&path) {
                      if let Ok(mtime) = metadata.modified() {
                          // Compare with cached mtime
                          // If different, artifacts have changed
                      }
                  }
              }
          }

          false
      }
  }
  ```

**Dependencies**: Task 4.3
**Output**: Cache invalidation

---

## 4.5 Test manifest generation

- [ ] Create tests:
  ```rust
  #[tokio::test]
  async fn test_generate_manifest() {
      let temp_dir = tempfile::tempdir().unwrap();

      // Create test artifacts
      let artifact_dir = temp_dir.path().join("stable/arm64");
      std::fs::create_dir_all(&artifact_dir).unwrap();
      std::fs::write(artifact_dir.join("vmlinuz"), vec![0u8; 1024]).unwrap();
      std::fs::write(artifact_dir.join("initramfs.img"), vec![0u8; 512]).unwrap();

      let signing_key = SigningKey::generate(&mut OsRng);
      let mut state = ProviderState::new(
          ProviderConfig {
              artifacts_dir: temp_dir.path().to_path_buf(),
              // ...
          },
          signing_key,
      );

      let manifest = state.generate_manifest("stable", "arm64").await.unwrap();

      assert_eq!(manifest.channel, "stable");
      assert_eq!(manifest.arch, "arm64");
      assert!(manifest.artifacts.contains_key("kernel"));
      assert!(manifest.artifacts.contains_key("initramfs"));
      assert_eq!(manifest.signatures.len(), 1);
  }
  ```

**Dependencies**: Task 4.4
**Output**: Generation tests

---

## Validation Checklist

- [ ] ManifestBuilder creates valid manifests
- [ ] generate_manifest scans artifact directory
- [ ] All artifact hashes computed correctly
- [ ] Manifest signed automatically
- [ ] Caching reduces redundant computation
- [ ] Cache invalidates when files change
- [ ] All tests pass
