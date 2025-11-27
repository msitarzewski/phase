# Task 3 — Phase Boot Integration

**Agent**: QA Agent
**Estimated**: 3 days

## 3.1 Prepare real boot artifacts

- [ ] Build Phase Boot kernel and initramfs:
  ```bash
  cd boot
  make download-kernel ARCH=arm64
  make initramfs ARCH=arm64
  ```
- [ ] Copy to provider artifacts:
  ```bash
  mkdir -p ~/boot-artifacts/stable/arm64
  cp build/kernel/vmlinuz-arm64 ~/boot-artifacts/stable/arm64/vmlinuz
  cp build/initramfs/initramfs-arm64.img ~/boot-artifacts/stable/arm64/initramfs.img
  ```

**Dependencies**: M5/Tasks 1-2
**Output**: Real boot artifacts ready

---

## 3.2 Start Mac provider

- [ ] Start plasmd on Mac:
  ```bash
  plasmd serve --artifacts ~/boot-artifacts --channel stable --arch arm64
  ```
- [ ] Verify manifest:
  ```bash
  curl http://localhost:8080/manifest.json | jq
  # Should show real kernel/initramfs hashes
  ```

**Dependencies**: Task 3.1
**Output**: Provider running with real artifacts

---

## 3.3 Boot Phase Boot VM

- [ ] Start QEMU with vmnet-shared:
  ```bash
  sudo qemu-system-aarch64 \
    -M virt -cpu host -accel hvf -m 512 \
    -kernel boot/build/kernel/vmlinuz-arm64 \
    -initrd boot/build/initramfs/initramfs-arm64.img \
    -append "console=ttyAMA0 phase.mode=internet" \
    -netdev vmnet-shared,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -nographic
  ```

**Dependencies**: Task 3.2
**Output**: VM booted on same network as Mac

---

## 3.4 Test discovery from VM

- [ ] In VM, run discovery:
  ```bash
  # Try mDNS first (should find Mac instantly)
  phase-discover --channel stable --arch arm64 --mode lan

  # Or DHT
  phase-discover --channel stable --arch arm64 --mode wan
  ```
- [ ] Expected: Manifest URL pointing to Mac provider

**Dependencies**: Task 3.3
**Output**: Discovery works from VM

---

## 3.5 Test fetch from VM

- [ ] Fetch artifacts:
  ```bash
  # In VM
  phase-fetch --manifest <discovered-url> --output /tmp/boot --artifact all

  ls -la /tmp/boot/
  # Should have kernel, initramfs
  ```
- [ ] Verify hashes match manifest

**Dependencies**: Task 3.4
**Output**: Fetch works from VM

---

## 3.6 Test full boot chain (future)

- [ ] When kexec is enabled in initramfs:
  ```bash
  # Discovery → Fetch → Verify → kexec
  # This boots into the fetched kernel
  ```
- [ ] For now, verify manual steps work

**Dependencies**: Task 3.5
**Output**: Full chain documented

---

## Validation Checklist

- [ ] Real boot artifacts served by Mac
- [ ] VM boots on same network
- [ ] phase-discover finds Mac provider
- [ ] phase-fetch downloads artifacts
- [ ] Hashes verify correctly
- [ ] Full boot chain documented for kexec
