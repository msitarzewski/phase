# Task 1 — macOS ARM64 Testing

**Agent**: Platform Agent
**Estimated**: 2 days

## 1.1 Build and run on macOS

- [ ] Build plasmd:
  ```bash
  cd daemon
  cargo build --release
  ```
- [ ] Verify binary:
  ```bash
  file target/release/plasmd
  # Expected: Mach-O 64-bit executable arm64
  ```

**Dependencies**: M4 complete
**Output**: macOS binary

---

## 1.2 Test basic provider functionality

- [ ] Create test artifacts:
  ```bash
  mkdir -p ~/Library/Application\ Support/plasm/boot-artifacts/stable/arm64
  cp /path/to/test/vmlinuz ~/Library/Application\ Support/plasm/boot-artifacts/stable/arm64/
  cp /path/to/test/initramfs.img ~/Library/Application\ Support/plasm/boot-artifacts/stable/arm64/
  ```
- [ ] Start provider:
  ```bash
  ./target/release/plasmd serve
  ```
- [ ] Test HTTP endpoints:
  ```bash
  curl http://localhost:8080/health
  curl http://localhost:8080/status | jq
  curl http://localhost:8080/manifest.json | jq
  curl -I http://localhost:8080/stable/arm64/kernel
  ```

**Dependencies**: Task 1.1
**Output**: Basic functionality verified

---

## 1.3 Test mDNS on macOS

- [ ] Verify mDNS advertisement:
  ```bash
  dns-sd -B _phase-image._tcp local.
  # Should show: phase-stable-arm64._phase-image._tcp.local.

  dns-sd -L "phase-stable-arm64" _phase-image._tcp local.
  # Should show TXT records
  ```

**Dependencies**: Task 1.2
**Output**: mDNS working

---

## 1.4 Test DHT on macOS

- [ ] Start provider with DHT:
  ```bash
  ./target/release/plasmd serve --artifacts ~/Library/Application\ Support/plasm/boot-artifacts
  ```
- [ ] Verify DHT publishing (check logs):
  ```
  INFO Publishing boot manifest to DHT: /phase/stable/arm64/manifest
  INFO DHT record published
  ```
- [ ] Test discovery from another machine:
  ```bash
  phase-discover --channel stable --arch arm64 --timeout 60
  ```

**Dependencies**: Task 1.3
**Output**: DHT working

---

## 1.5 macOS-specific issues

- [ ] Test with macOS firewall enabled
- [ ] Test with Little Snitch/firewall apps
- [ ] Verify port binding on 0.0.0.0
- [ ] Test mDNS through macOS network stack

**Dependencies**: Task 1.4
**Output**: Firewall compatibility verified

---

## Validation Checklist

- [ ] Binary builds on macOS ARM64
- [ ] Provider starts and serves files
- [ ] mDNS works with macOS dns-sd
- [ ] DHT publishing works
- [ ] Firewall doesn't block provider
- [ ] Status commands work
