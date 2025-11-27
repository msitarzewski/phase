# Phase Boot Provider Security Guide

## Security Model Overview

Phase Boot Provider implements a multi-layered security model for distributed network boot:

1. **Discovery** (libp2p DHT/mDNS): Untrusted - provides provider locations only
2. **Transport** (HTTP): Integrity via hash verification (HTTPS optional for confidentiality)
3. **Artifacts**: Authenticated via Ed25519 signatures, integrity via SHA256 hashes
4. **Execution**: Kernel/initramfs verified before boot

### Trust Model

```
┌─────────────────────────────────────────────────────────────┐
│                      Trust Boundaries                        │
└─────────────────────────────────────────────────────────────┘

UNTRUSTED                VERIFIED                 TRUSTED
┌────────────┐          ┌────────────┐          ┌────────────┐
│    DHT     │          │ Manifest   │          │  Booted    │
│  Records   │────────> │ Signatures │────────> │   System   │
│            │          │  + Hashes  │          │            │
└────────────┘          └────────────┘          └────────────┘
     │                        │                       │
     │ Discovery              │ Authentication        │ Execution
     │ (libp2p)               │ (Ed25519 + SHA256)    │ (Kernel)
     │                        │                       │
     └────────────────────────┴───────────────────────┘
                    Verification Chain
```

### Security Guarantees

| Component | Guarantee | Mechanism |
|-----------|-----------|-----------|
| **Artifact Integrity** | Tamper-proof | SHA256 hash verification |
| **Manifest Authenticity** | Trusted source | Ed25519 signature verification |
| **Discovery Privacy** | Optional anonymity | Ephemeral peer identities |
| **Transport Security** | Optional encryption | HTTPS (future: Noise protocol) |

---

## Cryptographic Signatures

### Ed25519 Signing

Phase uses Ed25519 for manifest signing:

**Properties**:
- **Security**: 128-bit security level
- **Performance**: Fast verification (~70,000 verifications/sec)
- **Key Size**: 32-byte private key, 32-byte public key
- **Signature Size**: 64 bytes

### Key Management

#### Generating Signing Keys

**Development/Testing**:
```bash
# Generate new keypair
# Keys are stored in ./keys/ directory
plasmd generate-key --output keys/dev-signing-key.pem

# Key format: PEM-encoded Ed25519
```

**Production**:
```bash
# Generate on secure, offline machine
# Use hardware security module (HSM) if available
plasmd generate-key --output /secure/offline/storage/prod-key.pem

# Store private key securely:
# - Encrypt at rest
# - Restrict file permissions (chmod 400)
# - Backup to secure location
# - Consider multi-sig for critical deployments
```

#### Key Storage Best Practices

**Private Keys**:
```bash
# Restrict permissions (owner read-only)
chmod 400 /path/to/private-key.pem
chown plasm:plasm /path/to/private-key.pem

# Store outside web root
# NEVER in /var/lib/plasm/artifacts or /var/www/

# Consider encrypted filesystem
# - LUKS (Linux)
# - FileVault (macOS)
# - BitLocker (Windows)

# Backup securely
# - Offline storage (USB, paper wallet)
# - Encrypted backup
# - Multiple geographic locations
```

**Public Keys**:
```bash
# Distribute freely
# Include in documentation
# Publish via trusted channels (website, Git repo)

# Format: Hex-encoded 32-byte key
# Example: abc123def456...

# Derive key_id for verification:
# key_id = sha256(public_key)[0:16]
```

#### Key Rotation

**When to Rotate**:
- Regular schedule (e.g., annually)
- Suspected key compromise
- Personnel changes (access revocation)
- After major security incident

**Rotation Procedure**:
```bash
# 1. Generate new key
plasmd generate-key --output keys/new-key.pem

# 2. Dual-sign manifests during transition
plasmd serve --signing-keys keys/old-key.pem,keys/new-key.pem

# 3. Update clients to trust new key
# (Publish new public key via trusted channel)

# 4. After transition period (30 days)
plasmd serve --signing-keys keys/new-key.pem

# 5. Securely destroy old key
shred -vfz -n 3 keys/old-key.pem
```

#### Multi-Signature Quorum

For critical deployments, require N-of-M signatures:

**Example: 2-of-3 Multi-sig**:
```bash
# Three keys: admin, security, operations
# Manifests require any 2 signatures

# Admin signs
plasmd sign-manifest \
  --manifest manifest.json \
  --key keys/admin.pem \
  --output manifest-admin.json

# Security signs
plasmd sign-manifest \
  --manifest manifest-admin.json \
  --key keys/security.pem \
  --output manifest-final.json

# Verification requires both signatures
# Client trusts: 2 of {admin_key, security_key, ops_key}
```

