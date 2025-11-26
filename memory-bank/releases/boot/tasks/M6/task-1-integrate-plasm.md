# Task 1 — Integrate Plasm Daemon


**Agent**: Runtime Agent
**Estimated**: 4 days

#### 1.1 Build Plasm daemon for boot rootfs
- [ ] Source: `daemon/` from Phase MVP main branch
- [ ] Cross-compile for x86_64 and ARM64:
  - `cargo build --release --target x86_64-unknown-linux-musl`
  - `cargo build --release --target aarch64-unknown-linux-musl`
- [ ] Static linking: Ensure no dynamic dependencies (musl target handles this)
- [ ] Strip symbols: `strip target/*/release/plasmd`
- [ ] Verify size: <15MB per binary
- [ ] Output: `plasmd` binaries for x86_64, ARM64

**Dependencies**: None (leverages existing daemon/)
**Output**: Plasm daemon binaries

#### 1.2 Install Plasm into target rootfs
- [ ] Update: M1 Task 5.1 (rootfs structure)
- [ ] Install binary:
  - x86_64: Copy `plasmd` to `boot/rootfs/usr/bin/plasmd-x86_64`
  - ARM64: Copy `plasmd` to `boot/rootfs/usr/bin/plasmd-arm64`
  - Symlink: `ln -s plasmd-$(uname -m) /usr/bin/plasmd` in init script
- [ ] Set permissions: `chmod +x /usr/bin/plasmd*`
- [ ] Verify: `plasmd --version` works in chroot

**Dependencies**: Task 1.1, M1 Task 5.1 (rootfs)
**Output**: Plasm daemon installed in rootfs

#### 1.3 Plasm configuration file
- [ ] File: `boot/rootfs/etc/plasm/plasmd.toml`
- [ ] Configuration:
  ```toml
  [network]
  listen_address = "0.0.0.0:8080"
  bootstrap_nodes = [
    "/ip4/1.2.3.4/tcp/4001/p2p/12D3KooWABC123...",
    "/ip4/5.6.7.8/tcp/4001/p2p/12D3KooWXYZ789..."
  ]

  [runtime]
  max_memory_mb = 256
  max_cpu_seconds = 30
  timeout_seconds = 60

  [storage]
  receipts_dir = "/var/log/plasm/receipts"  # Disabled in Private Mode
  cache_dir = "/var/lib/plasm/cache"         # Disabled in Private Mode

  [mode]
  # Populated by init script based on phase.mode kernel param
  mode = "internet"  # or "local" or "private"
  ```
- [ ] Mode-specific config:
  - Private Mode: Disable receipts_dir, cache_dir (set to empty or "/dev/null")

**Dependencies**: Task 1.2
**Output**: Plasm configuration file

---
