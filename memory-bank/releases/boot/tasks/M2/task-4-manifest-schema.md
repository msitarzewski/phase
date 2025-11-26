# Task 4 — Manifest Schema & Retrieval


**Agent**: Networking Agent, Security Agent (preparation for M3)
**Estimated**: 4 days

#### 4.1 Define manifest JSON schema
- [ ] File: `boot/schemas/manifest.schema.json`
- [ ] Schema:
  ```json
  {
    "version": "0.1",
    "channel": "stable",
    "arch": "x86_64",
    "artifacts": {
      "kernel": {
        "hash": "sha256:abc123...",
        "size": 12345678,
        "url": "https://mirror.phase.io/stable/kernel-x86_64-0.1.0"
      },
      "initramfs": {
        "hash": "sha256:def456...",
        "size": 23456789,
        "url": "https://mirror.phase.io/stable/initramfs-x86_64-0.1.0"
      },
      "rootfs": {
        "hash": "sha256:ghi789...",
        "size": 345678901,
        "url": "https://mirror.phase.io/stable/rootfs-x86_64-0.1.0.sqfs"
      }
    },
    "signatures": [
      {
        "keyid": "ed25519:key1",
        "sig": "base64-signature-here"
      }
    ],
    "metadata": {
      "build_date": "2025-11-12T00:00:00Z",
      "version": "0.1.0"
    }
  }
  ```
- [ ] JSON Schema validation rules (preparation for M3 verification)

**Dependencies**: None
**Output**: Manifest schema file

#### 4.2 Manifest fetcher
- [ ] Script: `boot/initramfs/scripts/fetch-manifest.sh`
- [ ] Inputs:
  - `--url <URL>` OR `--cid <CID>` (IPFS)
  - `--arch <x86_64|arm64>`
  - `--channel <stable|testing>`
- [ ] Steps:
  - Fetch manifest JSON via curl (HTTPS) or IPFS gateway
  - Validate JSON structure (basic parsing)
  - Extract artifact URLs and hashes
  - Save to `/tmp/manifest.json`
- [ ] Retry logic: 3 attempts with exponential backoff

**Dependencies**: Task 4.1
**Output**: Manifest fetcher script

#### 4.3 Integration: Discovery → Fetch
- [ ] Orchestrator logic in `boot/initramfs/init`:
  - Network up (Task 1.3)
  - Discover manifest:
    - Local Mode: `phase-mdns-client` → manifest URL
    - Internet/Private: `phase-libp2p-client` → manifest CID/URL
  - Fetch manifest: `fetch-manifest.sh --url <discovered_url>`
  - Validate manifest (basic, M3 adds signature verification)
  - Display manifest summary to console

**Dependencies**: Tasks 1.3, 2.3, 3.4, 4.2
**Output**: Integrated discovery-to-fetch flow in init script

---