---

## Artifact Integrity

### Hash Verification

All artifacts have SHA256 hashes in the manifest:

```json
{
  "artifacts": {
    "kernel": {
      "hash": "sha256:abc123def456...",
      "filename": "vmlinuz",
      "size_bytes": 8388608
    }
  }
}
```

**Verification Process**:
```bash
# 1. Download artifact
curl -O http://provider:8080/stable/x86_64/vmlinuz

# 2. Compute hash
computed_hash=$(sha256sum vmlinuz | awk '{print $1}')

# 3. Compare with manifest
manifest_hash=$(curl -s http://provider:8080/manifest.json | \
  jq -r '.artifacts.kernel.hash' | cut -d: -f2)

# 4. Verify match
if [ "$computed_hash" = "$manifest_hash" ]; then
  echo "✓ Hash verified"
else
  echo "✗ Hash mismatch - DO NOT BOOT"
  exit 1
fi
```

**Automated Verification**:
```bash
# Use phase-verify for automatic hash checking
phase-verify manifest.json vmlinuz
```

### Hash Algorithm Selection

**Current**: SHA256 (256-bit security)

**Future Support**:
- SHA3-256 (alternative 256-bit)
- BLAKE3 (faster, same security)
- SHA512 (512-bit, overkill for most use cases)

**Migration Path**:
```json
{
  "artifacts": {
    "kernel": {
      "hash": "sha256:abc123...",
      "hashes": {
        "sha256": "abc123...",
        "blake3": "def456..."
      }
    }
  }
}
```

---

## Network Security

### Firewall Configuration

#### Provider Server

**Minimum Required Ports**:
```bash
# HTTP artifact serving
sudo ufw allow 8080/tcp comment 'Phase Boot Provider HTTP'

# Optional: HTTPS (if configured)
sudo ufw allow 8443/tcp comment 'Phase Boot Provider HTTPS'
```

**Full Discovery Stack**:
```bash
# HTTP
sudo ufw allow 8080/tcp

# mDNS discovery (when implemented)
sudo ufw allow 5353/udp comment 'mDNS'

# libp2p DHT (variable ports)
# Option 1: Allow range
sudo ufw allow 4000:4100/tcp comment 'libp2p DHT'

# Option 2: Specific port
sudo ufw allow 4001/tcp comment 'libp2p DHT'
```

**Restrictive Configuration** (known clients only):
```bash
# Allow only specific subnet
sudo ufw allow from 192.168.1.0/24 to any port 8080

# Allow specific IP
sudo ufw allow from 192.168.1.100 to any port 8080

# Deny all other
sudo ufw default deny incoming
```

#### Client (Boot Environment)

**Outbound Only**:
```bash
# Clients only need outbound HTTP(S)
# No firewall changes required for egress

# If ingress firewall is strict, allow established connections
sudo ufw allow out 8080/tcp
sudo ufw allow out 8443/tcp
```

### TLS/HTTPS Configuration

**Note**: Current implementation uses HTTP. HTTPS support planned for future releases.

**Future Configuration**:
```bash
# Generate TLS certificate
openssl req -x509 -newkey rsa:4096 \
  -keyout server.key -out server.crt \
  -days 365 -nodes

# Start with HTTPS
plasmd serve \
  --tls-cert server.crt \
  --tls-key server.key \
  --port 8443

# Clients verify certificate
curl --cacert trusted-ca.crt https://provider:8443/manifest.json
```

**Certificate Pinning** (future):
```bash
# Pin provider's certificate fingerprint
# Prevents MITM with rogue CA

curl --pinnedpubkey 'sha256//abc123...' \
  https://provider:8443/manifest.json
```

### DDoS Mitigation

**Rate Limiting** (future feature):
```bash
# Limit requests per IP
# Example: 10 req/sec per IP, 100 req/sec global
plasmd serve \
  --rate-limit-per-ip 10 \
  --rate-limit-global 100
```

**Current Mitigation**:
```bash
# Use reverse proxy (nginx, HAProxy) for rate limiting
# Example nginx config:

# /etc/nginx/sites-available/plasmd
limit_req_zone $binary_remote_addr zone=plasmd:10m rate=10r/s;

server {
  listen 80;
  location / {
    limit_req zone=plasmd burst=20;
    proxy_pass http://127.0.0.1:8080;
  }
}
```

