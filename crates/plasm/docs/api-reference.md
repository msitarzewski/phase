# Phase Boot Provider API Reference

## HTTP API Endpoints

All endpoints use standard HTTP/1.1 with JSON or binary responses.

### GET /

Provider information endpoint.

**Response**: `200 OK`

```json
{
  "name": "plasmd-provider",
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "uptime_seconds": 3600
}
```

**Example**:
```bash
curl http://localhost:8080/

# Response:
{
  "name": "plasmd-provider",
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "uptime_seconds": 3600
}
```

---

### GET /health

Health check endpoint.

**Response**: `200 OK` (healthy) or `503 Service Unavailable` (unhealthy)

```json
{
  "status": "healthy",
  "checks": {
    "artifacts_readable": true,
    "disk_space_ok": true
  }
}
```

**Status Values**:
- `healthy`: All checks passed
- `unhealthy`: One or more checks failed

**Example**:
```bash
curl http://localhost:8080/health

# Healthy response (200):
{
  "status": "healthy",
  "checks": {
    "artifacts_readable": true,
    "disk_space_ok": true
  }
}

# Unhealthy response (503):
{
  "status": "unhealthy",
  "checks": {
    "artifacts_readable": false,
    "disk_space_ok": true
  }
}
```

**Use Cases**:
- Load balancer health checks
- Monitoring/alerting systems
- Readiness probes (Kubernetes)

---

### GET /status

Detailed provider status including metrics.

**Response**: `200 OK`

```json
{
  "name": "plasmd-provider",
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "uptime_seconds": 3600,
  "health": {
    "status": "healthy",
    "artifacts_readable": true,
    "disk_space_ok": true
  },
  "metrics": {
    "requests_total": 142,
    "bytes_served_total": 1073741824
  }
}
```

**Example**:
```bash
curl http://localhost:8080/status

# Response includes all provider metrics
```

**Use Cases**:
- Operations dashboard
- Capacity planning
- Performance monitoring

---

### GET /manifest.json

Default manifest for the configured channel and architecture.

**Response**: `200 OK`

```json
{
  "manifest_version": 1,
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "created_at": "2025-01-01T00:00:00Z",
  "expires_at": "2025-01-31T23:59:59Z",
  "artifacts": {
    "kernel": {
      "filename": "vmlinuz",
      "size_bytes": 8388608,
      "hash": "sha256:abc123...",
      "download_url": "stable/x86_64/vmlinuz"
    },
    "initramfs": {
      "filename": "initramfs.img",
      "size_bytes": 67108864,
      "hash": "sha256:def456...",
      "download_url": "stable/x86_64/initramfs.img"
    }
  },
  "signatures": [
    {
      "algorithm": "ed25519",
      "key_id": "abc123...",
      "signature": "def456...",
      "signed_at": "2025-01-01T00:00:00Z"
    }
  ]
}
```

**Example**:
```bash
curl http://localhost:8080/manifest.json

# Returns manifest for default channel/arch
```

**Use Cases**:
- Quick manifest access
- Default configuration discovery
- Simple client implementations

---

### GET /:channel/:arch/manifest.json

Channel and architecture-specific manifest.

**Path Parameters**:
- `channel`: Update channel (`stable`, `testing`, `dev`)
- `arch`: Target architecture (`x86_64`, `arm64`, `aarch64`)

**Response**: `200 OK` (found) or `404 Not Found` (no artifacts)

**Example**:
```bash
# Get stable x86_64 manifest
curl http://localhost:8080/stable/x86_64/manifest.json

# Get testing arm64 manifest
curl http://localhost:8080/testing/arm64/manifest.json

# No artifacts for this combination (404):
curl http://localhost:8080/dev/x86_64/manifest.json
# {"error": "No artifacts found for channel/arch"}
```

**Error Responses**:

`404 Not Found`:
```json
{
  "error": "No artifacts found for channel/arch"
}
```

`500 Internal Server Error`:
```json
{
  "error": "Failed to generate manifest"
}
```

---

### GET /:channel/:arch/:artifact

Download boot artifact.

**Path Parameters**:
- `channel`: Update channel
- `arch`: Target architecture
- `artifact`: Artifact filename (e.g., `vmlinuz`, `initramfs.img`)

**Response**: `200 OK` (full file) or `206 Partial Content` (range request)

