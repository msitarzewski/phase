# Milestone M4 — CLI & Configuration

**Status**: PLANNED
**Owner**: Tooling Agent (primary), Runtime Agent (integration)
**Dependencies**: M3 complete (DHT/mDNS advertisement)
**Estimated Effort**: 1.5 weeks

## Intent Summary

Provide user-friendly CLI commands and configuration files for operating a provider. This is the operator interface - making it easy to start, configure, and monitor a provider.

---

## Acceptance Criteria

1. **plasmd serve**: Single command to start provider mode
2. **Configuration file**: TOML config for persistent settings
3. **Status commands**: Check provider health and metrics
4. **Manifest commands**: Generate and inspect manifests
5. **Platform defaults**: Sensible defaults for macOS and Linux

## CLI Commands

```bash
# Start provider (main command)
plasmd serve [OPTIONS]
  --artifacts <PATH>      Path to artifacts directory
  --channel <CHANNEL>     Channel to advertise (default: stable)
  --arch <ARCH>           Architecture (default: auto-detect)
  --port <PORT>           HTTP port (default: 8080)
  --config <FILE>         Config file path
  --no-dht                Disable DHT advertisement
  --no-mdns               Disable mDNS advertisement

# Provider status
plasmd provider status    Show provider health and metrics
plasmd provider list      List advertised channels/architectures
plasmd provider keyid     Show signing key ID

# Manifest operations
plasmd manifest generate  Generate manifest from artifacts
plasmd manifest show      Display current manifest
plasmd manifest verify    Verify manifest signature
```

---

## Tasks

1. [CLI Command Structure](task-1-cli-structure.md) — Clap command definitions
2. [Configuration File](task-2-config-file.md) — TOML parsing and defaults
3. [Serve Command](task-3-serve-command.md) — Main provider entry point
4. [Status Commands](task-4-status-commands.md) — Health and metrics
5. [Testing](task-5-testing.md) — CLI integration tests
