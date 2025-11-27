# Phase Boot Provider Troubleshooting

## Common Issues and Solutions

### Provider Server Issues

#### Port Already in Use

**Symptoms**:
```
Error: Address already in use (os error 48)
Error: Failed to bind to 0.0.0.0:8080
```

**Cause**: Another process is using port 8080 (or configured port)

**Solution**:

1. Find the process using the port:
```bash
# Linux/macOS
sudo lsof -i :8080
# or
sudo netstat -tlnp | grep 8080

# Windows
netstat -ano | findstr :8080
```

2. Stop the conflicting process or choose a different port:
```bash
# Use a different port
plasmd serve --port 8081

# Or stop the conflicting process
sudo kill <PID>
```

3. If plasmd is already running:
```bash
# Find and kill existing plasmd
pgrep plasmd
pkill plasmd

# Or use systemd (if installed)
sudo systemctl stop plasmd
```

---

#### No Artifacts Found

**Symptoms**:
```
Error: No artifacts found for channel/arch
HTTP 404 when accessing /stable/x86_64/manifest.json
```

**Cause**: Artifacts directory is empty or has incorrect structure

**Solution**:

1. Check artifacts directory exists and is readable:
```bash
ls -la /var/lib/plasm/artifacts
# or your configured directory
ls -la /path/to/artifacts
```

2. Verify correct directory structure:
```
artifacts/
└── stable/              # Channel name
    └── x86_64/          # Architecture
        ├── vmlinuz      # Required: kernel
        ├── initramfs.img  # Recommended
        └── rootfs.squashfs  # Optional
```

3. Create the required structure:
```bash
# Example for stable/x86_64
sudo mkdir -p /var/lib/plasm/artifacts/stable/x86_64

# Copy your artifacts
sudo cp /boot/vmlinuz /var/lib/plasm/artifacts/stable/x86_64/
sudo cp /boot/initramfs.img /var/lib/plasm/artifacts/stable/x86_64/
```

4. Verify permissions:
```bash
# Artifacts must be readable by the plasmd user
sudo chmod 644 /var/lib/plasm/artifacts/stable/x86_64/*
sudo chown -R plasm:plasm /var/lib/plasm/artifacts
```

5. Restart the provider:
```bash
plasmd serve --artifacts /var/lib/plasm/artifacts
```

**Debug Command**:
```bash
# List what the provider sees
plasmd provider list --addr http://localhost:8080

# Or query the manifest endpoint
curl http://localhost:8080/stable/x86_64/manifest.json
```

---

#### Permission Denied Reading Artifacts

**Symptoms**:
```
Error opening artifact file: Permission denied (os error 13)
HTTP 500 Internal Server Error when downloading artifacts
```

**Cause**: plasmd process cannot read artifact files

**Solution**:

1. Check file permissions:
```bash
ls -l /var/lib/plasm/artifacts/stable/x86_64/
```

2. Fix permissions:
```bash
# Make files readable
sudo chmod 644 /var/lib/plasm/artifacts/stable/x86_64/*

# If running as specific user (e.g., plasm)
sudo chown -R plasm:plasm /var/lib/plasm/artifacts
```

3. If running via systemd, check service user:
```bash
# View service configuration
systemctl cat plasmd

# Ensure User= directive matches artifact ownership
# Or run as root (not recommended for production)
```

4. SELinux/AppArmor issues (Linux):
```bash
# Check SELinux denials
sudo ausearch -m avc -ts recent

# Fix SELinux context
sudo semanage fcontext -a -t httpd_sys_content_t "/var/lib/plasm/artifacts(/.*)?"
sudo restorecon -Rv /var/lib/plasm/artifacts

# Or temporarily disable for testing
sudo setenforce 0  # TESTING ONLY
```

---

#### Health Check Failing

**Symptoms**:
```
HTTP 503 Service Unavailable from /health
{"status": "unhealthy", "checks": {"artifacts_readable": false}}
```

**Cause**: Artifacts directory not accessible or low disk space

**Solution**:

1. Check the status endpoint for details:
```bash
curl http://localhost:8080/status | jq '.health'
```

2. If `artifacts_readable` is false:
```bash
# Check directory exists
ls -la /var/lib/plasm/artifacts

# Check permissions
sudo chmod 755 /var/lib/plasm/artifacts
sudo chmod 755 /var/lib/plasm/artifacts/stable
sudo chmod 755 /var/lib/plasm/artifacts/stable/x86_64
```

3. If `disk_space_ok` is false:
```bash
# Check disk usage
df -h /var/lib/plasm/artifacts

# Free up space or use different directory
plasmd serve --artifacts /mnt/large-disk/artifacts
```

