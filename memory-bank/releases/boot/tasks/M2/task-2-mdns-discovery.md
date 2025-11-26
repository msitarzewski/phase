# Task 2 — mDNS Discovery (Local Mode)


**Agent**: Networking Agent
**Estimated**: 6 days

#### 2.1 Choose mDNS implementation
- [ ] **Decision**: Avahi client library (static build) OR custom mDNS resolver
  - Rationale: Avahi mature, but large; custom resolver <1MB
  - **Recommendation**: Custom resolver using existing Rust crates (mdns, trust-dns)
- [ ] Build as static binary: `phase-mdns-client`

**Dependencies**: None
**Output**: mDNS client binary decision document

#### 2.2 Implement mDNS service discovery
- [ ] Rust binary: `boot/tools/mdns-client/`
- [ ] Service type: `_phase-image._tcp.local`
- [ ] Query TXT records:
  - `manifest=<URL or CID>` — Manifest location
  - `arch=<x86_64|arm64>` — Architecture
  - `channel=<stable|testing>` — Channel
  - `version=<semver>` — Image version
- [ ] Output format (JSON):
  ```json
  {
    "providers": [
      {
        "host": "phase-node-1.local",
        "ip": "192.168.1.100",
        "port": 8080,
        "manifest": "http://192.168.1.100:8080/manifest.json",
        "arch": "x86_64",
        "channel": "stable",
        "version": "0.1.0"
      }
    ]
  }
  ```

**Dependencies**: Task 2.1
**Output**: mDNS client source code

#### 2.3 Build mDNS client (static, dual-arch)
- [ ] Build script: `boot/tools/mdns-client/build.sh`
- [ ] Cross-compile:
  - `cargo build --release --target x86_64-unknown-linux-musl`
  - `cargo build --release --target aarch64-unknown-linux-musl`
- [ ] Strip symbols: `strip phase-mdns-client`
- [ ] Verify size: <2MB per binary
- [ ] Install to initramfs: `boot/initramfs/bin/phase-mdns-client`

**Dependencies**: Task 2.2
**Output**: Static mDNS client binaries (x86_64, arm64)

#### 2.4 Test mDNS discovery
- [ ] Test setup: 2 machines on same LAN
  - Machine 1: Run mock provider advertising `_phase-image._tcp`
  - Machine 2: Boot Phase initramfs, run mDNS client
- [ ] Validation:
  - Client discovers provider within 5 seconds
  - TXT record parsing correct (manifest URL, arch, channel)
  - Multiple providers handled correctly (pick first matching arch)

**Dependencies**: Task 2.3
**Output**: mDNS test results, mock provider script

---
