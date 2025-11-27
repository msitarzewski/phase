# Task 4 — Self-Hosting Test

**Agent**: QA Agent
**Estimated**: 2 days

## 4.1 Boot from provider, become provider

- [ ] The loop:
  ```
  1. Mac runs plasmd, serves boot artifacts
  2. VM boots via Phase Boot
  3. VM fetches artifacts from Mac
  4. VM kexecs into new kernel
  5. New kernel runs plasmd
  6. VM now also serves boot artifacts
  7. Another VM can boot from first VM
  ```

**Dependencies**: M5/Task 3
**Output**: Self-hosting concept documented

---

## 4.2 Include plasmd in initramfs

- [ ] Build static plasmd for initramfs:
  ```bash
  cargo build --release --target aarch64-unknown-linux-musl
  ```
- [ ] Add to Phase Boot initramfs:
  ```bash
  cp target/aarch64-unknown-linux-musl/release/plasmd \
     boot/build/initramfs-work/usr/bin/
  ```
- [ ] Add to rootfs (for post-kexec):
  ```bash
  # In rootfs build script
  cp plasmd /usr/local/bin/
  ```

**Dependencies**: Task 4.1
**Output**: plasmd in boot images

---

## 4.3 Auto-start provider post-boot

- [ ] Add to post-boot init:
  ```bash
  # In /etc/init.d/plasmd or systemd service
  plasmd serve --artifacts /var/lib/plasm/boot-artifacts &
  ```
- [ ] Cache fetched artifacts for re-serving:
  ```bash
  # After fetching from upstream provider
  mv /tmp/boot/* /var/lib/plasm/boot-artifacts/stable/arm64/
  ```

**Dependencies**: Task 4.2
**Output**: Auto-start configured

---

## 4.4 Test the loop

- [ ] Iteration 1:
  ```
  Mac (provider) → VM1 (client)
  VM1 boots, becomes provider
  ```
- [ ] Iteration 2:
  ```
  VM1 (provider) → VM2 (client)
  VM2 boots from VM1 (not Mac)
  ```
- [ ] Verify VM2 discovered VM1, not Mac

**Dependencies**: Task 4.3
**Output**: Self-hosting loop verified

---

## 4.5 Network topology notes

- [ ] For self-hosting to work:
  - Both VMs need vmnet-shared (same network as Mac)
  - Or real network with DHCP
  - DHT bootstrap nodes accessible
- [ ] Document network requirements

**Dependencies**: Task 4.4
**Output**: Network requirements documented

---

## Validation Checklist

- [ ] plasmd included in boot images
- [ ] Provider auto-starts post-boot
- [ ] Fetched artifacts cached for re-serving
- [ ] VM can become provider
- [ ] Second VM can boot from first VM
- [ ] Self-hosting loop works
