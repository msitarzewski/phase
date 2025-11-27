# Phase Boot Provider Quickstart

Get a boot artifact provider running in 5 minutes.

## Prerequisites

- macOS (ARM64) or Linux (x86_64)
- Rust toolchain (for building from source)
- Boot artifacts (kernel, initramfs)

## Step 1: Build plasmd

```bash
cd daemon
cargo build --release
```

The binary will be available at `target/release/plasmd`.

## Step 2: Prepare Artifacts

### macOS (ARM64)

```bash
# Create artifacts directory
mkdir -p ~/Library/Application\ Support/plasm/artifacts/stable/aarch64

# Copy your boot files
cp vmlinuz ~/Library/Application\ Support/plasm/artifacts/stable/aarch64/kernel
cp initramfs.img ~/Library/Application\ Support/plasm/artifacts/stable/aarch64/initramfs
```

### Linux (x86_64)

```bash
# Create artifacts directory
sudo mkdir -p /var/lib/plasm/artifacts/stable/x86_64

# Copy your boot files
sudo cp vmlinuz /var/lib/plasm/artifacts/stable/x86_64/kernel
sudo cp initramfs.img /var/lib/plasm/artifacts/stable/x86_64/initramfs
```

**Directory structure:**
```
artifacts/
  stable/           # Channel name
    aarch64/        # Architecture (aarch64 on macOS ARM, x86_64 on Linux)
      kernel        # Kernel image
      initramfs     # Initial RAM filesystem
```

## Step 3: Start Provider

```bash
./target/release/plasmd serve
```

You should see:
```
╔══════════════════════════════════════════════╗
║           Phase Boot Provider                ║
╠══════════════════════════════════════════════╣
║ HTTP:     http://0.0.0.0:8080                ║
║ Artifacts: /Users/.../plasm/artifacts        ║
║ Channel:  stable                             ║
║ Arch:     aarch64                            ║
║ DHT:      enabled                            ║
║ mDNS:     enabled                            ║
╚══════════════════════════════════════════════╝
```

The provider is now serving boot artifacts over HTTP and advertising via DHT and mDNS.

## Step 4: Verify

```bash
# Check health
curl http://localhost:8080/health

# View manifest
curl http://localhost:8080/manifest.json | jq

# Check status (detailed)
./target/release/plasmd provider status

# List available artifacts
./target/release/plasmd provider list
```

## Command Reference

### Starting the Provider

```bash
# Start with defaults (auto-detect paths and architecture)
plasmd serve

# Custom port and artifacts directory
plasmd serve --port 9000 --artifacts /path/to/artifacts

# Different channel (e.g., testing, beta)
plasmd serve --channel testing

# Disable DHT or mDNS
plasmd serve --no-dht --no-mdns

# Custom bind address (e.g., localhost only)
plasmd serve --bind 127.0.0.1
```

### Checking Provider Status

```bash
# Human-friendly status
plasmd provider status

# JSON output
plasmd provider status --json

# Check remote provider
plasmd provider status --addr http://192.168.1.100:8080
```

### Listing Artifacts

```bash
# List local provider artifacts
plasmd provider list

# List remote provider artifacts
plasmd provider list --addr http://192.168.1.100:8080
```

## Default Paths

The provider uses platform-specific default paths:

**macOS:**
- Artifacts: `~/Library/Application Support/plasm/artifacts`

**Linux:**
- Artifacts: `/var/lib/plasm/artifacts`

**Windows:**
- Artifacts: `%LOCALAPPDATA%\plasm\artifacts`

Override with `--artifacts` flag.

## Next Steps

- [Configure multiple channels](./configuration.md) - Serve stable, beta, and testing channels simultaneously
- [Set up as a system service](./systemd.md) - Run provider as a background service
- [Enable DHT for internet-wide discovery](./dht.md) - Make your provider discoverable globally
- [Secure your provider](./security.md) - TLS, authentication, and firewall configuration
- [Monitor and metrics](./monitoring.md) - Track usage and performance

## Troubleshooting

**Provider won't start:**
- Check that the artifacts directory exists and is readable
- Verify that port 8080 (or custom port) is not already in use
- Check file permissions on the artifacts directory

**Artifacts not found:**
- Verify directory structure matches: `artifacts/<channel>/<arch>/{kernel,initramfs}`
- Check file names: must be exactly `kernel` and `initramfs` (no extensions)
- Ensure architecture matches: `aarch64` on ARM64, `x86_64` on x86-64

**Can't connect to provider:**
- Check firewall rules allow incoming connections on port 8080
- Verify provider is bound to correct address (use `0.0.0.0` for all interfaces)
- Try `curl http://localhost:8080/health` from the same machine first

**Need help?**
- Check logs with `RUST_LOG=debug plasmd serve`
- Open an issue at https://github.com/phase/phase/issues