**Response Headers**:
- `Content-Type`: `application/octet-stream`
- `Content-Length`: File size in bytes
- `Accept-Ranges`: `bytes`
- `X-Artifact-Hash`: SHA256 hash (e.g., `sha256:abc123...`)

**Example (Full Download)**:
```bash
curl -O http://localhost:8080/stable/x86_64/vmlinuz

# Response headers:
HTTP/1.1 200 OK
Content-Type: application/octet-stream
Content-Length: 8388608
Accept-Ranges: bytes
X-Artifact-Hash: sha256:abc123...

# Binary data follows...
```

**Example (Range Request)**:
```bash
# Resume from byte 1024
curl -H "Range: bytes=1024-" \
  http://localhost:8080/stable/x86_64/vmlinuz

# Response headers:
HTTP/1.1 206 Partial Content
Content-Type: application/octet-stream
Content-Range: bytes 1024-8388607/8388608
Content-Length: 8387584
Accept-Ranges: bytes
X-Artifact-Hash: sha256:abc123...

# Partial binary data follows...
```

**Example (Specific Range)**:
```bash
# Get bytes 1024-2047 (1KB chunk)
curl -H "Range: bytes=1024-2047" \
  http://localhost:8080/stable/x86_64/vmlinuz

# Response headers:
HTTP/1.1 206 Partial Content
Content-Range: bytes 1024-2047/8388608
Content-Length: 1024
```

**Range Request Formats**:
- `bytes=0-1023`: First 1024 bytes (0-1023 inclusive)
- `bytes=1024-`: From byte 1024 to end
- `bytes=1024-2047`: Bytes 1024-2047 (1KB chunk)

**Error Responses**:

`404 Not Found`: Artifact not found

`416 Range Not Satisfiable`:
```
HTTP/1.1 416 Range Not Satisfiable
Content-Range: bytes */8388608
Accept-Ranges: bytes
```

`500 Internal Server Error`: File read error

**Use Cases**:
- Resumable downloads
- Parallel downloads (multiple range requests)
- Bandwidth-limited environments
- Download verification (hash in header)

---

## CLI Commands

### plasmd serve

Start the boot artifact provider server.

**Synopsis**:
```bash
plasmd serve [OPTIONS]
```

**Options**:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--artifacts` | `-a` | Path | Platform default | Artifacts directory |
| `--channel` | `-c` | String | `stable` | Release channel |
| `--arch` | `-A` | String | Auto-detect | Target architecture |
| `--port` | `-p` | u16 | `8080` | HTTP port |
| `--bind` | `-b` | String | `0.0.0.0` | Bind address |
| `--no-dht` | | Flag | Disabled | Disable DHT advertisement |
| `--no-mdns` | | Flag | Disabled | Disable mDNS advertisement |

**Default Artifacts Directories**:
- Linux: `/var/lib/plasm/artifacts`
- macOS: `~/Library/Application Support/plasm/artifacts`
- Windows: `%LOCALAPPDATA%\plasm\artifacts`

**Examples**:

```bash
# Start with defaults (stable/auto-arch on port 8080)
plasmd serve

# Custom artifacts directory and channel
plasmd serve --artifacts /srv/boot --channel testing

# Specific architecture and port
plasmd serve --arch arm64 --port 9000

# Listen on specific interface
plasmd serve --bind 192.168.1.100 --port 8080

# Disable network discovery (HTTP only)
plasmd serve --no-dht --no-mdns

# Development server (all options)
plasmd serve \
  --artifacts ./artifacts \
  --channel dev \
  --arch x86_64 \
  --port 8080 \
  --bind 127.0.0.1 \
  --no-dht \
  --no-mdns
```

**Output**:
```
╔══════════════════════════════════════════════╗
║           Phase Boot Provider                ║
╠══════════════════════════════════════════════╣
║ HTTP:     http://0.0.0.0:8080                ║
║ Artifacts: /var/lib/plasm/artifacts          ║
║ Channel:  stable                             ║
║ Arch:     x86_64                             ║
║ DHT:      enabled                            ║
║ mDNS:     enabled                            ║
╚══════════════════════════════════════════════╝

