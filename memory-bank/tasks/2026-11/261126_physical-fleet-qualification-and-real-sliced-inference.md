# 261126_physical-fleet-qualification-and-real-sliced-inference

## Objective
Execute soup-to-nuts physical 5-node fleet qualification across **Cloud Relay**, **Ubuntu Desktop**, **Ubuntu Laptop**, **Mac M1 Max**, and **Mac M5 Max**, diagnose and fix real-world WAN/hardware/performance issues iteratively, ship installable `v0.2.0-rc6` → `v0.2.0-rc8` GitHub Release bundles, and demonstrate both Apple Silicon MLX over cellular+ProtonVPN and **real sliced multi-Mac LLM inference**.

## Outcome
- **Releases & CI**: `v0.2.0-rc6` (`6101df2`), `v0.2.0-rc8` (`3eec1de`, Release Bundles green in `25m56s`), catalog resilience fix (`a1849f0`, CI green), and loopback RPC slicing (`b97f93e`, `38/38` `worker_llama` unit + `10/10` integration tests, strict Clippy clean).
- **Physical Fleet Benchmarks (Runs #1–#12)**: **`120/120` (100%) verified Ed25519 receipts** across every run ([fleet-progress-report.md](../../releases/lucid-v0.2/fleet-progress-report.md)):
  - Run #1 (CPU baseline): `3.7 ch/s`, `4,468 ms` TTFT (`NVRM Status 62` diagnosed on Ubuntu Desktop RTX 5060 Ti — awaits host reboot).
  - Runs #2–#5B (Mac M1 Max Metal 4 WAN): `12.0 → 24.1 → 24.7 tok/s`, TTFT p50 `-30%` to `1,696 ms` (`--cache-reuse 256` + alias cache).
  - Run #6 (Mac M5 Max local MLX `Qwen3.6-35B-A3B-4bit-DWQ` `20.68 GB`): `66.7 tok/s`, `72 ms` TTFT, booted in `36s` (down from `>180s` via hardware `ring` SHA-256).
  - Runs #7A → #7B (WAN MLX over Cloud Relay): `model_peers_cache` + bounded catalog refresh cut WAN TTFT **`10,458 ms → 85 ms` (`-99.2%`, 123× faster)** and lifted throughput **`4.4 → 67.1 tok/s`**.
  - Run #8 (2-peer reputation routing): `peer_count: 2` discovered; reputation automatically favored Metal 4 over CPU fallback (`20.7 tok/s`, `2.88×` Run #3).
  - Run #9 (Cross-machine Tier-1 `ShardWorker` protocol): `POST /api/lucid/cluster/execute` Mac M5 Max → Cloud Relay → Mac M1 Max in **`51.5 ms`** (`403` Tier-2 fail-closed, local & remote `cluster_enabled=false` default-deny, live zero-restart opt-in, `2/2` chained Ed25519 receipts).
  - Runs #10A–#10C (Ubuntu Desktop Linux consumer → Cellular Hotspot + ProtonVPN Mac M5 Max MLX + Home Mac M1 Max Metal 4): **`71.3 tok/s` @ `124 ms` TTFT** over cellular+VPN, **`28.8 tok/s` @ `606 ms` TTFT** (`1 ms` jitter) on warm M1 Max Metal, **`98.0 tok/s` combined** (`26.5×` Run #1).
  - Runs #11–#12D (**FIRST REAL SLICED 2-MAC LLM INFERENCE**): ONE `6.98 GB` `gemma-4-12B-it-QAT-Q4_0` sliced by layers across **`135.6 GiB` pooled Apple Silicon Metal 4** (`90%` M5 Max `MTL0` + `10%` M1 Max `RPC0` via `ggml-rpc-server -c`) streaming real tokens (`"I am Gemma, running sliced across two Macs."`) to Ubuntu Desktop via Cloud Relay: **`5.7 tok/s`** Cellular+VPN (`2,447 ms` TTFT, `42.1s` cold load) → **`16.4 tok/s`** 10G+ProtonVPN/Tailscale (`619 ms` TTFT, `7.4s` warm SSD cache load) → **`21.3 tok/s` (`25.0 tok/s` decode, `431 ms` TTFT, `5.4s` warm load)** on Direct LAN (`3.4 ms` RTT).
- **Physical P2P Pull & Security Drills**: `10.18 GB` pull Ubuntu Desktop → Mac M1 Max interrupted at `172 MiB`, resumed cleanly at exact offset (`~16.5 MB/s`), SHA-256 verified; uninvited DCUtR-connected Ubuntu Laptop strictly rejected.

## Key Files Modified & Real-World Bugs Fixed
- `.github/workflows/release.yml` — baseline `x86-64` Linux codegen (`-march=x86-64 -mno-bmi -mno-bmi2 -mno-avx2`, fixing 2012 Ivy Bridge `SIGILL`); `timeout-minutes: 60` + Tauri workspace caching for `Swatinem/rust-cache@v2`.
- `crates/lucidd/src/registry/mod.rs` & `tests.rs` — `:latest` alias resolution, inode triple-hash elimination, short-TTL verified `alias_cid_cache` + `model_peers_cache` (`123×` WAN TTFT speedup), WAN `StartProviding` timeout resilience (preventing 5-minute catalog expiration), and concurrent `buffered(8)` publisher catalog fetches.
- `crates/lucidd/src/consumer.rs:38` — bumped `QUERY_DEADLINE` `5s → 15s` above `phase-net`'s `10s` `DISCOVERY_OPERATION_TIMEOUT` so NATted/`--outbound-only` WAN DHT enrollment cleanly reaches `state: "authorized"` (`3–4s`).
- `crates/lucidd/src/ollama/mod.rs` & `crates/lucidd/src/main.rs` — `POST /api/lucid/cluster/execute`, dedicated shard-worker invitation minting, `SIGTERM`/`SIGINT` graceful shutdown reaping child backends cleanly, `node.toml` `serve_model_content` honored on startup, graceful missing `llama_server_binary` degradation.
- `crates/lucidd/src/worker_llama/mod.rs` & `tests.rs` — `--cache-reuse 256`, Ollama CUDA-12 lib discovery (`LD_LIBRARY_PATH`), and strict loopback-only `--llama-rpc-servers` + `--llama-tensor-split` for Tier-1 layer slicing.
- `crates/lucidd/src/worker_mlx/mod.rs` & `crates/phase-artifact-server/src/artifacts.rs` & `crates/lucidd/src/runtime_manager.rs` — hardware-accelerated `ring::digest::SHA256` (`~2.7 GB/s`, `7.8×` faster), `mlx-lm 0.32.x` `delta.reasoning` SSE parsing.

## Next Handoff
See [multi-agent-sprint-plan.md](../../releases/lucid-v0.2/multi-agent-sprint-plan.md) for parallel non-overlapping Tracks A (libp2p RPC tunnel + rendezvous quotas), B (correctness-evidence validator + Desktop Cluster UX + `/api/ps`), and C (RTX 5060 Ti CUDA post-reboot + Linux ARM64 VM qualification).
