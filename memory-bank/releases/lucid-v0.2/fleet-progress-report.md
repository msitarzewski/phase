# LUCID v0.2 — Physical Fleet Qualification & Benchmark Progress Report

**Purpose:** Standardized, append-only progress log tracking LUCID's real-world maturity across physical hardware, NAT traversal, verified receipts, inference performance (`lucidd bench`), and Tier-1 `ShardWorker` milestones over time.
**Privacy invariant:** Contains ONLY sanitized role names, anonymized hardware specs, timings, verification verdicts, and findings — NEVER private hostnames, SSH aliases, usernames, raw prompts/completions, or private IPs.
**Authoritative release tracker:** [README.md](./README.md) (this report feeds evidence into the tracker; milestone gates promote only when acceptance criteria pass).

---

## Fleet Readiness Scorecard (Latest Snapshot: 2026-11-26)

| Capability / Gate | Status | Verified Evidence | Next Maturity Target |
|---|---|---|---|
| **Zero-config DNS Bootstrap** (`bootstrap.phasebased.net`) | 🟢 Passing | `366 ms` Mac → Cloud Relay; Ubuntu Desktop direct connect via Porkbun TXT | Add 2nd geographic relay + 60s failover TTL drill |
| **Cloud Relay Infrastructure** (`--mode infrastructure`) | 🟢 Passing | Unprivileged `User=lucid`, `3.2 MiB` RAM, TCP `4001`, circuit-relay reservation accepted (`58 ms`), Caddy `80`/`443` untouched | 100+ concurrent stream saturation test |
| **Self-Configuring Probe & Runtime Sandbox** | 🟢 Passing | Ubuntu Desktop RTX 5060 Ti auto-detected CUDA + 5 Ollama models; downloaded & SHA-256-verified `llama.cpp b12651` CUDA (`zero sudo`) | Verify Mac M1 Max (Metal + MLX) & Ubuntu Laptop CPU |
| **Permission-First Enrollment (`lucid://join/...`)** | 🟢 Passing | One-time invite issued on Ubuntu Desktop, approved & authorized on Mac M5 Max (`--outbound-only`) across Cloud Relay in `<10s` | Test revoked/expired invite rejection across WAN |
| **Live Streamed Remote Chat + Signed Receipts** | 🟢 Passing | HTTP 200 streamed NDJSON across Cloud Relay (`x-lucid-routed-via: peer`, `x_lucid_receipt_verified: "true"`, verified SHA-256 commitment) | Re-verify after `:latest` alias fix on human model name |
| **Thermal / Policy Protection** | 🟢 Passing | Caught real Ryzen `k10temp:Tctl` 90 °C spike at default 75 °C cap and refused cleanly; resumed immediately at 95 °C desktop cap | Separate GPU vs CPU thermal policy guidance for desktops |
| **Whole-Model Baseline (`lucidd bench`)** | 🟡 1-Peer Recorded | 1-peer baseline recorded (Ubuntu Desktop RTX 5060 Ti: `6/6` ok, TTFT p50 `4.47s`, `3.7 chunks/s` @ `c=2`) | Add 2-peer baseline (`Ubuntu Desktop CUDA` + `Mac M1 Max Metal`) |
| **P2P Multi-GB Model Pull & Resume** | ⬜ Not Yet Run | Loopback tests green; physical multi-GB pull/interrupt/resume across WAN pending | Pull 6–10 GB GGUF across Cloud Relay + kill/resume check |
| **Apple Silicon MLX Backend (`--worker mlx`)** | ⬜ Not Yet Run | Mac M1 Max currently offline; pinned adapter unit-tested locally | Run live MLX inference locally & via Cloud Relay on Mac M1 Max |
| **Tier-1 Trusted-Circle `ShardWorker`** | 🟡 Lab Verified | 3-node libp2p integration test + signed layer planner + GGUF reader + Cluster UI green on CI | Complete 2-peer `lucidd bench` baseline + ADR sign-off + live multi-machine RPC run |

---

## Historical Benchmark Trend (`lucidd bench`)