INFO Starting provider HTTP server on 0.0.0.0:8080
INFO Provider server listening on 0.0.0.0:8080
```

**Exit Codes**:
- `0`: Normal termination (Ctrl+C)
- `1`: Error (port in use, artifacts not found, etc.)

---

### plasmd provider status

Query provider server status.

**Synopsis**:
```bash
plasmd provider status [OPTIONS]
```

**Options**:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--json` | | Flag | Disabled | Output as JSON |
| `--addr` | `-a` | URL | `http://localhost:8080` | Provider HTTP address |

**Examples**:

```bash
# Query local provider
plasmd provider status

# Query remote provider
plasmd provider status --addr http://192.168.1.100:8080

# JSON output
plasmd provider status --json
```

**Output (Human-Friendly)**:
```
Provider Status:
  Name:     plasmd-provider
  Version:  0.1.0
  Channel:  stable
  Arch:     x86_64
  Uptime:   3600s

Health:
  Status:             healthy
  Artifacts readable: true
  Disk space ok:      true

Metrics:
  Requests total:     142
  Bytes served total: 1073741824
```

**Output (JSON)**:
```json
{
  "name": "plasmd-provider",
  "version": "0.1.0",
  "channel": "stable",
  "arch": "x86_64",
  "uptime_seconds": 3600,
  "health": {
    "status": "healthy",
    "artifacts_readable": true,
    "disk_space_ok": true
  },
  "metrics": {
    "requests_total": 142,
    "bytes_served_total": 1073741824
  }
}
```

**Exit Codes**:
- `0`: Provider reachable and healthy
- `1`: Provider unreachable or error

---

### plasmd provider list

List available artifacts from provider.

**Synopsis**:
```bash
plasmd provider list [OPTIONS]
```

**Options**:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--addr` | `-a` | URL | `http://localhost:8080` | Provider HTTP address |

**Examples**:

```bash
# List artifacts from local provider
plasmd provider list

# List from remote provider
plasmd provider list --addr http://192.168.1.100:8080
```

**Output**:
```
Available Artifacts:
  Channel: stable
  Arch:    x86_64

  vmlinuz (8388608 bytes)
    Hash: sha256:abc123...
    URL:  stable/x86_64/vmlinuz

  initramfs.img (67108864 bytes)
    Hash: sha256:def456...
    URL:  stable/x86_64/initramfs.img

  rootfs.squashfs (1073741824 bytes)
    Hash: sha256:789abc...
    URL:  stable/x86_64/rootfs.squashfs

Total: 3 artifacts
```

**Exit Codes**:
- `0`: Artifacts found and listed
- `1`: Provider unreachable or error

---

## Discovery Utilities

### phase-discover

Discover boot manifests via libp2p DHT.

**Synopsis**:
```bash
phase-discover [OPTIONS]
```

**Options**:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--arch` | `-a` | String | `x86_64` | Target architecture |
| `--channel` | `-c` | String | `stable` | Update channel |
| `--ephemeral` | | Flag | Disabled | Use ephemeral identity (Private Mode) |
| `--bootstrap` | `-b` | Multiaddr[] | Default nodes | Bootstrap nodes |
| `--timeout` | `-t` | u64 | `30` | Discovery timeout (seconds) |
| `--format` | `-f` | String | `text` | Output format (text, json) |
| `--quiet` | `-q` | Flag | Disabled | Only output manifest URL |

**Examples**:

```bash
# Discover stable x86_64 manifest
phase-discover --arch x86_64 --channel stable

# Private mode discovery
phase-discover --ephemeral --channel testing --arch arm64

# Custom bootstrap node
phase-discover \
  --bootstrap /ip4/192.168.1.100/tcp/4001/p2p/12D3Koo... \
  --channel stable

# JSON output
phase-discover --format json --channel stable