---

### Discovery Issues

#### mDNS Discovery Not Working

**Symptoms**:
```
No providers found via avahi-browse _phase-image._tcp
```

**Cause**: mDNS not yet implemented (placeholder) or network issues

**Solution**:

1. Check if mDNS is implemented:
```bash
# Look for warning in logs
plasmd serve 2>&1 | grep mDNS

# Expected warning:
# "mDNS service advertisement not yet implemented"
```

2. Current workaround - use direct HTTP access:
```bash
# Find provider IP via other means
# Then access directly
curl http://192.168.1.100:8080/manifest.json
```

3. Future: When mDNS is implemented, check firewall:
```bash
# Allow mDNS (port 5353 UDP)
sudo ufw allow 5353/udp
sudo firewall-cmd --add-service=mdns --permanent
```

4. Test mDNS functionality:
```bash
# Linux - install avahi tools
sudo apt-get install avahi-utils
avahi-browse -a

# macOS - built-in
dns-sd -B _phase-image._tcp
```

---

#### DHT Discovery Timeout

**Symptoms**:
```
phase-discover --channel stable --arch x86_64
Error: Discovery timeout after 30s
```

**Cause**: No bootstrap nodes, network isolation, or no providers advertising

**Solution**:

1. Check network connectivity:
```bash
# Ensure internet access (if using public bootstrap nodes)
ping 8.8.8.8

# Check firewall isn't blocking libp2p ports
sudo ufw status
```

2. Specify bootstrap nodes explicitly:
```bash
phase-discover \
  --bootstrap /ip4/192.168.1.100/tcp/4001/p2p/12D3Koo... \
  --channel stable
```

3. Verify a provider is actually advertising:
```bash
# Check provider is running
curl http://localhost:8080/status

# Verify DHT is enabled (not --no-dht)
# Check provider startup banner shows "DHT: enabled"
```

4. Increase timeout for slow networks:
```bash
phase-discover --timeout 60 --channel stable
```

5. Use JSON output for debugging:
```bash
phase-discover --format json --channel stable 2>&1 | jq .
```

6. Fallback to direct HTTP access:
```bash
# If discovery fails, use known provider directly
curl http://provider-ip:8080/stable/x86_64/manifest.json
```

---

### Download Issues

#### Hash Mismatch After Download

**Symptoms**:
```
phase-verify manifest.json vmlinuz
ERROR Hash mismatch for vmlinuz
  Expected: sha256:abc123...
  Computed: sha256:def456...
```

**Cause**: Corrupted download, MITM attack, or stale manifest

**Solution**:

1. Re-download the artifact:
```bash
# Delete corrupted file
rm vmlinuz

# Download again
phase-fetch http://provider:8080/manifest.json -a kernel
```

2. Verify manifest is current:
```bash
# Check manifest expiration
curl http://provider:8080/manifest.json | jq '.expires_at'

# Get fresh manifest
curl -O http://provider:8080/manifest.json
```

3. Check for network issues:
```bash
# Use direct download with verification
curl -H "Range: bytes=0-1023" \
  http://provider:8080/stable/x86_64/vmlinuz | sha256sum

# Compare with manifest hash
```

4. If hash is consistently wrong:
```bash
# Provider may have updated artifacts without updating manifest
# Contact provider administrator

# Or compute fresh manifest
# (if you have access to provider)
plasmd serve --artifacts /path/to/artifacts
```

5. Security concern: If hash mismatch persists, assume compromise:
```bash
# DO NOT BOOT with mismatched hash
# Report to provider administrator
# Use alternate provider
```

---

#### Download Interrupted / Connection Reset

**Symptoms**:
```
curl: (56) Recv failure: Connection reset by peer
Partial download of vmlinuz (received 4.2 MB of 8.0 MB)
```

**Cause**: Network instability, provider restart, or firewall timeout

**Solution**:

1. Use resume functionality (HTTP Range):
```bash
# Check how much was downloaded
ls -lh vmlinuz

# Resume from where it stopped
curl -C - -O http://provider:8080/stable/x86_64/vmlinuz

# Or with phase-fetch
phase-fetch --resume http://provider:8080/manifest.json
```

2. Manual range request:
```bash
# If you know you have bytes 0-4194303 (4 MB)
curl -H "Range: bytes=4194304-" \
  http://provider:8080/stable/x86_64/vmlinuz >> vmlinuz
```

3. Check provider logs for errors:
```bash
# On provider server
journalctl -u plasmd -n 100

# Or provider stderr
plasmd serve 2>&1 | grep ERROR
```