**CDN/Caching**:
```bash
# Use CDN to absorb traffic and reduce load
# Manifest caching: 5 minutes (TTL 300)
# Artifact caching: Immutable (TTL 86400)

# Example Cloudflare config:
# Cache artifacts aggressively
# Cache manifests with short TTL
```

---

## Running as Non-Root

**Security Principle**: Never run network services as root

### Systemd Service (Linux)

**Service Configuration** (`/etc/systemd/system/plasmd.service`):
```ini
[Unit]
Description=Phase Boot Provider
After=network.target

[Service]
Type=simple
User=plasm
Group=plasm
ExecStart=/usr/bin/plasmd serve --artifacts /var/lib/plasm/artifacts
Restart=on-failure
RestartSec=10

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/plasm/artifacts
CapabilityBoundingSet=

# Only needed if binding to port < 1024
# AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

**Setup**:
```bash
# Create dedicated user
sudo useradd -r -s /bin/false plasm

# Create artifacts directory
sudo mkdir -p /var/lib/plasm/artifacts
sudo chown -R plasm:plasm /var/lib/plasm

# Install and start service
sudo systemctl daemon-reload
sudo systemctl enable plasmd
sudo systemctl start plasmd
```

### Port Binding < 1024

**Problem**: Non-root users cannot bind to ports < 1024 (e.g., port 80)

**Solution 1 - Use High Port** (Recommended):
```bash
# Run on port 8080 (no special privileges)
plasmd serve --port 8080

# Use reverse proxy for port 80
# nginx/HAProxy/Caddy -> localhost:8080
```

**Solution 2 - CAP_NET_BIND_SERVICE**:
```bash
# Grant capability to binary
sudo setcap 'cap_net_bind_service=+ep' /usr/bin/plasmd

# Now can bind to port 80 as non-root
sudo -u plasm plasmd serve --port 80
```

**Solution 3 - iptables Redirect**:
```bash
# Redirect port 80 -> 8080
sudo iptables -t nat -A PREROUTING -p tcp --dport 80 -j REDIRECT --to-port 8080

# Run on 8080 as non-root
sudo -u plasm plasmd serve --port 8080
```

---

## Privacy Considerations

### Ephemeral Identity Mode

**Use Case**: Boot without creating persistent peer identity

**Enable**:
```bash
# Discovery uses temporary keypair
phase-discover --ephemeral --channel stable

# No persistent identity
# New peer ID every boot
# Reduces tracking across sessions
```

**Trade-offs**:
- **Privacy**: No long-term peer identity
- **Cost**: No reputation or peer history
- **Performance**: Cold start DHT every time

### Metadata Leakage

**Information Disclosed**:
1. **DHT Queries**: Channel and architecture requested
2. **HTTP Requests**: Provider learns client IP, user-agent, download patterns
3. **Timing**: When artifacts are fetched (correlates with boot times)

**Mitigation**:

**Use Tor/VPN** (future):
```bash
# Route discovery and downloads through Tor
torsocks phase-discover --channel stable
torsocks phase-fetch http://provider.onion/manifest.json
```

**Request Padding** (future):
```bash
# Pad requests to fixed sizes
# Prevents size-based fingerprinting
```

**Decoy Traffic** (future):
```bash
# Fetch random artifacts to mask real intent
phase-fetch --decoy --channel stable
```

### Traffic Analysis Resistance

**Current**: No specific resistance to traffic analysis

**Future Enhancements**:
- **Noise Protocol**: Encrypted transport with forward secrecy
- **Onion Routing**: Multi-hop relay for anonymity
- **Dummy Traffic**: Constant-rate dummy packets
- **Mixnets**: Delay and mixing for unlinkability

---

## Threat Model

### Threats and Mitigations

| Threat | Impact | Mitigation | Status |
|--------|--------|------------|--------|
| **Malicious DHT Records** | Misdirection to fake provider | Manifest signature verification | ✅ Implemented |
| **MITM Attack** | Serve malicious artifacts | Hash verification, HTTPS | 🟡 Partial (hash only) |
| **Compromised Provider** | Serve malicious artifacts | Signature verification | ✅ Implemented |
| **Manifest Tampering** | Modified artifact list | Ed25519 signatures | ✅ Implemented |
| **Artifact Corruption** | Unbootable or backdoored system | SHA256 hash verification | ✅ Implemented |
| **Replay Attack** | Serve outdated/vulnerable artifacts | Manifest expiration timestamp | ✅ Implemented |
| **Key Compromise** | Sign malicious manifests | Key rotation, multi-sig | 🟡 Manual process |
| **Network Eavesdropping** | Leak boot patterns | HTTPS, Noise protocol | ❌ Planned |
| **DHT Sybil Attack** | Eclipse attack, censorship | DHT security features | 🟡 Depends on libp2p |
| **DDoS on Provider** | Service unavailability | Rate limiting, CDN | ❌ Planned |
| **Timing Attack** | Infer sensitive information | Constant-time crypto | ✅ Ed25519 is constant-time |

**Legend**:
- ✅ Implemented
- 🟡 Partial / Manual
- ❌ Planned / Not implemented

### Attack Scenarios

#### Scenario 1: Malicious Provider

**Attack**: Attacker runs rogue provider, advertises in DHT

**Steps**:
1. Attacker creates DHT record: `/phase/stable/x86_64/manifest → http://evil.com/manifest.json`
2. Client discovers via DHT
3. Downloads manifest from evil.com
4. Manifest contains malicious kernel hash

