# mDNS Service Advertisement Integration

## Overview

The `mdns.rs` module provides DNS-SD (DNS Service Discovery) advertisement for Phase Boot Providers, allowing clients on the local network to automatically discover available boot image servers.

## Architecture Decision

### Why Not Use libp2p mDNS?

The existing `discovery.rs` uses libp2p's mDNS for **peer-to-peer discovery** of libp2p nodes. This is NOT suitable for HTTP service advertisement because:

1. **Different protocols**: libp2p mDNS advertises multiaddrs (e.g., `/ip4/192.168.1.10/tcp/4001/p2p/12D3KooW...`)
2. **Different use case**: P2P discovery is for finding compute peers, not HTTP boot servers
3. **Client mismatch**: `phase_discover.rs` looks for DHT records, not mDNS services

### Our Approach: DNS-SD for HTTP Services

We implement proper DNS-SD service advertisement using the `_phase-image._tcp.local.` service type:

- **Service Type**: `_phase-image._tcp.local.`
- **Discovery Tools**: `avahi-browse`, `dns-sd`, or any Bonjour-compatible client
- **TXT Records**: Metadata about channel, architecture, version, and HTTP port

## Current Implementation Status

### ✅ Completed

- [x] Module structure (`mdns.rs`)
- [x] Configuration types (`MdnsConfig`)
- [x] TXT record generation
- [x] Placeholder advertiser (`MdnsAdvertiser`)
- [x] Comprehensive tests
- [x] Documentation

### ⚠️ Placeholder

The `MdnsAdvertiser` currently logs configuration but **does not perform actual DNS-SD advertisement**. This is a conscious design decision to avoid adding dependencies until integration is confirmed.

## Integration Steps

### Step 1: Add mdns-sd Dependency

Add to `Cargo.toml`:

```toml
[dependencies]
mdns-sd = "0.11"
```

### Step 2: Implement MdnsAdvertiser

Replace the placeholder in `mdns.rs` with:

```rust
use mdns_sd::{ServiceDaemon, ServiceInfo};

pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    service_fullname: String,
}

impl MdnsAdvertiser {
    pub fn new(config: MdnsConfig) -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .context("Failed to create mDNS daemon")?;

        // Create service info
        let properties: Vec<(&str, &str)> = vec![
            (TXT_CHANNEL, &config.channel),
            (TXT_ARCH, &config.arch),
            (TXT_VERSION, &config.version),
            (TXT_HTTP_PORT, &config.http_port.to_string()),
        ];

        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &config.service_name,
            &format!("{}.local.", hostname()),
            (),
            config.http_port,
            &properties[..],
        )
        .context("Failed to create service info")?;

        let service_fullname = service_info.get_fullname().to_string();

        // Register service
        daemon.register(service_info)
            .context("Failed to register mDNS service")?;

        info!(
            "mDNS service registered: {} on port {}",
            service_fullname, config.http_port
        );

        Ok(Self {
            daemon,
            service_fullname,
        })
    }

    pub fn shutdown(self) -> Result<()> {
        self.daemon.unregister(&self.service_fullname)
            .context("Failed to unregister service")?;
        self.daemon.shutdown()
            .context("Failed to shutdown mDNS daemon")?;
        info!("mDNS service unregistered: {}", self.service_fullname);
        Ok(())
    }
}
```

### Step 3: Integrate with ProviderServer

In `server.rs`, add mDNS advertisement when starting the HTTP server:

```rust
use super::mdns::{MdnsConfig, MdnsAdvertiser};

pub struct ProviderServer {
    config: ProviderConfig,
    artifact_store: Arc<ArtifactStore>,
    metrics: Arc<ProviderMetrics>,
    mdns_advertiser: Option<MdnsAdvertiser>,  // Add this field
}

impl ProviderServer {
    pub async fn start(&mut self) -> Result<()> {
        // ... existing server setup ...

        // Start mDNS advertisement
        if self.config.enable_mdns {  // Add config flag
            let mdns_config = MdnsConfig::new(
                self.config.port,
                &self.config.channel,
                &self.config.arch,
            );

            match MdnsAdvertiser::new(mdns_config) {
                Ok(advertiser) => {
                    info!("mDNS advertisement started");
                    self.mdns_advertiser = Some(advertiser);
                }
                Err(e) => {
                    warn!("Failed to start mDNS advertisement: {}", e);
                    warn!("Continuing without mDNS (manual discovery required)");
                }
            }
        }

        // ... rest of server startup ...
    }

    pub async fn shutdown(self) -> Result<()> {
        // Shutdown mDNS first
        if let Some(advertiser) = self.mdns_advertiser {
            if let Err(e) = advertiser.shutdown() {
                warn!("Error shutting down mDNS: {}", e);
            }
        }

        // ... rest of shutdown ...
    }
}
```

### Step 4: Update ProviderConfig

Add mDNS configuration to `config.rs`:

```rust
pub struct ProviderConfig {
    // ... existing fields ...

    /// Enable mDNS service advertisement
    pub enable_mdns: bool,

    /// Channel name for mDNS advertisement (e.g., "stable", "testing")
    pub channel: String,

    /// Architecture for mDNS advertisement (e.g., "x86_64", "arm64")
    pub arch: String,
}
```

## Client Discovery

Clients can discover Phase Boot Providers using standard tools:

### Linux (Avahi)

```bash
# Browse for all Phase image providers
avahi-browse _phase-image._tcp --resolve --terminate

# Filter by architecture (look for arch=x86_64 in TXT records)
avahi-browse _phase-image._tcp --resolve | grep -A 10 "arch=x86_64"
```

### macOS (dns-sd)

```bash
# Browse for Phase image providers
dns-sd -B _phase-image._tcp

# Resolve specific instance
dns-sd -L "plasmd-hostname" _phase-image._tcp
```

### Programmatic Discovery

Use the `mdns-sd` crate for discovery:

```rust
use mdns_sd::{ServiceDaemon, ServiceEvent};

let daemon = ServiceDaemon::new()?;
let receiver = daemon.browse("_phase-image._tcp.local.")?;

while let Ok(event) = receiver.recv() {
    match event {
        ServiceEvent::ServiceResolved(info) => {
            println!("Found provider: {}", info.get_fullname());
            println!("  Address: {}:{}", info.get_addresses().iter().next().unwrap(), info.get_port());

            // Extract metadata from TXT records
            if let Some(channel) = info.get_property_val_str("channel") {
                println!("  Channel: {}", channel);
            }
            if let Some(arch) = info.get_property_val_str("arch") {
                println!("  Arch: {}", arch);
            }
        }
        _ => {}
    }
}
```

## Testing

### Unit Tests

```bash
# Run mdns module tests
cargo test --lib provider::mdns

# Expected output:
# test provider::mdns::tests::test_mdns_config_creation ... ok
# test provider::mdns::tests::test_txt_records ... ok
# test provider::mdns::tests::test_hostname ... ok
# test provider::mdns::tests::test_advertiser_creation ... ok
```

### Integration Testing

After implementing full DNS-SD:

1. **Start provider**:
   ```bash
   cargo run --bin plasmd -- provider --port 8080 --enable-mdns
   ```

2. **Discover from another terminal**:
   ```bash
   avahi-browse _phase-image._tcp --resolve --terminate
   ```

3. **Verify TXT records**:
   - Check `channel=stable`
   - Check `arch=x86_64` (or your platform)
   - Check `http_port=8080`
   - Check `version=0.1.0`

## Security Considerations

### mDNS is LAN-Only

- mDNS operates on the local link (subnet) only
- No exposure to internet by design
- Perfect for boot environments and local development

### TXT Record Information Disclosure

TXT records contain:
- Channel name (stable/testing)
- Architecture (x86_64/arm64)
- Version number
- HTTP port

This is **intentional** for service discovery. If you need to hide this information, disable mDNS advertisement.

### Authentication

mDNS provides discovery only. Clients should:
1. Discover provider via mDNS
2. Fetch manifest via HTTP
3. Verify manifest signatures (Ed25519)
4. Verify artifact hashes (SHA-256)

## Performance

- **Memory**: ~1-2 MB for mdns-sd daemon
- **CPU**: Minimal (only on service registration/queries)
- **Network**: Multicast DNS packets (224.0.0.251:5353)

## Alternatives Considered

### 1. libp2p mDNS (Rejected)

- **Pro**: Already in use for P2P discovery
- **Con**: Advertises multiaddrs, not HTTP services
- **Con**: Client mismatch (DHT vs mDNS)

### 2. Manual Discovery (Current Fallback)

- **Pro**: Zero dependencies
- **Con**: Requires manual configuration
- **Con**: Poor UX for boot environments

### 3. DNS-SD via Avahi D-Bus (Rejected)

- **Pro**: System integration on Linux
- **Con**: Platform-specific (no Windows/macOS)
- **Con**: Requires D-Bus dependencies

### 4. mdns-sd Crate (Recommended)

- **Pro**: Pure Rust, cross-platform
- **Pro**: Standard DNS-SD implementation
- **Pro**: Active maintenance
- **Con**: Additional dependency (~200 KB compiled)

## References

- [RFC 6763: DNS-Based Service Discovery](https://datatracker.ietf.org/doc/html/rfc6763)
- [RFC 6762: Multicast DNS](https://datatracker.ietf.org/doc/html/rfc6762)
- [mdns-sd Crate Documentation](https://docs.rs/mdns-sd/)
- [libp2p mDNS (for comparison)](https://docs.rs/libp2p-mdns/)

## Future Enhancements

1. **Dynamic TXT Records**: Update channel/arch without restart
2. **Multiple Instances**: Advertise multiple channels from one provider
3. **Load Balancing**: Priority/weight in SRV records
4. **IPv6 Support**: Advertise on both IPv4 and IPv6
5. **Service Monitoring**: Health checks via TXT record updates