# Quiet mode (for scripts)
MANIFEST_URL=$(phase-discover --quiet --channel stable)
```

**Output (Text)**:
```
INFO Local peer ID: 12D3KooWABC...
INFO Looking up: /phase/stable/x86_64/manifest
INFO Added bootstrap node: 12D3KooWXYZ...
INFO Started DHT query: QueryId(1)
INFO Bootstrap successful
INFO Discovered peer: 12D3KooW123...
INFO Connected to: 12D3KooW456...
INFO Found manifest!
MANIFEST_URL=http://192.168.1.100:8080/stable/x86_64/manifest.json
```

**Output (JSON)**:
```json
{
  "key": "/phase/stable/x86_64/manifest",
  "manifest_url": "http://192.168.1.100:8080/stable/x86_64/manifest.json",
  "peer_id": "12D3KooWABC...",
  "provider_count": 1
}
```

**Output (Quiet)**:
```
http://192.168.1.100:8080/stable/x86_64/manifest.json
```

**Exit Codes**:
- `0`: Manifest found
- `1`: Discovery timeout or error

**Use Cases**:
- Boot-time manifest discovery
- Network diagnostics
- Testing DHT connectivity
- Scripted downloads

---

### phase-verify

Verify artifact integrity against manifest.

**Synopsis**:
```bash
phase-verify <MANIFEST> <ARTIFACT>
```

**Arguments**:
- `MANIFEST`: Path or URL to manifest JSON
- `ARTIFACT`: Path to artifact file

**Examples**:

```bash
# Verify local artifact
phase-verify manifest.json vmlinuz

# Verify against remote manifest
phase-verify http://provider:8080/manifest.json /boot/vmlinuz

# Verify multiple artifacts
for artifact in vmlinuz initramfs.img; do
  phase-verify manifest.json $artifact
done
```

**Output (Success)**:
```
INFO Verifying vmlinuz
INFO Expected: sha256:abc123...
INFO Computed: sha256:abc123...
✓ Artifact verified successfully
```

**Output (Failure)**:
```
ERROR Hash mismatch for vmlinuz
  Expected: sha256:abc123...
  Computed: sha256:def456...
✗ Verification failed
```

**Exit Codes**:
- `0`: Verification successful
- `1`: Hash mismatch or error

---

### phase-fetch

Download and verify artifacts from manifest.

**Synopsis**:
```bash
phase-fetch [OPTIONS] <MANIFEST_URL>
```

**Options**:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--output` | `-o` | Path | Current dir | Output directory |
| `--artifact` | `-a` | String[] | All | Specific artifacts to fetch |
| `--verify` | `-v` | Flag | Enabled | Verify hashes after download |
| `--resume` | `-r` | Flag | Enabled | Resume partial downloads |

**Examples**:

```bash
# Download all artifacts
phase-fetch http://provider:8080/stable/x86_64/manifest.json

# Download to specific directory
phase-fetch -o /boot http://provider:8080/manifest.json

# Download specific artifacts only
phase-fetch \
  -a kernel \
  -a initramfs \
  http://provider:8080/manifest.json

# Download without verification (dangerous!)
phase-fetch --no-verify http://provider:8080/manifest.json

# Resume interrupted download
phase-fetch --resume http://provider:8080/manifest.json
```

**Output**:
```
INFO Fetching manifest from http://provider:8080/stable/x86_64/manifest.json
INFO Found 3 artifacts
INFO Downloading kernel (vmlinuz, 8.0 MB)
  [============================] 100% 8.0 MB/8.0 MB
INFO Verifying vmlinuz... ✓
INFO Downloading initramfs (initramfs.img, 64.0 MB)
  [============================] 100% 64.0 MB/64.0 MB
INFO Verifying initramfs.img... ✓
INFO Downloading rootfs (rootfs.squashfs, 1.0 GB)
  [============================] 100% 1.0 GB/1.0 GB
INFO Verifying rootfs.squashfs... ✓
✓ All artifacts downloaded and verified
```

**Exit Codes**:
- `0`: All artifacts downloaded and verified
- `1`: Download or verification failure

---

## Error Handling

### HTTP Status Codes

| Code | Meaning | Common Causes |
|------|---------|---------------|
| `200` | OK | Successful request |
| `206` | Partial Content | Range request successful |
| `400` | Bad Request | Invalid Range header |
| `404` | Not Found | Artifact or manifest not found |
| `416` | Range Not Satisfiable | Invalid range for file size |
| `500` | Internal Server Error | File I/O error, manifest generation error |
| `503` | Service Unavailable | Health check failed |

### CLI Exit Codes

| Code | Meaning | Resolution |
|------|---------|------------|
| `0` | Success | No action needed |
| `1` | Error | Check error message, see [troubleshooting](troubleshooting.md) |

---

## Related Documentation

- [Architecture](architecture.md) - System design and components
- [Troubleshooting](troubleshooting.md) - Common issues and solutions
- [Security](security.md) - Security best practices
