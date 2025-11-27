# Task 4 — Multi-Channel Support

**Agent**: Networking Agent
**Estimated**: 1 day

## 4.1 Configure multiple channels

- [ ] Update provider config:
  ```toml
  [provider]
  # Channels to advertise
  channels = ["stable", "testing"]

  # Architectures to advertise
  architectures = ["arm64", "x86_64"]

  # Channel-specific settings
  [provider.channels.stable]
  artifacts_dir = "/var/lib/plasm/stable"

  [provider.channels.testing]
  artifacts_dir = "/var/lib/plasm/testing"
  ```

**Dependencies**: M3/Tasks 1-3
**Output**: Multi-channel configuration

---

## 4.2 Scan available artifacts

- [ ] Auto-detect channels/archs from directory structure:
  ```rust
  impl ProviderState {
      /// Scan artifacts directory to find available channel/arch combinations
      pub fn scan_available(&self) -> Vec<(String, String)> {
          let mut available = Vec::new();

          let channels = ["stable", "testing"];
          let archs = ["arm64", "x86_64"];

          for channel in channels {
              for arch in archs {
                  let dir = self.config.artifacts_dir.join(channel).join(arch);

                  // Check if kernel exists (minimum requirement)
                  if dir.join("vmlinuz").exists() {
                      available.push((channel.to_string(), arch.to_string()));
                  }
              }
          }

          available
      }
  }
  ```

**Dependencies**: Task 4.1
**Output**: Artifact scanning

---

## 4.3 Advertise all available combinations

- [ ] Publish to both DHT and mDNS:
  ```rust
  impl ProviderServer {
      pub async fn advertise_all(
          &mut self,
          discovery: &mut Discovery,
          mdns: &mut MdnsAdvertiser,
      ) -> Result<(), ProviderError> {
          let available = self.state.read().await.scan_available();

          tracing::info!("Found {} channel/arch combinations", available.len());

          for (channel, arch) in &available {
              // Generate manifest
              let manifest = self.state.write().await
                  .get_manifest(channel, arch, 0).await?;

              let manifest_url = self.manifest_url(channel, arch);

              // DHT
              discovery.publish_manifest(
                  channel,
                  arch,
                  &manifest_url,
                  &manifest.manifest.version,
              ).await?;

              // mDNS
              mdns.advertise(
                  &self.hostname,
                  self.port,
                  channel,
                  arch,
                  &manifest_url,
                  &manifest.manifest.version,
              )?;

              tracing::info!("Advertising {}/{}", channel, arch);
          }

          Ok(())
      }
  }
  ```

**Dependencies**: Task 4.2
**Output**: Multi-channel advertisement

---

## 4.4 Handle partial availability

- [ ] Provider may have stable/arm64 but not stable/x86_64:
  ```rust
  // Only advertise what actually exists
  let available = state.scan_available();

  // Log what's missing
  let expected = vec![
      ("stable", "arm64"),
      ("stable", "x86_64"),
      ("testing", "arm64"),
      ("testing", "x86_64"),
  ];

  for (channel, arch) in expected {
      if !available.contains(&(channel.to_string(), arch.to_string())) {
          tracing::warn!("Missing artifacts for {}/{}", channel, arch);
      }
  }
  ```

**Dependencies**: Task 4.3
**Output**: Partial availability handling

---

## 4.5 Test multi-channel

- [ ] Setup test artifacts:
  ```bash
  mkdir -p /tmp/artifacts/{stable,testing}/{arm64,x86_64}

  # Create stable/arm64
  dd if=/dev/urandom of=/tmp/artifacts/stable/arm64/vmlinuz bs=1M count=10

  # Create testing/arm64
  dd if=/dev/urandom of=/tmp/artifacts/testing/arm64/vmlinuz bs=1M count=10

  # Leave stable/x86_64 and testing/x86_64 empty
  ```
- [ ] Start provider:
  ```bash
  plasmd serve --artifacts /tmp/artifacts

  # Expected logs:
  # INFO Found 2 channel/arch combinations
  # INFO Advertising stable/arm64
  # INFO Advertising testing/arm64
  # WARN Missing artifacts for stable/x86_64
  # WARN Missing artifacts for testing/x86_64
  ```
- [ ] Verify discovery:
  ```bash
  # Should work
  phase-discover --channel stable --arch arm64

  # Should fail (not available)
  phase-discover --channel stable --arch x86_64 --timeout 10
  ```

**Dependencies**: Task 4.4
**Output**: Multi-channel verified

---

## Validation Checklist

- [ ] Multiple channels configurable
- [ ] Auto-detection of available artifacts
- [ ] Each channel/arch advertised separately in DHT
- [ ] Each channel/arch advertised separately in mDNS
- [ ] Missing combinations logged but not advertised
- [ ] Clients can discover specific channel/arch