4. Test connection stability:
```bash
# Download small test file first
curl -O http://provider:8080/manifest.json

# Ping provider
ping provider-ip

# Check for packet loss
mtr provider-ip
```

---

#### Download Speed Very Slow

**Symptoms**:
```
Downloading vmlinuz: 50 KB/s (expected: several MB/s)
```

**Cause**: Network congestion, provider CPU/disk bottleneck, or bandwidth limits

**Solution**:

1. Check network path:
```bash
# Test bandwidth to provider
iperf3 -c provider-ip

# Check latency
ping provider-ip
mtr provider-ip
```

2. Try different provider (if available):
```bash
# Discover alternate providers
phase-discover --channel stable

# Use alternate provider URL
phase-fetch http://other-provider:8080/manifest.json
```

3. Use parallel downloads (future feature):
```bash
# Current: Single TCP stream
# Future: Multiple range requests in parallel

# Manual workaround: Split file
# Download ranges 0-4MB, 4MB-8MB in parallel
curl -H "Range: bytes=0-4194303" ... > vmlinuz.part1 &
curl -H "Range: bytes=4194304-8388607" ... > vmlinuz.part2 &
wait
cat vmlinuz.part1 vmlinuz.part2 > vmlinuz
```

4. Check provider server load:
```bash
# On provider server
curl http://localhost:8080/status | jq '.metrics'

# Check CPU/disk usage
htop
iotop
```

---

### Manifest Issues

#### Manifest Expired

**Symptoms**:
```
Warning: Manifest expired at 2025-01-31T23:59:59Z
```

**Cause**: Provider hasn't regenerated manifest (old artifacts)

**Solution**:

1. Check current time vs. expiration:
```bash
curl http://provider:8080/manifest.json | jq '.expires_at'
date -u
```

2. Request fresh manifest from provider:
```bash
# Manifests are generated on-demand
# Simply fetch again to get updated timestamps
curl http://provider:8080/stable/x86_64/manifest.json
```

3. If provider is under your control:
```bash
# Restart provider to regenerate manifests
sudo systemctl restart plasmd

# Or verify manifest expiry is reasonable (30 days default)
```

4. If expiration is critical (e.g., security policy):
```bash
# Reject expired manifests
if [ $(date +%s) -gt $(date -d "2025-01-31T23:59:59Z" +%s) ]; then
  echo "Manifest expired, refusing to use"
  exit 1
fi
```

---

#### Missing Required Artifacts in Manifest

**Symptoms**:
```
Error: Manifest missing required artifact: kernel
```

**Cause**: Incomplete artifact set on provider

**Solution**:

1. Check manifest structure:
```bash
curl http://provider:8080/manifest.json | jq '.artifacts | keys'

# Required: ["kernel"]
# Recommended: ["kernel", "initramfs"]
```

2. Add missing artifacts to provider:
```bash
# On provider server
sudo cp /boot/vmlinuz /var/lib/plasm/artifacts/stable/x86_64/
sudo systemctl restart plasmd
```

3. Verify updated manifest:
```bash
curl http://provider:8080/stable/x86_64/manifest.json | \
  jq '.artifacts.kernel'
```

---

### Signature Verification Issues

#### Signature Verification Failed

**Symptoms**:
```
Error: Invalid signature on manifest
Signature verification failed for key_id: abc123...
```

**Cause**: Unsigned manifest, wrong key, or manifest tampering

**Solution**:

1. Check if manifest has signatures:
```bash
curl http://provider:8080/manifest.json | jq '.signatures'

# Should return array of signature objects
# Empty array = unsigned manifest
```

2. If unsigned is acceptable (testing/development):
```bash
# Skip signature verification (TESTING ONLY)
# Production: Always verify signatures
```

3. Verify key_id matches expected signing key:
```bash
# Check manifest key_id
curl http://provider:8080/manifest.json | \
  jq '.signatures[0].key_id'

# Compare with trusted key ID
# If mismatch, manifest signed by different key
```

4. Get provider's public key:
```bash
# Contact provider administrator for public key
# Verify key_id matches: sha256(public_key)[0:16]
```

5. If signature is invalid:
```bash
# DO NOT USE MANIFEST
# Possible tampering or corruption
# Report to provider administrator
# Use alternate provider
```

---

## Debug Commands

### Provider Health Check

```bash
# Quick health check
curl -s http://localhost:8080/health | jq .

# Detailed status
curl -s http://localhost:8080/status | jq .

# Check specific health aspect
curl -s http://localhost:8080/status | \
  jq '.health.checks.artifacts_readable'
```

### Manifest Inspection

