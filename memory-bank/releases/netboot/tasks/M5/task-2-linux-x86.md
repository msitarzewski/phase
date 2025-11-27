# Task 2 — Linux x86_64 Testing

**Agent**: Platform Agent
**Estimated**: 2 days

## 2.1 Build on Linux

- [ ] Build plasmd:
  ```bash
  cargo build --release --target x86_64-unknown-linux-gnu
  # Or for static binary:
  cargo build --release --target x86_64-unknown-linux-musl
  ```
- [ ] Verify binary:
  ```bash
  file target/release/plasmd
  # Expected: ELF 64-bit LSB executable, x86-64
  ```

**Dependencies**: M4 complete
**Output**: Linux binary

---

## 2.2 Test on Ubuntu/Debian

- [ ] Install and run:
  ```bash
  sudo mkdir -p /var/lib/plasm/boot-artifacts/stable/x86_64
  sudo cp vmlinuz initramfs.img /var/lib/plasm/boot-artifacts/stable/x86_64/

  ./plasmd serve --artifacts /var/lib/plasm/boot-artifacts
  ```
- [ ] Test endpoints:
  ```bash
  curl http://localhost:8080/health
  curl http://localhost:8080/manifest.json | jq
  ```

**Dependencies**: Task 2.1
**Output**: Ubuntu tested

---

## 2.3 Test mDNS on Linux

- [ ] Install avahi:
  ```bash
  sudo apt install avahi-utils
  ```
- [ ] Verify advertisement:
  ```bash
  avahi-browse -r _phase-image._tcp
  ```

**Dependencies**: Task 2.2
**Output**: mDNS on Linux

---

## 2.4 Test systemd integration

- [ ] Create service file:
  ```ini
  # /etc/systemd/system/plasmd-provider.service
  [Unit]
  Description=Phase Boot Provider
  After=network-online.target

  [Service]
  ExecStart=/usr/local/bin/plasmd serve
  Restart=on-failure
  User=plasm

  [Install]
  WantedBy=multi-user.target
  ```
- [ ] Test service:
  ```bash
  sudo systemctl start plasmd-provider
  sudo systemctl status plasmd-provider
  journalctl -u plasmd-provider -f
  ```

**Dependencies**: Task 2.3
**Output**: systemd integration

---

## 2.5 Test on container

- [ ] Docker test:
  ```dockerfile
  FROM debian:bookworm-slim
  COPY target/release/plasmd /usr/local/bin/
  EXPOSE 8080
  CMD ["plasmd", "serve", "--no-mdns"]
  ```
- [ ] Run container:
  ```bash
  docker build -t plasmd-provider .
  docker run -p 8080:8080 -v /path/to/artifacts:/artifacts plasmd-provider
  ```

**Dependencies**: Task 2.4
**Output**: Container tested

---

## Validation Checklist

- [ ] Binary builds on Linux x86_64
- [ ] Provider runs on Ubuntu/Debian
- [ ] mDNS works with avahi
- [ ] systemd service works
- [ ] Docker container works
