# Task 5 — Testing & Validation

**Agent**: QA Agent
**Estimated**: 2 days

## 5.1 DHT discovery test

- [ ] End-to-end DHT test:
  ```bash
  # Terminal 1: Start provider
  plasmd serve --artifacts /tmp/artifacts --port 8080

  # Terminal 2: Run phase-discover (simulating boot client)
  phase-discover --channel stable --arch arm64 --mode wan --timeout 60

  # Expected output:
  # Found manifest URL: http://192.168.1.100:8080/stable/arm64/manifest.json
  # Provider: 12D3KooW...
  ```

**Dependencies**: M3/Tasks 1-4
**Output**: DHT discovery verified

---

## 5.2 mDNS discovery test

- [ ] LAN discovery test:
  ```bash
  # Start provider
  plasmd serve --artifacts /tmp/artifacts

  # Discover via mDNS
  phase-discover --channel stable --arch arm64 --mode lan --timeout 10

  # Should find provider faster than DHT (no bootstrap needed)
  ```

**Dependencies**: Task 5.1
**Output**: mDNS discovery verified

---

## 5.3 Multi-provider test

- [ ] Two providers advertising same channel:
  ```bash
  # Provider 1 (Mac)
  plasmd serve --artifacts /tmp/artifacts --port 8080 &

  # Provider 2 (Linux VM or another machine)
  plasmd serve --artifacts /tmp/artifacts --port 8081 &

  # Discovery should find both
  phase-discover --channel stable --arch arm64 --mode all

  # Expected: Multiple URLs returned
  ```
- [ ] Verify phase-fetch handles provider selection

**Dependencies**: Task 5.2
**Output**: Multi-provider verified

---

## 5.4 Full boot flow test

- [ ] Test with Phase Boot VM:
  ```bash
  # 1. Start provider on Mac
  plasmd serve --artifacts /path/to/real/boot-artifacts

  # 2. Boot Phase Boot VM with vmnet-shared
  sudo qemu-system-aarch64 \
    -M virt -cpu host -accel hvf -m 512 \
    -kernel boot/build/kernel/vmlinuz-arm64 \
    -initrd boot/build/initramfs/initramfs-arm64.img \
    -append "console=ttyAMA0 phase.mode=internet" \
    -netdev vmnet-shared,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -nographic

  # 3. In VM, run discovery and fetch
  phase-discover --channel stable --arch arm64
  phase-fetch --manifest <discovered-url> --output /tmp

  # 4. Verify artifacts fetched successfully
  ls -la /tmp/kernel /tmp/initramfs
  ```

**Dependencies**: Task 5.3
**Output**: Full boot flow verified

---

## 5.5 Fallback test

- [ ] mDNS → DHT fallback:
  ```bash
  # Provider on different subnet (mDNS won't work)
  # phase-discover should fall back to DHT

  phase-discover --channel stable --arch arm64 --mode auto
  # Should try mDNS first, then DHT
  ```
- [ ] DHT bootstrap failure:
  ```bash
  # No bootstrap nodes reachable
  # phase-discover should fail gracefully with clear error
  ```

**Dependencies**: Task 5.4
**Output**: Fallback behavior verified

---

## 5.6 Performance test

- [ ] Measure discovery latency:
  ```bash
  # mDNS (should be <1 second on LAN)
  time phase-discover --mode lan --timeout 10

  # DHT (depends on network, typically 2-10 seconds)
  time phase-discover --mode wan --timeout 60
  ```
- [ ] Verify acceptable for boot use case

**Dependencies**: Task 5.5
**Output**: Performance benchmarks

---

## Validation Checklist

- [ ] DHT discovery works with phase-discover
- [ ] mDNS discovery works on LAN
- [ ] Multiple providers discoverable
- [ ] Full boot flow: discover → fetch → verify
- [ ] Fallback from mDNS to DHT works
- [ ] Discovery latency acceptable (<10s DHT, <1s mDNS)
- [ ] Clear error messages on failure
