# Task 2 — DHT Refresh & TTL

**Agent**: Networking Agent
**Estimated**: 2 days

## 2.1 Implement periodic refresh

- [ ] Create refresh task:
  ```rust
  impl Discovery {
      /// Start background task to refresh DHT records
      pub fn start_manifest_refresh(
          &self,
          manifests: Vec<(String, String, String)>,  // (channel, arch, url)
          interval: Duration,
      ) -> tokio::task::JoinHandle<()> {
          let peer_id = self.peer_id.to_string();
          let swarm_tx = self.command_tx.clone();

          tokio::spawn(async move {
              let mut interval_timer = tokio::time::interval(interval);

              loop {
                  interval_timer.tick().await;

                  for (channel, arch, url) in &manifests {
                      // Send refresh command to swarm
                      let _ = swarm_tx.send(SwarmCommand::RefreshManifest {
                          channel: channel.clone(),
                          arch: arch.clone(),
                          url: url.clone(),
                      }).await;
                  }

                  tracing::debug!("DHT manifest records refreshed");
              }
          })
      }
  }
  ```

**Dependencies**: M3/Task 1
**Output**: Periodic refresh task

---

## 2.2 Configure refresh interval

- [ ] Add to provider config:
  ```toml
  [provider.dht]
  # Refresh interval in seconds (default: 30 minutes)
  refresh_interval = 1800

  # Record TTL in seconds (default: 1 hour)
  record_ttl = 3600
  ```
- [ ] Use configuration:
  ```rust
  let refresh_interval = Duration::from_secs(config.dht.refresh_interval);
  let record_ttl = Duration::from_secs(config.dht.record_ttl);
  ```

**Dependencies**: Task 2.1
**Output**: Configurable refresh interval

---

## 2.3 Handle record expiration

- [ ] Set TTL on records:
  ```rust
  libp2p::kad::Record {
      key,
      value: record_value,
      publisher: Some(self.peer_id),
      expires: Some(std::time::Instant::now() + record_ttl),
  }
  ```
- [ ] Ensure refresh happens before expiration:
  ```rust
  // Refresh at 2/3 of TTL to ensure overlap
  let refresh_interval = record_ttl * 2 / 3;
  ```

**Dependencies**: Task 2.2
**Output**: TTL handling

---

## 2.4 Re-publish on manifest change

- [ ] Watch for manifest updates:
  ```rust
  impl ProviderState {
      /// Called when artifacts change
      pub async fn on_artifacts_changed(
          &mut self,
          channel: &str,
          arch: &str,
          discovery: &mut Discovery,
      ) -> Result<(), ProviderError> {
          // Invalidate cache
          self.invalidate_manifest(channel, arch);

          // Regenerate manifest
          let manifest = self.get_manifest(channel, arch, 0).await?;

          // Re-publish to DHT
          let url = self.manifest_url(channel, arch);
          discovery.publish_manifest(
              channel,
              arch,
              &url,
              &manifest.manifest.version,
          ).await?;

          tracing::info!(
              "Re-published manifest after artifact change: {}/{}",
              channel, arch
          );

          Ok(())
      }
  }
  ```

**Dependencies**: Task 2.3
**Output**: Re-publish on change

---

## 2.5 Graceful shutdown

- [ ] Remove records on shutdown:
  ```rust
  impl Discovery {
      /// Remove boot manifest records from DHT
      pub async fn unpublish_manifests(
          &mut self,
          manifests: &[(String, String)],  // (channel, arch)
      ) {
          for (channel, arch) in manifests {
              let key = manifest_dht_key(channel, arch);

              // Note: Kademlia doesn't have explicit delete
              // Records will expire based on TTL
              // But we stop refreshing, so they'll disappear

              tracing::info!(
                  "Stopped advertising manifest: {}/{}",
                  channel, arch
              );
          }
      }
  }
  ```
- [ ] Call on SIGTERM/SIGINT

**Dependencies**: Task 2.4
**Output**: Graceful shutdown

---

## 2.6 Test refresh behavior

- [ ] Verify records persist after refresh:
  ```bash
  # Start provider
  plasmd serve --artifacts /tmp/artifacts &
  PID=$!

  # Wait for initial publish
  sleep 5

  # Verify discoverable
  phase-discover --channel stable --arch arm64

  # Wait past refresh interval
  sleep 1900  # >30 minutes

  # Should still be discoverable
  phase-discover --channel stable --arch arm64

  # Stop provider
  kill $PID

  # Wait for expiration
  sleep 3700  # >1 hour

  # Should no longer be discoverable
  phase-discover --channel stable --arch arm64 --timeout 10
  # Expected: timeout/not found
  ```

**Dependencies**: Task 2.5
**Output**: Refresh behavior verified

---

## Validation Checklist

- [ ] Records refresh before TTL expiration
- [ ] Refresh interval configurable
- [ ] Records expire after provider stops
- [ ] Manifest changes trigger re-publish
- [ ] Graceful shutdown stops refresh
- [ ] No stale records after shutdown + TTL
