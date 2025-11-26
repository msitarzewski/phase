# Task 3 — libp2p DHT Discovery (Internet/Private Modes)


**Agent**: Networking Agent
**Estimated**: 8 days

#### 3.1 Adapt existing libp2p client
- [ ] Source: `daemon/src/network/discovery.rs`
- [ ] Simplify for initramfs:
  - Remove Plasm-specific job protocol
  - Keep Kademlia DHT + bootstrap logic
  - Focus on manifest retrieval only
- [ ] New binary: `boot/tools/libp2p-client/`

**Dependencies**: None (leverages existing codebase)
**Output**: Simplified libp2p client source

#### 3.2 Manifest key scheme
- [ ] DHT key format: `/phase/<channel>/<arch>/manifest`
  - Example: `/phase/stable/x86_64/manifest`
  - Example: `/phase/testing/arm64/manifest`
- [ ] Value: CID (IPFS) or URL (HTTPS) of manifest JSON
- [ ] Provider advertisement (separate tool for node operators):
  - Rust binary: `phase-manifest-advertise`
  - Advertises manifest on DHT
  - Runs on nodes hosting manifest files

**Dependencies**: Task 3.1
**Output**: Key scheme documentation

#### 3.3 Bootstrap nodes configuration
- [ ] File: `boot/configs/bootstrap-nodes.toml`
- [ ] Format:
  ```toml
  [[bootstrap]]
  peer_id = "12D3KooWABC123..."
  address = "/ip4/1.2.3.4/tcp/4001/p2p/12D3KooWABC123..."

  [[bootstrap]]
  peer_id = "12D3KooWXYZ789..."
  address = "/ip4/5.6.7.8/tcp/4001/p2p/12D3KooWXYZ789..."
  ```
- [ ] Initial nodes: 3-5 bootstrap nodes (Phase-operated or community)
- [ ] Fallback: Embed in binary if config missing

**Dependencies**: None
**Output**: Bootstrap nodes configuration file

#### 3.4 Build libp2p client (static, dual-arch)
- [ ] Build script: `boot/tools/libp2p-client/build.sh`
- [ ] Cross-compile:
  - `cargo build --release --target x86_64-unknown-linux-musl --features static`
  - `cargo build --release --target aarch64-unknown-linux-musl --features static`
- [ ] Statically link OpenSSL: `OPENSSL_STATIC=1`
- [ ] Strip symbols: `strip phase-libp2p-client`
- [ ] Verify size: <8MB per binary (libp2p is larger than mDNS)
- [ ] Install to initramfs: `boot/initramfs/bin/phase-libp2p-client`

**Dependencies**: Tasks 3.1, 3.2, 3.3
**Output**: Static libp2p client binaries (x86_64, arm64)

#### 3.5 Ephemeral identity (Private Mode)
- [ ] Private Mode behavior:
  - Generate ephemeral Ed25519 keypair on boot (in-memory)
  - Use ephemeral PeerID for DHT queries
  - No persistent identity storage
- [ ] Implementation: `--ephemeral` flag to `phase-libp2p-client`
- [ ] Verification: No files written to cache partition in Private Mode

**Dependencies**: Task 3.4
**Output**: Ephemeral identity feature

#### 3.6 Test DHT discovery
- [ ] Test setup:
  - Bootstrap node: Run full libp2p node with Kademlia
  - Provider node: Advertise manifest on DHT via `phase-manifest-advertise`
  - Client: Boot Phase initramfs, run libp2p client
- [ ] Validation:
  - Client bootstraps to DHT within 10 seconds
  - Client queries `/phase/stable/x86_64/manifest` successfully
  - Manifest CID/URL retrieved
  - Private Mode uses ephemeral identity (different PeerID each boot)

**Dependencies**: Tasks 3.4, 3.5
**Output**: DHT test results, test node scripts

---
