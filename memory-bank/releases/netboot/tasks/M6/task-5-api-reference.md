# Task 5 — API Reference

**Agent**: Docs Agent
**Estimated**: 1 day

## 5.1 Create HTTP API reference

- [ ] Create `docs/api-reference.md`:

```markdown
# Phase Boot Provider API Reference

## HTTP Endpoints

### GET /

Provider information.

**Response**:
\`\`\`json
{
  "name": "plasmd",
  "version": "0.1.0",
  "channel": "stable",
  "arch": "arm64",
  "uptime_secs": 3600
}
\`\`\`

### GET /health

Health check for load balancers.

**Response**: `200 OK` or `503 Service Unavailable`

\`\`\`json
{
  "status": "healthy",
  "checks": {
    "artifacts_readable": true,
    "dht_connected": true,
    "disk_space_ok": true
  }
}
\`\`\`

### GET /status

Detailed provider status.

**Response**:
\`\`\`json
{
  "provider": {
    "version": "0.1.0",
    "channel": "stable",
    "arch": "arm64",
    "uptime_secs": 3600,
    "peer_id": "12D3KooW..."
  },
  "artifacts": {
    "available": [
      {
        "name": "kernel",
        "channel": "stable",
        "arch": "arm64",
        "size_bytes": 8388608,
        "hash": "sha256:abc..."
      }
    ],
    "total_size_bytes": 12582912
  },
  "metrics": {
    "requests_total": 1000,
    "bytes_served_total": 10737418240
  }
}
\`\`\`

### GET /manifest.json

Boot manifest for default channel/arch.

**Response**: Signed manifest JSON

### GET /:channel/:arch/manifest.json

Boot manifest for specific channel/arch.

**Parameters**:
- `channel`: Release channel (stable, testing)
- `arch`: Architecture (arm64, x86_64)

### GET /:channel/:arch/:artifact

Download boot artifact.

**Parameters**:
- `artifact`: One of: kernel, initramfs, rootfs

**Headers**:
- `Content-Type`: application/octet-stream
- `Content-Length`: File size
- `X-Content-SHA256`: SHA256 hash

**Range Requests**: Supported via `Range` header

## CLI Commands

### plasmd serve

Start provider.

\`\`\`
plasmd serve [OPTIONS]

OPTIONS:
    -a, --artifacts <PATH>    Artifacts directory [default: /var/lib/plasm/boot-artifacts]
    -c, --channel <CHANNEL>   Channel [default: stable]
    -A, --arch <ARCH>         Architecture [default: auto]
    -p, --port <PORT>         HTTP port [default: 8080]
    -C, --config <FILE>       Config file
        --no-dht              Disable DHT
        --no-mdns             Disable mDNS
\`\`\`

### plasmd provider status

Show provider status.

\`\`\`
plasmd provider status [--json]
\`\`\`

### plasmd provider list

List advertised artifacts.

### plasmd provider keyid

Show signing key ID.

### plasmd manifest generate

Generate manifest from artifacts.

\`\`\`
plasmd manifest generate --artifacts <PATH> --output <FILE> [--sign]
\`\`\`
```

**Dependencies**: M5 complete
**Output**: API reference

---

## 5.2 Create configuration reference

- [ ] Document all config options
- [ ] Show example configurations
- [ ] Platform-specific defaults

**Dependencies**: Task 5.1
**Output**: Config reference

---

## Validation Checklist

- [ ] All HTTP endpoints documented
- [ ] All CLI commands documented
- [ ] Request/response examples
- [ ] Configuration options complete
