# LUCID v0.2 — Physical Fleet Qualification & Benchmark Progress Report

**Purpose:** Standardized, append-only progress log tracking LUCID's real-world maturity across physical hardware, NAT traversal, verified receipts, inference performance (`lucidd bench`), and Tier-1 `ShardWorker` milestones over time.
**Privacy invariant:** Contains ONLY sanitized role names, anonymized hardware specs, timings, verification verdicts, and findings — NEVER private hostnames, SSH aliases, usernames, raw prompts/completions, or private IPs.
**Authoritative release tracker:** [README.md](./README.md) (this report feeds evidence into the tracker; milestone gates promote only when acceptance criteria pass).

---

## Fleet Readiness Scorecard (Latest Snapshot: 2026-11-27, `v0.2.0-rc8` Ready — All 5 Physical Nodes Verified)

| Capability / Gate | Status | Verified Evidence | Next Maturity Target |
|---|---|---|---|
| **Zero-config DNS Bootstrap** (`bootstrap.phasebased.net`) | 🟢 Passing | `366 ms` Mac → Cloud Relay; `51 ms` relay reservation on Ubuntu Desktop; DCUtR hole-punching on Ubuntu Laptop | Add 2nd geographic relay + 60s failover TTL drill |
| **Cloud Relay Infrastructure** (`--mode infrastructure`) | 🟢 Passing | Unprivileged `User=lucid`, `18.5 MiB` RSS, TCP `4001`, Caddy `80`/`443` untouched | 100+ concurrent stream saturation test |
| **Self-Configuring Probe & Runtime Sandbox** | 🟢 Passing | Zero-`sudo` pinned `b12651` Metal (Mac M1 Max `MTL0` 25.6 GiB free) + Vulkan/CUDA-12 detection (Ubuntu Desktop) + baseline `x86-64` Ivy Bridge support (Ubuntu Laptop, zero SIGILL) | Verify RTX 5060 Ti CUDA 12.8 after host reboot clears `NVRM Status 62` |
| **Permission-First Enrollment & Default-Deny** | 🟢 Passing | One-time `lucid://join/...` WAN DHT enrollment fixed (`QUERY_DEADLINE` 5s→15s so NATted/`--outbound-only` peers reach `state: "authorized"`); uninvited DCUtR-connected Ubuntu Laptop strictly denied (`Access is not authorized`) | Revoked/expired invite rejection drill |
| **Live Streamed Remote Chat + Signed Receipts** | 🟢 Passing | Routed directly by human model name (`qwen3.8-iq3xxs:latest`, `qwen3.6-35b-mlx`, `gemma-4-12b-sliced`), **`122/122` (`100%`)** remote peer-verified Ed25519 receipts across Runs #1–#12 (plus `6/6` local loopback commitments in Run #6 and `2/2` chained stage receipts in Run #9 protocol test) | Fix `bench.rs:244` so local runs never increment `verified_receipts`; keep `100%` remote verification |
| **Thermal / Policy Protection** | 🟢 Passing | Ryzen `k10temp:Tctl` 90 °C cap, `manual_pause`, and live zero-restart `cluster_enabled` toggle verified | Separate GPU vs CPU thermal policy guidance for desktops |
| **Whole-Model Baseline (`lucidd bench`)** | 🟢 Multi-Peer & WAN MLX Verified | Peak **`67.1 tok/s`** WAN MLX (`85 ms` TTFT p50, **18.1× Run #1**); **`24.7 tok/s`** WAN Metal (`c=4`, `1,696 ms` TTFT p50); 2-peer reputation routing (`Run #8`: `peer_count: 2`, **`20.7 tok/s`**, **2.88× Run #3**) | Re-measure 2-peer GPU+GPU load spread once RTX 5060 Ti reboot clears `NVRM Status 62` |
| **P2P Multi-GB Model Pull & Resume** | 🟢 Passing | `10.18 GB` (`10,934,860,704 B`) `qwen3.8-iq3xxs:latest` pulled Ubuntu Desktop → Mac M1 Max, interrupted at `172 MiB`, resumed at exact offset (`180,289,536 B`, `~16.5 MB/s`), SHA-256 verified (`c0b7c303…`) & hardlink-installed | Add multi-provider striped pull benchmark |
| **Apple Silicon MLX Backend (`--worker mlx`)** | 🟢 Local + WAN Verified | Real `20.68 GB` `Qwen3.6-35B-A3B-4bit-DWQ` immutable bundle (`10 files`, `cid=b48e6beb…`): local loopback **`66.7 tok/s`** (`72 ms` TTFT); **WAN over Cloud Relay (`Run #7B`) `67.1 tok/s` (`85 ms` TTFT p50, `6/6` verified receipts, clean `SIGTERM` child reaping)** | Add concurrent MLX queue/batching telemetry |
| **Tier-1 Trusted-Circle `ShardWorker`** | 🟡 Prototype Verified (Design Consolidation Open) | **Run #9** verified `/phase/shard-stage/1.0.0` transport, default-deny authz, and chained Ed25519 receipts (`51.5 ms` via loopback test stub — not 43 layers of compute). **Runs #11–#12** verified **real sliced 2-Mac LLM compute**: `6.98 GB` `gemma-4-12B-it-QAT-Q4_0` split across `135.6 GiB` pooled Metal 4 (`90%` M5 Max + `10%` M1 Max via `ggml-rpc-server -c`) at **`5.7 tok/s`** Cellular+VPN → **`16.4 tok/s`** 10G+VPN → **`21.3 tok/s` (`25.0` decode)** Direct LAN (`28/28` remote peer-verified receipts) | Wave 1: gate `--llama-rpc-servers` on `cluster_enabled` + refuse on `X-Lucid-Local-Only`; Wave 2: native Noise-encrypted `/phase/shard-rpc/1.0.0` tunnel + ADR consolidation |

---

## Historical Benchmark Trend (`lucidd bench`)

| Run # | Date (UTC) | Release / Commit | Topology & Serving Peers | GPU Offload (VRAM / Temp) | Model (Quant / Size) | Req (`ok/tot`, `c`) | Verified Receipts | TTFT p50 / p95 | Duration p50 / p95 | Throughput (`tok/s` · `chunks/s` · `jobs/s`) | Notes |
|---:|---|---|---|---|---|---|---|---|---|---|---|
| **1** | 2026-11-26 17:19Z | `v0.2.0-rc4` (`3194dd1`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Ubuntu Desktop | **CPU fallback** (`14 MiB / 16,311 MiB` VRAM, `35 °C` GPU) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `6/6` (`c=2`, `48 max`) | `6/6` | `4,468 ms` / `4,580 ms` | `25,380 ms` / `27,528 ms` | `n/a` · `3.7 ch/s` · `0.08 jobs/s` | Baseline CPU fallback; RTX 5060 Ti kernel driver later diagnosed in `NVRM Status 62` (host reboot required) |
| **2** | 2026-11-26 21:42Z | `v0.2.0-rc6` (`6101df2`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (Metal) | **Apple M1 Max Metal 4** (`12.1 GiB` unified mem, `50%` CPU) | `qwen3.8-iq3xxs:latest` (`10.18 GB`, P2P-pulled) | `6/6` (`c=2`, `64 max`) | `6/6` | `2,710 ms` / `4,060 ms` | `10,293 ms` / `11,697 ms` | **`12.0 tok/s`** · `12.0 ch/s` · `0.19 jobs/s` | **3.24× Run #1**; routed natively by human model name (`:latest` fix verified live) |
| **2B** | 2026-11-26 21:43Z | `v0.2.0-rc6` (`6101df2`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (Metal) | **Apple M1 Max Metal 4** (`12.1 GiB` unified mem) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `4/4` (`c=1`, `64 max`) | `4/4` | `1,908 ms` / `2,088 ms` | `6,406 ms` / `6,641 ms` | **`10.0 tok/s`** (`14.2` decode) · `0.16 jobs/s` | Single-stream baseline; exposed `~1.5s` routing/DHT overhead vs `256 ms` raw `llama-server` prompt eval |
| **3** | 2026-11-26 21:50Z | `v0.2.0-rc6` (`6101df2`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → 2 Peers (M1 Max + Ubuntu Desktop) | Mixed: M1 Max Metal (`24.6 tok/s` batch) + Ubuntu Desktop CPU | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `8/8` (`c=4`, `64 max`) | `8/8` | `8,057 ms` / `8,058 ms` | `59,293 ms` / `59,306 ms` | **`7.2 tok/s`** · `7.2 ch/s` · `0.11 jobs/s` | Exposed single-key Kademlia `find_peers_for_model` truncation (hid 2nd peer after 30s) & lack of in-flight load spread |
| **4** | 2026-11-26 21:52Z | `v0.2.0-rc6` (`6101df2`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (Metal, `c=4`) | **Apple M1 Max Metal 4** (`12.1 GiB` unified mem, cont. batching) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `8/8` (`c=4`, `64 max`) | `8/8` | `2,422 ms` / `2,423 ms` | `10,642 ms` / `10,654 ms` | **`24.1 tok/s`** · `24.1 ch/s` · `0.38 jobs/s` | **6.5× Run #1 peak**; **1 ms TTFT p50→p95 jitter** over WAN Cloud Relay! |
| **5A** | 2026-11-26 23:07Z | `v0.2.0-rc7` (`5063220`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (Metal, `c=1`) | **Apple M1 Max Metal 4** (`12.1 GiB`, `--cache-reuse 256`) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `4/4` (`c=1`, `64 max`) | `4/4` | **`1,515 ms`** / `1,589 ms` | `5,935 ms` / `6,008 ms` | **`10.8 tok/s`** · `10.8 ch/s` · `0.17 jobs/s` | Alias cache + `--cache-reuse 256` cut `c=1` TTFT p50 by **`-393 ms` (`-21%`)** and p95 by **`-24%`** |
| **5B** | 2026-11-26 23:08Z | `v0.2.0-rc7` (`5063220`) | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (Metal, `c=4`) | **Apple M1 Max Metal 4** (`12.1 GiB`, `--cache-reuse 256`) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `8/8` (`c=4`, `64 max`) | `8/8` | **`1,696 ms`** / `2,712 ms` | `10,739 ms` / `11,014 ms` | **`24.7 tok/s`** · `24.7 ch/s` · `0.39 jobs/s` | **New WAN Metal peak `24.7 tok/s` (`6.7×` Run #1)**; `c=4` TTFT p50 slashed by **`-726 ms` (`-30%`)** |
| **6** | 2026-11-26 23:23Z | `v0.2.0-rc7` (`5063220`) | Mac M5 Max (`--worker mlx`, local loopback) | **Apple M5 Max MLX Metal** (`20.68 GB` immutable bundle) | `qwen3.6-35b-mlx` (`Qwen3.6-35B-A3B-4bit-DWQ`) | `6/6` (`c=1`, `64 max`) | `6/6` *(local)* | **`72 ms`** / `141 ms` | `907 ms` / `967 ms` | **`66.7 tok/s`** · `66.7 ch/s` · `1.09 jobs/s` | **First physical Apple Silicon MLX run (`18.0×` Run #1)**; booted in `36s` (down from `>180s`) via `ring` hardware SHA-256 + `mlx-lm 0.32.x` `delta.reasoning` fix |
| **7A** | 2026-11-26 23:49Z | `v0.2.0-rc7` (`5063220`) | Mac M1 Max → Cloud Relay → Mac M5 Max (`--worker mlx --outbound-only`) | **Apple M5 Max MLX Metal** (`20.68 GB` over WAN Relay) | `qwen3.6-35b-mlx` (`Qwen3.6-35B-A3B-4bit-DWQ`) | `6/6` (`c=1`, `64 max`) | `6/6` | `10,458 ms` / `20,646 ms` | `11,338 ms` / `21,526 ms` | `4.4 tok/s` *(72.7 decode)* · `0.07 jobs/s` | First remote WAN Apple Silicon MLX + clean `SIGTERM` reaping (`rc=0`, 0 leftover `mlx_lm.server`); exposed 10s Kademlia `GetRecord` wait on every uncached `find_peers_for_model` |
| **7B** | 2026-11-27 00:02Z | `v0.2.0-rc8` | Mac M1 Max → Cloud Relay → Mac M5 Max (`--worker mlx --outbound-only`) | **Apple M5 Max MLX Metal** (`20.68 GB` over WAN Relay) | `qwen3.6-35b-mlx` (`Qwen3.6-35B-A3B-4bit-DWQ`) | `6/6` (`c=1`, `64 max`) | `6/6` | **`85 ms`** / **`206 ms`** | **`907 ms`** / `1,016 ms` | **`67.1 tok/s`** · `67.1 ch/s` · **`1.09 jobs/s`** | **`model_peers_cache` + `500 ms` catalog bound**: slashed WAN TTFT p50 **`10,458 → 85 ms` (`-99.2%`, 123× faster)**, **18.1× Run #1**, within **13 ms** of local loopback! |
| **8** | 2026-11-27 00:08Z | `v0.2.0-rc8` | Mac M5 Max (`--outbound-only`) → Cloud Relay → 2 Peers (M1 Max + Ubuntu Desktop) | **Reputation-selected M1 Max Metal 4** (`peer_count: 2`) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `8/8` (`c=4`, `64 max`) | `8/8` | `5,493 ms` / `5,493 ms` | `13,726 ms` / `13,738 ms` | **`20.7 tok/s`** *(24.9 warm)* · `0.32 jobs/s` | Multi-peer catalog saw both candidates (`local evidence ordered 2 candidates`); reputation evidence avoided slow CPU fallback (`2.88×` Run #3, `0 ms` TTFT jitter) |
| **9** | 2026-11-27 00:30Z | `v0.2.0-rc8` | Mac M5 Max (`--outbound-only`) → Cloud Relay → Mac M1 Max (`/phase/shard-stage/1.0.0`) | **2-Stage Tier-1 Trusted-Circle `ShardWorker`** (`0..36`, `36..43`) | `DeepSeek-V4-Flash` 43L plan (`POST /api/lucid/cluster/execute`) | `1/1` (4 security drills) | `2/2` chained stage receipts | **`51.5 ms`** total | **`51.5 ms`** | `trusted-circle-sharded` | Verified Tier-2 fail-closed (`403`), local/remote `cluster_enabled=false` default-deny, WAN DHT enrollment fix (`QUERY_DEADLINE` 5s→15s), live zero-restart opt-in, & chained SHA-256 activation handoff |
| **10A** | 2026-11-27 01:40Z | `v0.2.0-rc8` (`a1849f0`) | **Ubuntu Desktop** → Cloud Relay → **Cellular Hotspot Mac M5 Max** (`--worker mlx --outbound-only`) | **Apple M5 Max MLX Metal** (`20.68 GB` over Cellular CGNAT) | `qwen3.6-35b-mlx` (`Qwen3.6-35B-A3B-4bit-DWQ`) | `6/6` (`c=1`, `64 max`) | `6/6` | **`124 ms`** / **`142 ms`** | **`860 ms`** / `878 ms` | **`71.3 tok/s`** · `71.3 ch/s` · **`1.16 jobs/s`** | **New single-peer peak `71.3 tok/s` (`19.3×` Run #1)** across real **Cellular CGNAT → Cloud Relay → Linux Desktop**; dual-enrolled in `3s`! |
| **10B** | 2026-11-27 01:41Z | `v0.2.0-rc8` (`a1849f0`) | **Ubuntu Desktop** → **Home Mac M1 Max** (Metal 4, `c=4`) | **Apple M1 Max Metal 4** (`12.1 GiB`, `--cache-reuse 256`) | `qwen3.8-iq3xxs:latest` (`10.18 GB`) | `8/8` (`c=4`, `64 max`) | `8/8` | `2,128 ms` / `2,128 ms` | `10,405 ms` / `10,405 ms` | **`26.2 tok/s`** · `26.2 ch/s` · `0.41 jobs/s` | Enrolled in `4s`; `0 ms` TTFT jitter across Linux → Apple Silicon Metal 4 |
| **10C** | 2026-11-27 01:41Z | `v0.2.0-rc8` (`a1849f0`) | **Ubuntu Desktop** → **Simultaneous Cellular Mac M5 Max MLX + Home Mac M1 Max Metal 4** | **2-Mac Apple Silicon Burst** (`69.2` MLX + `28.8` Metal @ `606 ms` TTFT) | `qwen3.6-35b-mlx` + `qwen3.8-iq3xxs:latest` | `16/16` (`c=1+4`, `64 max`) | `16/16` | **`120 ms`** (M5) · **`606 ms`** (M1) | `865 ms` (M5) · `8,911 ms` (M1) | **`98.0 tok/s` combined** (`1.58 jobs/s`) | **All-time fleet record `98.0 tok/s` (`26.5×` Run #1)** flowing into one Linux Desktop across two Macs (one on cellular hotspot!); M1 Max warm `--cache-reuse 256` hit `606 ms` TTFT (`1 ms` jitter) & `28.8 tok/s`! |
| **11** | 2026-11-27 02:39Z | `v0.2.0-rc8+` (`b97f93e`) | **Ubuntu Desktop** → Cloud Relay → **REAL SLICED 2-Mac Pool (Cellular+ProtonVPN M5 Max 90% + Home M1 Max 10%)** | **135.6 GiB Pooled Metal 4** (`MTL0` M5 Max `110.1 GiB` + `RPC0` M1 Max `25.6 GiB`) | `gemma-4-12b-sliced` (`gemma-4-12B-it-QAT-Q4_0`, `6.98 GB`, **ONE model sliced**) | `4/4` (`c=1`, `32 max`) | `4/4` | `2,447 ms` / `2,856 ms` | `5,615 ms` / `5,882 ms` | **`5.7 tok/s`** *(10.1 decode)* · `0.18 jobs/s` | **FIRST REAL SLICED MULTI-MAC LLM TOKENS** (`"I am Gemma, running sliced across two Macs."`); Cellular Hotspot + ProtonVPN RTT (`~60 ms/tok` inter-Mac hop) |
| **12A/B** | 2026-11-27 02:49Z | `v0.2.0-rc8+` (`b97f93e`) | **Ubuntu Desktop** → Cloud Relay → **SLICED 2-Mac Pool (10G+ProtonVPN/Tailscale `18-57 ms` RTT)** | **135.6 GiB Pooled Metal 4** (`7.4s` warm SSD cache load) | `gemma-4-12b-sliced` (`6.98 GB`, **ONE model sliced**) | `12/12` (`c=1` & `c=4`, `48 max`) | `12/12` | **`619 ms`** / **`683 ms`** (`c=1`) | `2,984 ms` / `3,043 ms` | **`16.4 tok/s`** *(20.3 decode)* · `0.34 jobs/s` | **2.88× Cellular+VPN** (`-75%` TTFT); M1 Max `-c` SSD cache cut sliced load `42.1s → 7.4s` |
| **12C/D** | 2026-11-27 02:54Z | `v0.2.0-rc8+` (`b97f93e`) | **Ubuntu Desktop** → Cloud Relay (ProtonVPN) → **SLICED 2-Mac Pool (Direct LAN `3.4 ms` RTT)** | **135.6 GiB Pooled Metal 4** (`5.4s` warm SSD cache load) | `gemma-4-12b-sliced` (`6.98 GB`, **ONE model sliced**) | `12/12` (`c=1` & `c=4`, `48 max`) | `12/12` | **`431 ms`** / **`458 ms`** (`c=1`) | `2,360 ms` / `2,481 ms` | **`21.3 tok/s`** (`20.4` `c=1`, **`25.0` decode**) · `0.44 jobs/s` | **3.74× Cellular+VPN** (`-82%` TTFT, **`5.4s`** warm load = `7.8×` faster); proves exact 3-point inter-Mac RTT scaling curve while still routing client traffic through ProtonVPN + Cloud Relay! |

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