```bash
# View full manifest
curl -s http://localhost:8080/manifest.json | jq .

# List available artifacts
curl -s http://localhost:8080/manifest.json | \
  jq '.artifacts | keys'

# Check artifact hash
curl -s http://localhost:8080/manifest.json | \
  jq '.artifacts.kernel.hash'

# Verify expiration
curl -s http://localhost:8080/manifest.json | \
  jq '.expires_at'
```

### Network Connectivity

```bash
# Test HTTP connectivity
curl -I http://provider:8080/

# Test manifest download
curl -s http://provider:8080/manifest.json | jq '.version'

# Test artifact download (small chunk)
curl -H "Range: bytes=0-1023" \
  http://provider:8080/stable/x86_64/vmlinuz | wc -c

# Test DHT connectivity (phase-discover)
phase-discover --timeout 10 --quiet
```

### Artifact Verification

```bash
# Compute artifact hash
sha256sum vmlinuz

# Compare with manifest
curl -s http://provider:8080/manifest.json | \
  jq -r '.artifacts.kernel.hash'

# Automated verification
phase-verify manifest.json vmlinuz
```

### Provider Logs

```bash
# Systemd service logs
sudo journalctl -u plasmd -f

# Last 100 lines
sudo journalctl -u plasmd -n 100

# Errors only
sudo journalctl -u plasmd -p err

# Manual run with verbose output
RUST_LOG=debug plasmd serve --artifacts /tmp/artifacts
```

### Network Discovery

```bash
# mDNS discovery (when implemented)
avahi-browse _phase-image._tcp
dns-sd -B _phase-image._tcp

# DHT discovery
phase-discover --channel stable --arch x86_64 --format json

# Direct manifest lookup
curl http://provider-ip:8080/manifest.json
```

---

## Performance Debugging

### Slow Manifest Generation

**Symptoms**: `/manifest.json` takes several seconds to respond

**Debug**:
```bash
# Time manifest generation
time curl http://localhost:8080/manifest.json > /dev/null

# Check artifact count
ls -1 /var/lib/plasm/artifacts/*/* | wc -l

# Check filesystem performance
time sha256sum /var/lib/plasm/artifacts/stable/x86_64/vmlinuz
```

**Solution**:
- Manifests are generated on-demand
- Future: Add manifest caching
- Reduce artifact count if excessive
- Use faster storage (SSD vs HDD)

---

### High Memory Usage

**Symptoms**: plasmd consuming excessive RAM

**Debug**:
```bash
# Check process memory
ps aux | grep plasmd

# Check memory details
top -p $(pgrep plasmd)
```

**Solution**:
- Normal: ~10-50 MB for provider server
- High: Streaming downloads shouldn't buffer in memory
- Check for memory leaks: Monitor over time
- File descriptor leak: `lsof -p $(pgrep plasmd) | wc -l`

---

### High CPU Usage

**Symptoms**: plasmd consuming significant CPU

**Debug**:
```bash
# Check CPU usage
top -p $(pgrep plasmd)

# Check request rate
curl http://localhost:8080/status | jq '.metrics.requests_total'

# Profile with perf (Linux)
sudo perf record -p $(pgrep plasmd) -g -- sleep 10
sudo perf report
```

**Solution**:
- Normal: CPU spikes during hash computation
- Sustained high: Check request rate (DDoS?)
- Optimize: Use hash caching (future)

---

## Getting Help

### Information to Provide

When reporting issues, include:

1. **Version information**:
```bash
plasmd --version
```

2. **System information**:
```bash
uname -a
lsb_release -a  # Linux
sw_vers  # macOS
```

3. **Provider configuration**:
```bash
# How you started the provider
plasmd serve --artifacts /path/to/artifacts --port 8080

# Artifacts structure
tree /var/lib/plasm/artifacts
```

4. **Error messages**:
```bash
# Full error output
plasmd serve 2>&1 | tee error.log

# Or systemd logs
sudo journalctl -u plasmd -n 100
```

5. **Network information**:
```bash
# Provider IP and port
ip addr
ss -tlnp | grep plasmd

# Firewall rules
sudo ufw status verbose
sudo iptables -L -n
```

6. **Manifest and artifacts**:
```bash
# Manifest JSON
curl http://localhost:8080/manifest.json | jq .

# Artifact list
plasmd provider list
```

### Support Channels

- GitHub Issues: https://github.com/phasebased/phase/issues
- Documentation: See [README.md](../README.md)
- Architecture: See [architecture.md](architecture.md)
- API Reference: See [api-reference.md](api-reference.md)

---

## Related Documentation

- [Architecture](architecture.md) - System design and discovery flows
- [API Reference](api-reference.md) - HTTP endpoints and CLI commands
- [Security](security.md) - Security best practices and threat model
