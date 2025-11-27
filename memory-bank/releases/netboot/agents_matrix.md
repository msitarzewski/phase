# Agents Matrix (who does what)

| Area | Agent/Owner | Responsibilities |
|------|-------------|------------------|
| HTTP Server | Runtime Agent | Hyper/Axum integration, endpoints, range requests |
| Manifest | Security Agent | Schema, generation, signing, verification |
| DHT | Networking Agent | Record publishing, key scheme, refresh |
| mDNS | Networking Agent | Service advertisement, TXT records |
| CLI | Tooling Agent | Commands, argument parsing, output formatting |
| Config | Tooling Agent | TOML parsing, validation, defaults |
| macOS | Platform Agent | ARM64 testing, Darwin-specific code |
| Linux | Platform Agent | x86_64/ARM64 testing, systemd integration |
| Integration | QA Agent | End-to-end tests, multi-provider scenarios |
| Docs | Docs Agent | Guides, diagrams, troubleshooting |

---

## Detailed Responsibilities

### Runtime Agent
- Integrate HTTP server (hyper or axum) into plasmd
- Implement artifact serving with proper Content-Type headers
- Support HTTP Range requests for large files (kernel, rootfs)
- Health check endpoint for load balancers
- Graceful shutdown and resource cleanup

### Security Agent
- Define manifest JSON schema (compatible with phase-verify)
- Implement manifest generation from artifact directory
- Ed25519 signing using existing plasmd keypair
- Hash computation (SHA256) for all artifacts
- Version and channel metadata

### Networking Agent
- DHT record publishing: `/phase/{channel}/{arch}/manifest`
- Record refresh (TTL management, re-advertisement)
- mDNS service advertisement: `_phase-image._tcp`
- TXT record format for mDNS (arch, channel, manifest URL)
- Integration with existing libp2p swarm

### Tooling Agent
- `plasmd serve` command implementation
- `plasmd provider status` command
- `plasmd manifest generate` command
- TOML configuration file parsing
- Argument validation and error messages

### Platform Agent
- macOS ARM64 build and testing
- Linux x86_64/ARM64 build and testing
- Platform-specific network configuration
- File path conventions per platform

### QA Agent
- End-to-end tests: VM discovers provider, fetches, boots
- Multi-provider tests: multiple providers, client selection
- Self-hosting test: boot from provider, become provider
- Cross-architecture tests: ARM64 VM from x86_64 provider
- Failure scenarios: provider down, partial fetch, bad signature

### Docs Agent
- Provider setup guide (step-by-step)
- Network topology documentation
- Troubleshooting guide
- Architecture diagrams (ASCII and/or images)
- Security considerations and best practices
