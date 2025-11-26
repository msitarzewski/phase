# Task 6 — Testing & Validation


**Agent**: Runtime Agent, Networking Agent
**Estimated**: 4 days

#### 6.1 End-to-end test: Internet Mode
- [ ] Setup:
  - Boot Phase USB (Internet Mode)
  - Full M1-M5 flow: boot → discover → verify → kexec
  - Post-boot: Plasm service starts
- [ ] Validation:
  - Plasm daemon running: `systemctl status plasm`
  - Job discovered from DHT
  - WASM module fetched and verified
  - Job executed: stdout = "dlroW ,olleH"
  - Receipt signed and verified
  - Receipt logged to `/var/log/plasm/receipts/`
  - Console displays formatted receipt

**Dependencies**: All M6 tasks, M1-M5 complete
**Output**: End-to-end Internet Mode test results

#### 6.2 Test: Local Mode
- [ ] Setup: Boot Phase USB (Local Mode)
- [ ] Validation:
  - Plasm starts with Local Mode config
  - Job discovered via mDNS (local provider)
  - No WAN connections (verify routes, firewall logs)
  - Receipt logged (persistent in Local Mode)

**Dependencies**: All M6 tasks
**Output**: Local Mode test results

#### 6.3 Test: Private Mode
- [ ] Setup: Boot Phase USB (Private Mode)
- [ ] Validation:
  - Plasm starts (ephemeral identity from M2)
  - Job discovered via DHT (ephemeral peer ID)
  - Job executed successfully
  - Receipt printed to console
  - NO receipt file in `/var/log/plasm/` (verify empty directory)
  - Reboot: No persistent Plasm state

**Dependencies**: All M6 tasks
**Output**: Private Mode test results

#### 6.4 Non-blocking boot test
- [ ] Setup: Disable Plasm service (`systemctl disable plasm`)
- [ ] Validation:
  - Boot succeeds (kexec completes)
  - Target OS accessible
  - No errors from missing Plasm
  - Optional warning: "Plasm disabled, hello job skipped"

**Dependencies**: Task 2.3 (service)
**Output**: Non-blocking boot test results

#### 6.5 Receipt verification test
- [ ] Valid receipt test:
  - Execute hello job
  - Verify signature with phase-verify
  - Expected: "✓ Verified"
- [ ] Tampered receipt test:
  - Modify receipt JSON (change output)
  - Verify signature
  - Expected: "✗ Invalid signature"

**Dependencies**: Task 4.4 (receipt verification)
**Output**: Receipt verification test results

---
