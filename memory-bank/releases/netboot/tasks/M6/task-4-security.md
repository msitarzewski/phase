# Task 4 — Security Best Practices

**Agent**: Docs Agent
**Estimated**: 1 day

## 4.1 Create security guide

- [ ] Create `docs/security.md`:

```markdown
# Security Best Practices for Phase Boot Providers

## Key Management

### Protect Your Signing Key

Your Ed25519 signing key is critical:
- It proves your identity to clients
- Compromised key = attackers can serve malicious artifacts

**Recommendations**:
\`\`\`bash
# Key stored at (platform-specific):
# macOS: ~/Library/Application Support/plasm/identity.key
# Linux: /var/lib/plasm/identity.key

# Restrict permissions
chmod 600 /var/lib/plasm/identity.key
chown plasm:plasm /var/lib/plasm/identity.key
\`\`\`

### Key Rotation

If you suspect key compromise:
1. Generate new key
2. Update manifests with new signature
3. Re-publish to DHT
4. Notify clients (if applicable)

## Network Security

### Firewall Configuration

Only expose necessary ports:
\`\`\`bash
# UFW (Ubuntu)
sudo ufw allow 8080/tcp  # HTTP artifacts
sudo ufw allow 5353/udp  # mDNS (LAN only)
sudo ufw allow 4001/tcp  # libp2p (if DHT)
\`\`\`

### TLS (Future)

Currently HTTP only. For sensitive deployments:
- Put behind reverse proxy with TLS
- Use VPN for artifact transfer

## Artifact Integrity

### Always Verify Hashes

Clients verify SHA256 hashes, but providers should too:
\`\`\`bash
# After copying new artifacts
sha256sum /var/lib/plasm/boot-artifacts/stable/arm64/*

# Regenerate manifest
plasmd manifest generate --sign
\`\`\`

### Source Verification

Only serve artifacts you trust:
- Build from source yourself
- Verify upstream signatures
- Don't serve random downloads

## Running as Non-Root

\`\`\`bash
# Create dedicated user
sudo useradd -r -s /bin/false plasm

# Own artifacts directory
sudo chown -R plasm:plasm /var/lib/plasm

# Run as plasm user
sudo -u plasm plasmd serve
\`\`\`

## Monitoring

Watch for suspicious activity:
\`\`\`bash
# Check request logs
journalctl -u plasmd-provider | grep -i error

# Monitor bandwidth
nethogs  # See which processes using network
\`\`\`

## Threat Model

| Threat | Mitigation |
|--------|------------|
| Malicious artifacts | Ed25519 signatures, SHA256 hashes |
| MITM attacks | Signature verification, future TLS |
| Key compromise | Key rotation, monitoring |
| DoS | Rate limiting, multiple providers |
| Unauthorized access | Firewall, authentication (future) |
```

**Dependencies**: M5 complete
**Output**: Security guide

---

## Validation Checklist

- [ ] Key management covered
- [ ] Network security explained
- [ ] Best practices actionable
- [ ] Threat model documented
