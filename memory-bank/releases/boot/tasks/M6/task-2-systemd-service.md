# Task 2 — Systemd Service


**Agent**: Systems Agent
**Estimated**: 3 days

#### 2.1 Create plasm.service unit file
- [ ] File: `boot/rootfs/lib/systemd/system/plasm.service`
- [ ] Unit file:
  ```ini
  [Unit]
  Description=Phase Plasm Daemon
  After=network-online.target
  Wants=network-online.target

  [Service]
  Type=simple
  ExecStart=/usr/bin/plasmd --config /etc/plasm/plasmd.toml
  Restart=on-failure
  RestartSec=5s
  User=plasm
  Group=plasm
  NoNewPrivileges=true
  PrivateTmp=true
  ProtectSystem=strict
  ProtectHome=true
  ReadWritePaths=/var/log/plasm /var/lib/plasm

  [Install]
  WantedBy=multi-user.target
  ```
- [ ] Security hardening:
  - Run as non-root user (`plasm:plasm`)
  - No new privileges
  - Read-only filesystem (except logs, cache)

**Dependencies**: Task 1.3
**Output**: Systemd service unit file

#### 2.2 Create plasm user and directories
- [ ] User/group creation: Add to `boot/rootfs/etc/passwd`, `boot/rootfs/etc/group`
  - User: `plasm:x:1000:1000:Phase Plasm:/var/lib/plasm:/bin/false`
  - Group: `plasm:x:1000:`
- [ ] Directories:
  - `/var/log/plasm/` — Receipt logs (if not Private Mode)
  - `/var/lib/plasm/` — Cache directory (if not Private Mode)
- [ ] Permissions:
  - `chown -R plasm:plasm /var/log/plasm /var/lib/plasm`
  - `chmod 755 /var/log/plasm /var/lib/plasm`

**Dependencies**: Task 2.1
**Output**: Plasm user and directories

#### 2.3 Enable service on boot
- [ ] Symlink: `ln -s /lib/systemd/system/plasm.service /etc/systemd/system/multi-user.target.wants/plasm.service`
- [ ] Alternatively: Use systemd preset file (auto-enable)
- [ ] Verify: `systemctl list-unit-files | grep plasm` shows `enabled`

**Dependencies**: Task 2.1
**Output**: Service enabled on boot

#### 2.4 Test service lifecycle
- [ ] Start: `systemctl start plasm`
- [ ] Status: `systemctl status plasm` → should show "active (running)"
- [ ] Logs: `journalctl -u plasm -f` → should show Plasm startup logs
- [ ] Stop: `systemctl stop plasm`
- [ ] Restart: `systemctl restart plasm`
- [ ] Validation: All lifecycle operations work correctly

**Dependencies**: Task 2.3
**Output**: Service lifecycle test results

---