**Mitigation**:
- Client verifies manifest signature with trusted public key
- Unsigned or incorrectly signed manifest is rejected
- Client only trusts manifests signed by known keys

**Result**: ✅ Attack mitigated if client verifies signatures

---

#### Scenario 2: Man-in-the-Middle

**Attack**: Network attacker intercepts HTTP traffic, injects malicious artifacts

**Steps**:
1. Client requests: `GET /stable/x86_64/vmlinuz`
2. Attacker intercepts, serves malicious kernel
3. Client downloads malicious artifact

**Mitigation**:
- Client computes SHA256 hash of downloaded artifact
- Compares with hash in signed manifest
- Hash mismatch → reject artifact

**Result**: ✅ Attack mitigated by hash verification

---

#### Scenario 3: Key Compromise

**Attack**: Attacker obtains provider's signing key

**Steps**:
1. Attacker steals private key from provider
2. Creates malicious manifest with valid signature
3. Advertises in DHT or serves via compromised provider
4. Clients download and verify signature (succeeds)

**Mitigation**:
- Detect compromise quickly (monitoring, IDS)
- Rotate keys immediately
- Publish key revocation (future: CRL/OCSP)
- Use multi-sig to require multiple keys

**Result**: 🟡 Partial mitigation (requires detection and manual intervention)

---

#### Scenario 4: Replay Attack

**Attack**: Attacker serves old, vulnerable version of artifacts

**Steps**:
1. Attacker obtains old signed manifest (e.g., from Git history)
2. Old manifest has known vulnerability in kernel
3. Serves old artifacts to clients

**Mitigation**:
- Manifests include `expires_at` timestamp
- Clients reject expired manifests
- Best practice: Set expiration to reasonable window (30 days)

**Result**: ✅ Attack mitigated if clients check expiration

---

## Security Best Practices

### For Provider Operators

1. **Key Management**:
   - Generate keys on secure, offline machine
   - Store private keys encrypted and access-controlled
   - Regular key rotation schedule (annual)
   - Multi-sig for critical deployments

2. **Infrastructure**:
   - Run as non-root user (dedicated `plasm` user)
   - Use systemd hardening (PrivateTmp, ProtectSystem, etc.)
   - Firewall: Only allow required ports
   - Regular security updates (OS, dependencies)

3. **Artifact Integrity**:
   - Verify artifact sources before hosting
   - Compute hashes on trusted system
   - Sign manifests offline if possible
   - Regular artifact audits

4. **Monitoring**:
   - Log all manifest requests
   - Monitor for unusual access patterns
   - Alert on signature verification failures
   - Track artifact download metrics

5. **Backups**:
   - Backup signing keys securely (encrypted, offline)
   - Backup artifacts to multiple locations
   - Document recovery procedures
   - Test recovery regularly

### For Client Users

1. **Verification**:
   - Always verify manifest signatures
   - Always verify artifact hashes
   - Reject expired manifests
   - Use trusted public keys only

2. **Discovery**:
   - Use ephemeral identity for privacy (`--ephemeral`)
   - Prefer local mDNS over global DHT when possible
   - Manually configure trusted providers if possible

3. **Network**:
   - Use HTTPS when available
   - Consider VPN/Tor for sensitive environments
   - Verify TLS certificates (when HTTPS implemented)