| Run # | Date (UTC) | Release / Commit | Topology & Serving Peers | GPU Offload (VRAM / Temp) | Model (Quant / Size) | Req (`ok/tot`, `c`) | Verified Receipts | TTFT p50 / p95 | Duration p50 / p95 | Throughput (`tok/s` · `chunks/s` · `jobs/s`) | Notes |
|---:|---|---|---|---|---|---|---|---|---|---|---|
| **1** | 2026-11-26 | `v0.2.0-rc4` (`3194dd1`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Ubuntu Desktop (RTX 5060 Ti 16GB) | **CPU fallback** (`14 MiB / 16,311 MiB` VRAM, `35 °C` GPU; `13.8 GB` RAM) | `qwen3.8-iq3xxs:latest` (`10.18 GB`, CID `c0b7c303…`) | `6/6` (`c=2`, `48 max`) | `6/6` *(live chat `"true"`; `bench.rs` string-parse fix landed post-run)* | `4,468 ms` / `4,580 ms` | `25,380 ms` / `27,528 ms` | `n/a` · `3.7 chunks/s` · `0.08 jobs/s` | Missing `libcudart.so.12` caused silent CPU fallback; Ryzen `k10temp:Tctl` hit `90 °C` at default `75 °C` cap |

---

## Detailed Run Log

### Run #1 — 2026-11-26 (`v0.2.0-rc4`, First Physical Cloud Relay + RTX 5060 Ti End-to-End Run)

**Topology:**
- **Cloud Relay** (DigitalOcean Ubuntu x86_64, static IPv4): `lucidd v0.2.0-rc4 --mode infrastructure`, `User=lucid`, `3.2 MiB` RAM, TCP `4001`.
- **Ubuntu Desktop (5060ti 16GB, 32GB RAM)** (behind home NAT): `lucidd v0.2.0-rc4` zero-flag `NodeMode::Auto` on loopback `:11435`, pinned `llama.cpp b12651` CUDA (`SHA-256 a12dacbe…`), serving `qwen3.8-iq3xxs:latest` (`10.18 GB`, Ollama blob hot-imported to CID `c0b7c3038681ed2e…`).
- **Mac M5 Max (128GB RAM)** (consumer): `lucidd v0.2.0-rc4 --mode consume-only --outbound-only` (mDNS disabled, zero manual bootstrap flags).

**What Passed:**
1. Zero-config DNS TXT discovery (`bootstrap.phasebased.net`) resolved on both Mac M5 Max (`366 ms`) and Ubuntu Desktop (`58 ms` circuit-relay reservation accepted).
2. One-time `lucid://join/...` invitation authorized Mac M5 Max over Cloud Relay in `<10s`; `/api/lucid/models` discovered `qwen3.8-iq3xxs:latest` (`peer_count: 1`, exact CID).
3. First live streamed `/api/chat` completed HTTP 200 across Cloud Relay with `x-lucid-routed-via: peer`, `x_lucid_receipt_verified: "true"`, `x_phase_commitment: 7d66a79a…`, cold load + first token `8.08s`, total `11.1s`.
4. `lucidd bench --requests 6 --concurrency 2 --max-tokens 48`: **6/6 HTTP 200 OK**, `48 chunks/req`, TTFT p50 `4,468 ms` / p95 `4,580 ms`, duration p50 `25,380 ms` / p95 `27,528 ms`, `3.7 chunks/s`, route distribution `{"peer": 6}`.
5. Standing Caddy (`80`/`443`), Ollama (`:11434`), and Tailscale remained 100% untouched on all hosts.

**Real-World Findings Discovered & Actioned Immediately:**
1. **`:latest` alias normalization mismatch (`ollama/mod.rs`)**: `ollama_routing_model_name` stripped `:latest` while Ollama-imported manifests stored `name:latest`, causing human-name `/api/chat` to miss when CID dispatch succeeded. **Fixed** in `crates/lucidd/src/ollama/mod.rs:resolve_job_model_cid` (now checks `<model>`, `<model>:latest`, and verified network cache).
2. **`lucidd bench` receipt verdict type (`bench.rs`)**: Stream terminal emits `"x_lucid_receipt_verified": "true"` (string), while `bench.rs` only checked `.as_bool()`. **Fixed** in `crates/lucidd/src/bench.rs` to accept both string `"true"` and boolean `true`.
3. **Desktop Ryzen thermal calibration + CUDA toolkit discovery (`ldd` finding)**:
   - Default laptop-friendly `75 °C` cap tripped on normal AMD Ryzen `k10temp:Tctl` load spikes (`90 °C`) while the RTX 5060 Ti sat at `35 °C` (`14 MiB / 16,311 MiB` VRAM used; `llama-server` held `13.8 GB` system RAM). Raising `thermal_limit_c` to `95 °C` unblocked the run.
   - **Root cause of CPU fallback (`3.7 chunks/s`)**: `ldd libggml-cuda.so` showed `libcudart.so.12 / libcublas.so.12 / libcublasLt.so.12 => not found` on Ubuntu Desktop (NVIDIA driver `libcuda.so.1` is present, but system-wide CUDA 12 runtime libraries are not installed). **Fix implemented:** `runtime_manager.rs` now checks for CUDA 12 runtime libraries before choosing `cuda` vs `vulkan` (so NVIDIA hosts without CUDA 12 automatically select our pinned `b12651` Vulkan build), and `bench.rs` now tracks real `eval_count` `tokens/sec` alongside `chunks/sec`.
   - **Bonus discovery:** Official `llama-b12651` bundles `ggml-rpc-server` and `libggml-rpc.so` out of the box, ready for live Tier-1 `ShardWorker` testing.

---

## Standard Run Entry Template (Copy for Future Runs)

```markdown
### Run #N — YYYY-MM-DD (`<tag/commit>`, <Short Title>)

**Topology:**
- **Cloud Relay**: <version, RAM, path>
- **Serving Peers**: <roles, GPUs/backends, models>
- **Consumer**: <role, network mode (`--outbound-only` / cellular / LAN)>

**What Passed:**
1. ...

**Measured Metrics (`lucidd bench`):**
- Requests: `X/Y ok` (`c=Z`, `N max-tokens`) | Verified receipts: `X/Y`
- TTFT p50 / p95: `... ms / ... ms` | Duration p50 / p95: `... ms / ... ms`
- Throughput: `... jobs/s` · `... chunks/s` | Routes: `{...}`

**Findings & Follow-ups:**
- ...
```
