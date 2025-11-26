# Task 5 — Mode Policy Enforcement


**Agent**: Tooling Agent
**Estimated**: 2 days

#### 5.1 Private Mode: Suppress persistent storage
- [ ] Pre-flight check in orchestrator:
  - Read `/etc/phase-mode` (from M4 Task 5.3)
  - If `PHASE_MODE=private`:
    - Skip receipt logging to `/var/log/plasm/receipts/`
    - Print receipt to console only
    - Warn: "Private Mode: Receipt not persisted"
- [ ] Verify: No files written to `/var/log/plasm/` in Private Mode

**Dependencies**: Task 4.1, M4 Task 5.3 (mode metadata)
**Output**: Private Mode enforcement logic

#### 5.2 Local Mode: Network restrictions
- [ ] Plasm configuration:
  - Restrict libp2p to LAN addresses only (no WAN bootstrap)
  - Use mDNS discovery only (from M2)
- [ ] Orchestrator:
  - Use mDNS to discover local Plasm nodes (if multi-node setup)
  - Skip DHT queries (WAN)
- [ ] Validation: No WAN connections in Local Mode

**Dependencies**: Task 1.3 (Plasm config), M2 Task 2.3 (mDNS)
**Output**: Local Mode network restrictions

---