4. **Operational**:
   - Keep discovery tools updated
   - Monitor for unusual boot patterns
   - Report suspicious providers
   - Maintain multiple trusted providers (redundancy)

### For Developers

1. **Code Security**:
   - Follow secure coding practices
   - Use safe cryptographic libraries (ed25519-dalek, sha2)
   - Regular dependency audits (`cargo audit`)
   - Static analysis (clippy)

2. **Cryptography**:
   - Use well-tested libraries, not custom crypto
   - Constant-time operations for timing attack resistance
   - Regular security reviews of crypto code
   - Stay updated on cryptographic best practices

3. **Testing**:
   - Unit tests for signature verification
   - Integration tests for hash verification
   - Fuzzing for parser robustness
   - Security regression tests

4. **Documentation**:
   - Document security assumptions
   - Provide clear security guidelines
   - Publish threat model and mitigations
   - Responsible disclosure policy

---

## Incident Response

### Suspected Key Compromise

**Immediate Actions**:
1. **Isolate**: Disconnect compromised provider from network
2. **Rotate**: Generate new signing key immediately
3. **Notify**: Alert all clients to stop trusting old key
4. **Investigate**: Determine scope and vector of compromise
5. **Remediate**: Patch vulnerability, harden system
6. **Monitor**: Watch for malicious manifests signed with old key

### Malicious Artifact Detection

**Response Procedure**:
1. **Verify**: Confirm artifact is malicious (hash mismatch, malware scan)
2. **Alert**: Notify provider operator and community
3. **Remove**: Delete malicious artifacts from provider
4. **Investigate**: How did malicious artifact reach provider?
5. **Forensics**: Preserve logs and evidence
6. **Publish**: Issue security advisory

### Provider Compromise

**Response Procedure**:
1. **Disconnect**: Take provider offline immediately
2. **Preserve**: Create forensic image of system
3. **Analyze**: Identify compromise method and timeline
4. **Clean**: Rebuild provider from trusted source
5. **Harden**: Apply additional security measures
6. **Restore**: Bring provider back online with monitoring

---

## Compliance and Auditing

### Audit Logging

**Provider Logs** (future):
```json
{
  "timestamp": "2025-01-01T12:00:00Z",
  "event": "artifact_download",
  "client_ip": "192.168.1.100",
  "artifact": "stable/x86_64/vmlinuz",
  "size_bytes": 8388608,
  "hash_verified": true
}
```

**Retention Policy**:
- Keep logs for 90 days minimum
- Archive long-term for security incidents
- Redact PII if required by regulation

### Vulnerability Disclosure

**Reporting**:
- Email: security@phasebased.com (future)
- PGP Key: [To be published]
- Bug Bounty: [To be established]

**Response Timeline**:
- Initial response: 48 hours
- Triage: 7 days
- Fix: 30 days (critical), 90 days (non-critical)
- Disclosure: 90 days after fix available

### Security Updates

**Update Channels**:
- GitHub Security Advisories
- Mailing list (security-announce@phasebased.com)
- RSS feed
- In-app notifications (future)

**Update Policy**:
- Critical: Immediate release
- High: 7 days
- Medium: 30 days
- Low: Next scheduled release

---

## Future Security Enhancements

### Planned Features

1. **Noise Protocol Integration**:
   - Encrypted transport between client and provider
   - Forward secrecy
   - Mutual authentication

2. **Certificate Transparency**:
   - Public log of all manifests
   - Detect unauthorized manifests
   - Append-only audit log

3. **Key Revocation**:
   - CRL (Certificate Revocation List)
   - OCSP (Online Certificate Status Protocol)
   - Bloom filter for efficient revocation checks

4. **Hardware Security Module (HSM)**:
   - Store signing keys in HSM
   - PKCS#11 interface
   - Tamper-resistant key storage

5. **Reproducible Builds**:
   - Verify artifacts built from source
   - Multiple builders sign manifests
   - Consensus-based trust

### Under Consideration

- **Zero-Knowledge Proofs**: Prove artifact authenticity without revealing content
- **Homomorphic Encryption**: Search encrypted artifacts
- **Post-Quantum Cryptography**: Quantum-resistant signatures (e.g., Dilithium)
- **Secure Boot Integration**: UEFI Secure Boot validation
- **Intel TXT/AMD SEV**: Measured boot and attestation

---

## Related Documentation

- [Architecture](architecture.md) - Security model and trust boundaries
- [API Reference](api-reference.md) - Secure API usage
- [Troubleshooting](troubleshooting.md) - Security-related issues
