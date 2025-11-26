# Task 4 — Post-Boot Job Execution


**Agent**: Runtime Agent
**Estimated**: 4 days

#### 4.1 Post-boot orchestrator script
- [ ] Script: `boot/rootfs/usr/local/bin/plasm-hello-job.sh`
- [ ] Responsibilities:
  - Wait for network: `until ping -c 1 -W 2 1.1.1.1; do sleep 1; done`
  - Wait for Plasm daemon: `until systemctl is-active plasm; do sleep 1; done`
  - Discover job manifest:
    - DHT: `phase-libp2p-client --query /phase/wasm/hello-job/manifest`
    - OR: Use local manifest for MVP (skip discovery)
  - Fetch WASM module:
    - `phase-fetch --url <manifest.module.url> --hash <manifest.module.hash> --output /tmp/hello.wasm`
  - Execute job via Plasm:
    - `plasmd execute-job --wasm /tmp/hello.wasm --args "Hello, World!"`
  - Capture output and receipt
  - Display to console
- [ ] Mode behavior:
  - Private Mode: Skip receipt logging to disk

**Dependencies**: Tasks 1.2, 2.4, 3.3
**Output**: Post-boot orchestrator script

#### 4.2 Integrate into systemd
- [ ] Option A: One-shot service
  - File: `boot/rootfs/lib/systemd/system/plasm-hello-job.service`
  - Type: `Type=oneshot`
  - After: `plasm.service`
  - ExecStart: `/usr/local/bin/plasm-hello-job.sh`
- [ ] Option B: Init script hook (simpler for MVP)
  - Call from end of M4 init script after kexec
  - Run in background: `plasm-hello-job.sh &`
- [ ] Recommendation: Option B for MVP (less complexity)

**Dependencies**: Task 4.1
**Output**: Systemd integration or init hook

#### 4.3 Receipt formatting and display
- [ ] Receipt output (from Plasm):
  ```json
  {
    "version": "0.1",
    "job_id": "hello-job-v0.1",
    "module_hash": "sha256:abc123...",
    "execution": {
      "wall_time_ms": 235,
      "cpu_time_ms": 233,
      "memory_peak_mb": 12,
      "exit_code": 0
    },
    "stdout": "dlroW ,olleH",
    "stderr": "",
    "signature": "base64-encoded-ed25519-sig",
    "node_peer_id": "12D3KooWABC..."
  }
  ```
- [ ] Console output formatting:
  ```
  ╔═══════════════════════════════════════════╗
  ║   Phase Boot: Hello Job Execution        ║
  ╠═══════════════════════════════════════════╣
  ║ Job ID:       hello-job-v0.1              ║
  ║ Module Hash:  sha256:abc123...            ║
  ║ Exit Code:    0                           ║
  ║ Wall Time:    235ms                       ║
  ║ Output:       dlroW ,olleH                ║
  ║ Receipt:      ✓ Verified                  ║
  ╚═══════════════════════════════════════════╝
  ```
- [ ] Log receipt (if not Private Mode):
  - File: `/var/log/plasm/receipts/hello-job-<timestamp>.json`

**Dependencies**: Task 4.1
**Output**: Receipt formatting logic

#### 4.4 Receipt verification
- [ ] Verify Plasm signature:
  - Extract `signature` and `node_peer_id` from receipt
  - Canonicalize receipt (exclude signature field)
  - Verify Ed25519 signature with node's public key (from peer_id)
- [ ] Tool: Reuse M3 `phase-verify` or create dedicated receipt verifier
- [ ] Display: "✓ Verified" or "✗ Invalid signature"

**Dependencies**: Task 4.3, M3 Task 2.3 (verification)
**Output**: Receipt verification logic

---
