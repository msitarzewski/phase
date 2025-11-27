# Task 3 — Troubleshooting Guide

**Agent**: Docs Agent
**Estimated**: 1 day

## 3.1 Create troubleshooting document

- [ ] Create `docs/troubleshooting.md`:

```markdown
# Troubleshooting Phase Boot Provider

## Provider Won't Start

### Port Already in Use
\`\`\`
Error: Port 8080 is already in use
\`\`\`

**Solution**: Use a different port or stop the other service
\`\`\`bash
plasmd serve --port 8081
# Or find what's using 8080:
lsof -i :8080
\`\`\`

### No Artifacts Found
\`\`\`
Error: No artifacts found in /var/lib/plasm/boot-artifacts
\`\`\`

**Solution**: Create artifact directory structure
\`\`\`bash
mkdir -p /var/lib/plasm/boot-artifacts/stable/arm64
cp vmlinuz initramfs.img /var/lib/plasm/boot-artifacts/stable/arm64/
\`\`\`

## Discovery Issues

### mDNS Not Working

**Symptoms**: `dns-sd -B _phase-image._tcp local.` shows nothing

**Check**:
1. Firewall allowing mDNS (UDP 5353)
2. Both machines on same subnet
3. mDNS enabled in provider config

**macOS**:
\`\`\`bash
sudo pfctl -d  # Temporarily disable firewall for testing
\`\`\`

### DHT Not Publishing

**Symptoms**: Provider logs show DHT errors

**Check**:
1. Internet connectivity
2. Bootstrap nodes reachable
3. Outbound UDP not blocked

**Debug**:
\`\`\`bash
RUST_LOG=libp2p=debug plasmd serve
\`\`\`

## Fetch Issues

### Hash Mismatch
\`\`\`
Error: Hash mismatch for kernel: expected sha256:abc..., got sha256:xyz...
\`\`\`

**Cause**: Artifact changed after manifest was generated

**Solution**: Regenerate manifest
\`\`\`bash
plasmd manifest generate --artifacts /path/to/artifacts --sign
\`\`\`

### Connection Refused
\`\`\`
Error: Failed to fetch from http://192.168.1.100:8080
\`\`\`

**Check**:
1. Provider is running
2. Firewall allows port 8080
3. Correct IP in discovery

## Performance Issues

### Slow Downloads

**Check**:
1. Network bandwidth
2. Disk I/O (provider)
3. Multiple concurrent downloads

**Improve**:
- Use SSD for artifacts
- Increase buffer sizes
- Run multiple providers

## Getting Help

1. Check logs: `journalctl -u plasmd-provider`
2. Enable debug: `RUST_LOG=debug plasmd serve`
3. File issue: https://github.com/phase/phase/issues
```

**Dependencies**: M5 complete
**Output**: Troubleshooting guide

---

## Validation Checklist

- [ ] Common issues covered
- [ ] Solutions tested
- [ ] Debug commands included
- [ ] Clear next steps for each issue
