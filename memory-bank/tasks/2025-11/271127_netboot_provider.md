# 271127_netboot_provider

## Objective

Implement the Netboot Provider - HTTP-based boot artifact server with DHT/mDNS advertisement - completing the provider side of the Phase Boot system to enable the self-hosting loop.

## Outcome

- 80 tests passing
- Build: Successful (minor warnings in legacy binaries only)
- 2,510 lines of Rust code (provider module)
- 3,000 lines of documentation
- All 6 milestones complete

## Files Created

### Provider Module (`daemon/src/provider/`)

| File | Lines | Purpose |
|------|-------|---------|
| server.rs | 504 | HTTP server with axum, artifact endpoints, range requests |
| manifest.rs | 549 | Boot manifest schema, validation, builder |
| artifacts.rs | 286 | Artifact storage, hash caching, arch aliasing |
| signing.rs | 243 | Ed25519 manifest signing and verification |
| generator.rs | 221 | Manifest generation from artifacts |
| dht.rs | 142 | DHT record types for advertisement |
| mdns.rs | 222 | mDNS service configuration |
| metrics.rs | 113 | Request metrics and health checks |
| config.rs | 176 | Provider configuration |
| mod.rs | 54 | Module exports |

### Documentation (`daemon/docs/`)

| File | Lines | Purpose |
|------|-------|---------|
| provider-quickstart.md | 177 | 5-minute setup guide |
| architecture.md | 415 | System design documentation |
| api-reference.md | 737 | HTTP and CLI reference |
| troubleshooting.md | 847 | Common issues and solutions |
| security.md | 824 | Security best practices |

### CLI Commands Added

```bash
# Start boot artifact provider
plasmd serve [OPTIONS]
  -a, --artifacts <PATH>   # Artifacts directory
  -c, --channel <CHANNEL>  # Release channel [default: stable]
  -A, --arch <ARCH>        # Architecture (auto-detect)
  -p, --port <PORT>        # HTTP port [default: 8080]
  -b, --bind <BIND>        # Bind address [default: 0.0.0.0]
  --no-dht                 # Disable DHT advertisement
  --no-mdns                # Disable mDNS advertisement

# Query provider status
plasmd provider status [--json] [-a <ADDR>]

# List available artifacts
plasmd provider list [-a <ADDR>]
```

### HTTP Endpoints

| Endpoint | Response |
|----------|----------|
| GET / | Provider info (name, version, uptime) |
| GET /health | 200 OK / 503 Unhealthy |
| GET /status | Detailed status with metrics |
| GET /manifest.json | Boot manifest for default channel/arch |
| GET /:channel/:arch/manifest.json | Channel-specific manifest |
| GET /:channel/:arch/:artifact | Download artifact (Range supported) |

## Patterns Applied

### From systemPatterns.md

- **HTTP Artifact Server Pattern**: axum Router with shared AppState
- **Boot Manifest Schema Pattern**: Signed manifest with artifacts HashMap
- **Manifest Signing Pattern**: Ed25519 over SHA256 of manifest JSON
- **DHT Advertisement Pattern**: `/phase/{channel}/{arch}/manifest` key scheme
- **Architecture Aliasing Pattern**: arm64/aarch64, amd64/x86_64 transparent aliasing
- **HTTP Range Request Pattern**: RFC 7233 compliant partial content

### New Patterns Documented

Added 6 new patterns to `systemPatterns.md#Netboot Provider Patterns`:
1. HTTP Artifact Server Pattern
2. Boot Manifest Schema Pattern
3. Manifest Signing Pattern
4. DHT Advertisement Pattern
5. Architecture Aliasing Pattern
6. HTTP Range Request Pattern

## Integration Points

### With Existing Code

- `daemon/src/main.rs` - Added `Serve` and `Provider` CLI commands
- `daemon/src/lib.rs` - Exports provider module
- `daemon/src/network/discovery.rs` - Added `publish_manifest_record()` method
- `daemon/Cargo.toml` - Added axum, tower, tower-http, chrono dependencies

### With Phase Boot Consumer

The provider serves artifacts that the Phase Boot consumer fetches:

```
Consumer (phase-discover) → DHT → ManifestRecord → HTTP URL
Consumer (phase-fetch) → Provider HTTP → Artifacts
Consumer (phase-verify) → Manifest signature verification
```

## Architectural Decisions

### axum over actix-web
- **Decision**: Use axum for HTTP server
- **Rationale**: Tower ecosystem compatibility, async-first, simpler API
- **Trade-offs**: Slightly less batteries-included than actix

### Manifest as HashMap vs Array
- **Decision**: Artifacts stored as `HashMap<String, ArtifactInfo>`
- **Rationale**: Lookup by name (kernel, initramfs), flexible additions
- **Trade-offs**: Slightly more complex serialization

### Architecture Aliasing
- **Decision**: Transparently try arm64/aarch64, amd64/x86_64 variants
- **Rationale**: Linux uses arm64/amd64, Rust uses aarch64/x86_64
- **Trade-offs**: Slight performance cost (extra filesystem checks)

## Testing Notes

### Integration Tests Performed
1. Server startup and info endpoint
2. Health check (200/503)
3. Status endpoint with metrics
4. Manifest generation
5. Artifact download
6. Range request support
7. CLI status command
8. CLI list command

### Issues Found and Fixed
1. **CLI format mismatch**: `provider list` expected array, manifest returns object - Fixed
2. **Arch aliasing**: Server detected aarch64, artifacts stored as arm64 - Fixed with aliasing

## Artifacts

- Release plan: `memory-bank/releases/netboot/`
- Documentation: `daemon/docs/`
- Provider module: `daemon/src/provider/`

## Self-Hosting Loop

With this implementation, the complete self-hosting loop is now possible:

```
1. Boot from DHT (Phase Boot consumer)
     ↓
2. Run plasmd serve (Netboot Provider)
     ↓
3. Advertise to DHT (ManifestRecord)
     ↓
4. Others boot from you
```

## Next Steps (Future Work)

- [ ] Full mDNS service advertisement (add mdns-sd crate)
- [ ] Multi-provider load balancing
- [ ] Manifest caching with TTL
- [ ] Production key management
- [ ] Secure Boot integration
