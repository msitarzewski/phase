# Task 5 — Mode Policy Enforcement


**Agent**: Tooling Agent
**Estimated**: 3 days

#### 5.1 Mode parsing and configuration
- [ ] Script: `boot/initramfs/scripts/parse-mode.sh`
- [ ] Parse kernel cmdline:
  - `phase.mode=<internet|local|private>`
  - `phase.channel=<stable|testing>`
  - `phase.cache=<enabled|disabled>`
  - `phase.nowrite=<true|false>` (Private Mode)
- [ ] Set environment variables:
  - `PHASE_MODE`, `PHASE_CHANNEL`, `PHASE_CACHE`, `PHASE_NOWRITE`
- [ ] Export for use by discovery scripts

**Dependencies**: None
**Output**: Mode parsing script

#### 5.2 Mode-specific discovery selection
- [ ] Logic in `boot/initramfs/init`:
  - **Internet Mode**:
    - Discovery: DHT (libp2p-client)
    - Fetch: HTTPS mirrors, IPFS gateways
    - Cache: Enabled (if cache partition exists)
  - **Local Mode**:
    - Discovery: mDNS only (phase-mdns-client)
    - Fetch: LAN HTTP servers only
    - Cache: Enabled
  - **Private Mode**:
    - Discovery: DHT with ephemeral identity
    - Fetch: HTTPS, Tor (optional, M7), IPFS
    - Cache: Disabled (no persistent writes)

**Dependencies**: Task 5.1
**Output**: Mode selection logic in init script

#### 5.3 Cache partition handling
- [ ] Script: `boot/initramfs/scripts/cache-init.sh`
- [ ] Responsibilities:
  - Detect cache partition: `/dev/disk/by-label/PHASE-CACHE`
  - Mount if PHASE_CACHE=enabled: `mount -t ext4 /dev/... /cache`
  - Skip mount if PHASE_CACHE=disabled (Private Mode)
  - Create cache directory structure: `/cache/manifests/`, `/cache/artifacts/`
- [ ] Verify no writes in Private Mode: Fail-safe mount as read-only if nowrite=true

**Dependencies**: M1 Task 1.3 (cache partition created)
**Output**: Cache initialization script

---
