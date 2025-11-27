# Task 1 — Quickstart Guide

**Agent**: Docs Agent
**Estimated**: 1 day

## 1.1 Create quickstart document

- [ ] Create `docs/provider-quickstart.md`:

```markdown
# Phase Boot Provider Quickstart

Get a boot artifact provider running in 5 minutes.

## Prerequisites

- macOS (ARM64) or Linux (x86_64)
- Rust toolchain (for building)
- Boot artifacts (kernel, initramfs)

## Step 1: Install plasmd

### From Source
\`\`\`bash
git clone https://github.com/phase/phase
cd phase/daemon
cargo build --release
sudo cp target/release/plasmd /usr/local/bin/
\`\`\`

### From Package (coming soon)
\`\`\`bash
# macOS
brew install phase/tap/plasmd

# Debian/Ubuntu
sudo dpkg -i plasmd_*.deb
\`\`\`

## Step 2: Prepare Artifacts

\`\`\`bash
# Create artifacts directory
sudo mkdir -p /var/lib/plasm/boot-artifacts/stable/arm64

# Copy your boot files
sudo cp vmlinuz /var/lib/plasm/boot-artifacts/stable/arm64/
sudo cp initramfs.img /var/lib/plasm/boot-artifacts/stable/arm64/
\`\`\`

## Step 3: Start Provider

\`\`\`bash
plasmd serve --artifacts /var/lib/plasm/boot-artifacts
\`\`\`

You should see:
\`\`\`
╔══════════════════════════════════════════════╗
║           Phase Boot Provider                ║
╠══════════════════════════════════════════════╣
║ HTTP:     http://0.0.0.0:8080                ║
║ DHT:      enabled                            ║
║ mDNS:     enabled                            ║
╚══════════════════════════════════════════════╝
\`\`\`

## Step 4: Verify

\`\`\`bash
# Check health
curl http://localhost:8080/health

# View manifest
curl http://localhost:8080/manifest.json | jq

# Check status
plasmd provider status
\`\`\`

## Next Steps

- [Configure multiple channels](./configuration.md)
- [Run as a service](./systemd.md)
- [Secure your provider](./security.md)
```

**Dependencies**: M5 complete
**Output**: Quickstart document

---

## 1.2 Create macOS-specific guide

- [ ] Create `docs/provider-quickstart-macos.md` with:
  - Homebrew installation
  - macOS firewall configuration
  - Launch agent setup
  - DNS-SD verification

**Dependencies**: Task 1.1
**Output**: macOS guide

---

## 1.3 Create Linux-specific guide

- [ ] Create `docs/provider-quickstart-linux.md` with:
  - Package installation
  - systemd service setup
  - Firewall (ufw/firewalld) configuration
  - Avahi verification

**Dependencies**: Task 1.2
**Output**: Linux guide

---

## Validation Checklist

- [ ] New user can follow quickstart in <5 minutes
- [ ] All commands tested and working
- [ ] Screenshots/output examples included
- [ ] Links to next steps
