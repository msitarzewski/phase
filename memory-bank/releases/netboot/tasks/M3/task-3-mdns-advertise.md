# Task 3 — mDNS Advertisement

**Agent**: Networking Agent
**Estimated**: 2 days

## 3.1 Choose mDNS implementation

- [ ] Options:
  - `libp2p-mdns` - Already in libp2p stack (peer discovery only)
  - `mdns` crate - Service advertisement
  - `zeroconf` crate - Cross-platform
- [ ] **Recommendation**: `mdns` crate for service advertisement
  - Lightweight
  - Supports TXT records
  - Active maintenance

**Dependencies**: None
**Output**: mDNS implementation decision

---

## 3.2 Add mdns dependency

- [ ] Update `daemon/Cargo.toml`:
  ```toml
  [dependencies]
  mdns-sd = "0.10"  # or latest
  ```

**Dependencies**: Task 3.1
**Output**: Dependency added

---

## 3.3 Implement mDNS service advertisement

- [ ] Create `daemon/src/provider/mdns.rs`:
  ```rust
  use mdns_sd::{ServiceDaemon, ServiceInfo};
  use std::collections::HashMap;

  pub struct MdnsAdvertiser {
      daemon: ServiceDaemon,
      service_name: String,
  }

  impl MdnsAdvertiser {
      const SERVICE_TYPE: &'static str = "_phase-image._tcp.local.";

      pub fn new() -> Result<Self, MdnsError> {
          let daemon = ServiceDaemon::new()
              .map_err(|e| MdnsError::DaemonError(e.to_string()))?;

          Ok(Self {
              daemon,
              service_name: String::new(),
          })
      }

      /// Advertise boot image service
      pub fn advertise(
          &mut self,
          hostname: &str,
          port: u16,
          channel: &str,
          arch: &str,
          manifest_url: &str,
          version: &str,
      ) -> Result<(), MdnsError> {
          // Build TXT records
          let mut properties = HashMap::new();
          properties.insert("channel".to_string(), channel.to_string());
          properties.insert("arch".to_string(), arch.to_string());
          properties.insert("manifest".to_string(), manifest_url.to_string());
          properties.insert("version".to_string(), version.to_string());

          // Create service info
          let service_name = format!("phase-{}-{}", channel, arch);
          let service_info = ServiceInfo::new(
              Self::SERVICE_TYPE,
              &service_name,
              hostname,
              (),  // Auto-detect IP
              port,
              Some(properties),
          ).map_err(|e| MdnsError::ServiceError(e.to_string()))?;

          // Register service
          self.daemon.register(service_info)
              .map_err(|e| MdnsError::RegisterError(e.to_string()))?;

          self.service_name = service_name;

          tracing::info!(
              "mDNS: Advertising {} at {}:{}",
              Self::SERVICE_TYPE,
              hostname,
              port
          );

          Ok(())
      }

      /// Stop advertising
      pub fn stop(&mut self) -> Result<(), MdnsError> {
          if !self.service_name.is_empty() {
              self.daemon.unregister(&self.service_name)
                  .map_err(|e| MdnsError::UnregisterError(e.to_string()))?;
          }
          Ok(())
      }
  }

  #[derive(Debug, thiserror::Error)]
  pub enum MdnsError {
      #[error("mDNS daemon error: {0}")]
      DaemonError(String),

      #[error("Service creation error: {0}")]
      ServiceError(String),

      #[error("Registration error: {0}")]
      RegisterError(String),

      #[error("Unregistration error: {0}")]
      UnregisterError(String),
  }
  ```

**Dependencies**: Task 3.2
**Output**: mDNS advertisement implementation

---

## 3.4 Integrate with provider startup

- [ ] Start mDNS when provider starts:
  ```rust
  impl ProviderServer {
      pub async fn start_with_mdns(
          &mut self,
      ) -> Result<MdnsAdvertiser, ProviderError> {
          let mut mdns = MdnsAdvertiser::new()?;

          let hostname = hostname::get()
              .map(|h| h.to_string_lossy().to_string())
              .unwrap_or_else(|_| "phase-provider".to_string());

          // Advertise each channel/arch
          for channel in &self.config.channels {
              for arch in &self.config.architectures {
                  let manifest_url = self.manifest_url(channel, arch);
                  let manifest = self.state.read().await
                      .get_manifest(channel, arch, 300).await?;

                  mdns.advertise(
                      &hostname,
                      self.port,
                      channel,
                      arch,
                      &manifest_url,
                      &manifest.manifest.version,
                  )?;
              }
          }

          Ok(mdns)
      }
  }
  ```

**Dependencies**: Task 3.3
**Output**: Provider-mDNS integration

---

## 3.5 Test mDNS discovery

- [ ] Manual test with dns-sd (macOS):
  ```bash
  # Start provider
  plasmd serve --artifacts /tmp/artifacts &

  # Browse for services
  dns-sd -B _phase-image._tcp local.

  # Look up specific service
  dns-sd -L "phase-stable-arm64" _phase-image._tcp local.

  # Expected output includes TXT records:
  # channel=stable
  # arch=arm64
  # manifest=http://...
  ```
- [ ] Test with avahi-browse (Linux):
  ```bash
  avahi-browse -r _phase-image._tcp
  ```

**Dependencies**: Task 3.4
**Output**: mDNS discovery verified

---

## 3.6 Test with phase-discover

- [ ] Use phase-discover in mDNS mode:
  ```bash
  # phase-discover should try mDNS first
  phase-discover --channel stable --arch arm64 --mode lan

  # Expected: finds local provider via mDNS
  ```

**Dependencies**: Task 3.5
**Output**: phase-discover mDNS integration verified

---

## Validation Checklist

- [ ] mDNS service advertised as `_phase-image._tcp.local`
- [ ] TXT records include channel, arch, manifest URL, version
- [ ] Service discoverable with dns-sd/avahi-browse
- [ ] phase-discover finds provider via mDNS
- [ ] Multiple channel/arch combinations advertised
- [ ] Service unregistered on shutdown
