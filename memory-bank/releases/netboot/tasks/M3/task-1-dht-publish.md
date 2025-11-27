# Task 1 — DHT Record Publishing

**Agent**: Networking Agent
**Estimated**: 3 days

## 1.1 Define DHT record structure

- [ ] Create `daemon/src/provider/dht.rs`:
  ```rust
  use serde::{Deserialize, Serialize};

  /// DHT record value for boot manifest advertisement
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct ManifestRecord {
      /// Full URL to manifest.json
      pub url: String,

      /// Provider's libp2p peer ID
      pub peer_id: String,

      /// Human-readable provider name
      #[serde(skip_serializing_if = "Option::is_none")]
      pub provider_name: Option<String>,

      /// ISO 8601 timestamp of last update
      pub last_updated: String,

      /// Manifest version being advertised
      pub version: String,
  }

  impl ManifestRecord {
      pub fn new(
          url: &str,
          peer_id: &str,
          provider_name: Option<&str>,
          version: &str,
      ) -> Self {
          Self {
              url: url.to_string(),
              peer_id: peer_id.to_string(),
              provider_name: provider_name.map(|s| s.to_string()),
              last_updated: chrono::Utc::now().to_rfc3339(),
              version: version.to_string(),
          }
      }

      pub fn to_bytes(&self) -> Vec<u8> {
          serde_json::to_vec(self).expect("ManifestRecord serialization")
      }

      pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
          serde_json::from_slice(bytes)
      }
  }
  ```

**Dependencies**: M2 complete
**Output**: DHT record structure

---

## 1.2 Implement DHT key generation

- [ ] Add key generation:
  ```rust
  use libp2p::kad::RecordKey;

  /// Generate DHT key for boot manifest
  pub fn manifest_dht_key(channel: &str, arch: &str) -> RecordKey {
      let key_str = format!("/phase/{}/{}/manifest", channel, arch);
      RecordKey::new(&key_str)
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_manifest_key_format() {
          let key = manifest_dht_key("stable", "arm64");
          let key_bytes = key.as_ref();
          let key_str = std::str::from_utf8(key_bytes).unwrap();
          assert_eq!(key_str, "/phase/stable/arm64/manifest");
      }
  }
  ```

**Dependencies**: Task 1.1
**Output**: DHT key generation

---

## 1.3 Add publish method to Discovery

- [ ] Extend `daemon/src/network/discovery.rs`:
  ```rust
  use crate::provider::dht::{ManifestRecord, manifest_dht_key};

  impl Discovery {
      /// Publish boot manifest URL to DHT
      pub async fn publish_manifest(
          &mut self,
          channel: &str,
          arch: &str,
          manifest_url: &str,
          version: &str,
      ) -> Result<(), DiscoveryError> {
          let key = manifest_dht_key(channel, arch);

          let record = ManifestRecord::new(
              manifest_url,
              &self.peer_id.to_string(),
              self.config.provider_name.as_deref(),
              version,
          );

          let record_value = record.to_bytes();

          tracing::info!(
              "Publishing boot manifest to DHT: {} -> {}",
              std::str::from_utf8(key.as_ref()).unwrap_or("?"),
              manifest_url
          );

          // Put record in DHT
          self.swarm
              .behaviour_mut()
              .kademlia
              .put_record(
                  libp2p::kad::Record {
                      key,
                      value: record_value,
                      publisher: Some(self.peer_id),
                      expires: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
                  },
                  libp2p::kad::Quorum::One,
              )
              .map_err(|e| DiscoveryError::DhtError(e.to_string()))?;

          Ok(())
      }
  }
  ```

**Dependencies**: Task 1.2
**Output**: Publish method

---

## 1.4 Handle DHT put confirmation

- [ ] Add event handling for put confirmation:
  ```rust
  // In Discovery event loop
  SwarmEvent::Behaviour(BehaviourEvent::Kademlia(
      KademliaEvent::OutboundQueryProgressed {
          result: QueryResult::PutRecord(result),
          ..
      }
  )) => {
      match result {
          Ok(PutRecordOk { key }) => {
              tracing::info!("DHT record published: {:?}", key);
          }
          Err(e) => {
              tracing::error!("DHT put failed: {:?}", e);
          }
      }
  }
  ```

**Dependencies**: Task 1.3
**Output**: Put confirmation handling

---

## 1.5 Integrate with provider startup

- [ ] Publish manifests when provider starts:
  ```rust
  // In provider initialization
  impl ProviderServer {
      pub async fn start_with_discovery(
          self,
          discovery: &mut Discovery,
      ) -> Result<(), ProviderError> {
          // Start HTTP server
          let addr = self.start().await?;
          let base_url = format!("http://{}:{}", self.external_ip, addr.port());

          // Publish each channel/arch to DHT
          for channel in &self.config.channels {
              for arch in &self.config.architectures {
                  let manifest_url = format!("{}/{}/{}/manifest.json", base_url, channel, arch);

                  // Get manifest version
                  let manifest = self.state.read().await
                      .get_manifest(channel, arch, 300).await?;

                  discovery.publish_manifest(
                      channel,
                      arch,
                      &manifest_url,
                      &manifest.manifest.version,
                  ).await?;
              }
          }

          Ok(())
      }
  }
  ```

**Dependencies**: Task 1.4
**Output**: Provider-discovery integration

---

## 1.6 Test DHT publishing

- [ ] Manual test:
  ```bash
  # Start provider with DHT
  plasmd serve --artifacts /tmp/artifacts --channel stable --arch arm64

  # Use phase-discover to find it
  phase-discover --channel stable --arch arm64 --timeout 30

  # Expected: URL of provider's manifest
  ```
- [ ] Log verification:
  ```
  INFO Publishing boot manifest to DHT: /phase/stable/arm64/manifest -> http://192.168.1.100:8080/...
  INFO DHT record published: /phase/stable/arm64/manifest
  ```

**Dependencies**: Task 1.5
**Output**: DHT publishing verified

---

## Validation Checklist

- [ ] ManifestRecord serializes to valid JSON
- [ ] DHT keys follow `/phase/{channel}/{arch}/manifest` format
- [ ] Records published with correct peer_id
- [ ] Put confirmation logged
- [ ] phase-discover can find published records
- [ ] Records expire after TTL
