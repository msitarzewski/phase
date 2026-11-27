# LUCID v0.2 — Multi-Agent Divide & Conquer Sprint Plan

**Created:** 2026-11-27
**Base Branch:** `feat/lucid-consumer-experience` (base commit `135ef26+`, CI green on `v0.2.0-rc8` + `a1849f0` + `b97f93e`)
**Live Fleet Snapshot:** All 5 physical nodes online on `v0.2.0-rc8` (`120/120` verified Ed25519 receipts across Runs #1–#12; see [fleet-progress-report.md](./fleet-progress-report.md)).

This plan partitions the remaining LUCID v0.2 release-acceptance work into **three strictly disjoint tracks** so multiple AI agents (**Claude Opus 5.5**, **GPT 6.2 Sol**, and a **Fleet/Ops agent**) can execute in parallel on separate branches with **zero file collisions**.

---

## Non-Negotiable Ground Rules (Every Agent Must Follow)

1. **Read First (Fast Context Load)**:
   - This file + [fleet-progress-report.md](./fleet-progress-report.md) + [README.md](./README.md) + [shardworker-adr.md](./shardworker-adr.md).
2. **Privacy / Opsec Hard Rule**:
   - **NEVER** commit private hostnames, SSH aliases, usernames, Tailscale/private IPs, tokens, or raw prompts/completions to any tracked file or commit message. Use only hardware role names: **Cloud Relay**, **Ubuntu Desktop**, **Ubuntu Laptop**, **Mac M1 Max**, **Mac M5 Max**.
3. **Substrate Neutrality (`memory-bank/decisions.md:854-875`)**:
   - `crates/phase-*` crates (`phase-net`, `phase-protocol`, `phase-manifest`, `phase-receipt`, `phase-identity`, `phase-artifact-server`) MUST remain 100% workload-neutral (opaque bytes only — NEVER put model names, tokens, GGUF, or Ollama logic into `phase-*`).
4. **Security & Honesty Invariants (Fail-Closed)**:
   - `#![deny(unsafe_code)]` across all crates & Tauri backend.
   - `ShardTrustTier::UntrustedSpotChecked` stays strictly fail-closed until partial-compute verification passes ADR approval.
   - `cluster_enabled` defaults to `false`; `allow_unauthenticated_jobs` defaults to `false`.
   - `llama-server`, `ggml-rpc-server`, and `mlx_lm.server` MUST bind strictly to `127.0.0.1` inside `env_clear()` — never `0.0.0.0`, never unauthenticated WAN sockets.
   - Receipts prove attributable transcript integrity (`trust_label = "trusted-circle-sharded"` for Tier-1 sharding), never third-party cryptographic proof of tensor math.
   - Tauri WebView NEVER receives the local control bearer token; keep strict CSP and minimal `capabilities/default.json`.
5. **Verification Gate Before Merging Each Track**:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   # If touching apps/lucid-desktop:
   (cd apps/lucid-desktop && npm run check && cargo test --manifest-path src-tauri/Cargo.toml --locked)
   ```
6. **Git Workflow**:
   - Create a dedicated branch from `feat/lucid-consumer-experience` (e.g., `feat/track-a-shard-rpc-tunnel`, `feat/track-b-validator-cluster-ui`).
   - Do not modify files owned by another track unless coordinated explicitly via a tiny shared interface PR.

---

## Track A — Protocol, Native Sliced-Model Tunnel, & Rendezvous Quotas
**Recommended Agent:** **Claude Opus 5.5**
**Branch:** `feat/track-a-shard-rpc-and-rendezvous`

### Exclusive File Ownership
- `crates/phase-net/src/discovery/**`
- `crates/phase-net/src/lib.rs`
- `crates/lucidd/src/shard_worker.rs`
- `crates/lucidd/tests/shard_worker.rs`
- `crates/lucidd/src/main.rs` *(CLI flags & rendezvous config wiring only — keep diff minimal & localized)*

### Deliverable A1: Native Tier-1 Noise-Encrypted libp2p Byte-Stream Tunnel (`/phase/shard-rpc/1.0.0`)
- **Problem Solved**: Runs #11–#12 proved real sliced 2-Mac `gemma-4-12B` inference (`135.6 GiB` pooled Metal 4, `21.3 tok/s` LAN / `16.4 tok/s` 10G+VPN / `5.7 tok/s` Cellular+VPN) using `lucidd --llama-rpc-servers 127.0.0.1:50052` + `ggml-rpc-server -H 127.0.0.1 -p 50052 -c`, but still relied on an external SSH `-L` tunnel for the encrypted loopback hop.
- **Concrete Implementation**:
  1. In `crates/phase-net/src/discovery/mod.rs`: register a workload-neutral opaque bidirectional stream protocol `/phase/shard-rpc/1.0.0` (using existing `libp2p_stream::Behaviour`, identical to `/phase/job-relay/2.0.0` and `/phase/shard-stage/1.0.0`), with bounded concurrent tunnel cap (e.g., max 4 active streams per peer, 16 global) and idle timeout.
  2. In `crates/lucidd/src/shard_worker.rs`:
     - **Worker (serving) side**: Add `RpcStageTunnelHandler` gated by BOTH `node_config.snapshot().cluster_enabled` AND `policy.is_authorized_submitter(...)` (default-deny!), plus `policy.should_serve(...)`. On authorized inbound `/phase/shard-rpc/1.0.0` stream, proxy bytes strictly to `127.0.0.1:<local_rpc_port>` (spawned via existing `SandboxedStageProcess::spawn_verified` running pinned `ggml-rpc-server -H 127.0.0.1 -p <ephemeral> -c` inside `env_clear()`!).
     - **Coordinator side**: Bind a local ephemeral `127.0.0.1:<port>` TCP listener that accepts connections from local `llama-server --rpc 127.0.0.1:<port>` and bridges each connection over `/phase/shard-rpc/1.0.0` to the invited target `PeerId` (over Noise+QUIC/TCP direct LAN or Cloud Relay automatically!).
     - **Remember `llama.cpp` ordering quirk discovered in Run #11**: `--tensor-split` places `RPC0..N` BEFORE local `MTL0/CUDA0` (`10,90` = 10% remote RPC0, 90% local GPU). Document and compute split weights accurately from `SignedShardPartitionPlan`!
  3. **Tests (`crates/lucidd/tests/shard_worker.rs`)**:
     - Two real libp2p nodes over loopback: verify unauthorized peer is rejected, `cluster_enabled = false` is rejected, live toggle `cluster_enabled = true` allows bidirectional TCP-over-libp2p stream framing with clean shutdown and zero leaked sockets.

### Deliverable A2: Hard-Quota Public Rendezvous Server (`reachability-plane.md`)
- **Problem Solved**: Currently `crates/lucidd/src/main.rs` leaves `rendezvous_server: None` on Cloud Relay because upstream TTL bounds lack global/per-peer/per-namespace admission quotas, producing `WARN rendezvous discovery failed: Unavailable` every 30s across the fleet.
- **Concrete Implementation**:
  1. Enforce hard admission caps in `crates/phase-net/src/discovery/mod.rs` (max registrations per peer = 4, per namespace = 128, global = 1024, max TTL = 300s, rate-limited per PeerId).
  2. Allow `--mode infrastructure` (or explicit `--enable-rendezvous-server`) in `crates/lucidd/src/main.rs` to enable bounded rendezvous serving safely.
  3. Add adversarial quota tests in `crates/phase-net` verifying flood rejection and clean expiry.

### Done Criteria for Track A
- Zero SSH needed for 2-Mac sliced `llama-server --rpc` over Noise-encrypted `/phase/shard-rpc/1.0.0` (verified in integration tests).
- Public rendezvous server passes flood/quota unit tests and stops `Unavailable` spam when enabled on Cloud Relay.
- Full `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` pass green.

---

## Track B — Correctness-Evidence Validator, Smart Multi-Peer Scheduling, & Desktop Cluster UX
**Recommended Agent:** **GPT 6.2 Sol**
**Branch:** `feat/track-b-validator-and-cluster-ux`

### Exclusive File Ownership
- `crates/phase-protocol/src/**` *(only opaque bounded evidence fields — stay workload-neutral!)*
- `crates/lucidd/src/reputation.rs`
- `crates/lucidd/src/router/**`
- `crates/lucidd/src/ollama/mod.rs` *(for `/api/ps` residency telemetry & trust-label headers only)*
- `apps/lucid-desktop/**`

### Deliverable B1: Independent Correctness-Evidence Seam & First Empirical Validator (`reputation-and-redundant-verification.md`)
- **Problem Solved**: Signed receipts currently prove attributable transcript integrity (`X-Lucid-Receipt-Verified: true`), not that a peer actually ran the claimed model/weights.
- **Concrete Implementation**:
  1. In `crates/phase-protocol/src/worker.rs`: add an optional, strictly bounded opaque evidence attachment on `Completion` / `JobResult` (`evidence: Option<OpaqueEvidence>`, max 8 KiB, schema-versioned — zero inference semantics in `phase-protocol`!).
  2. In `crates/lucidd/src/reputation.rs` & `crates/lucidd/src/router/`:
     - Implement a deterministic empirical validator path for LUCID's existing `--redundant-verification-permille` sampler: when a sampled request finishes on Peer A, dispatch a bounded deterministic audit (`temperature=0, seed=42`, capped `max_tokens`) to an independent Peer B (never self-validating!), compute normalized prefix/token agreement + exact commitment metadata, record a structured `ValidationOutcome` into the append-only observer-local evidence store, and surface an honest HTTP response header (`X-Lucid-Trust-Label: transcript-verified` vs `empirically-spot-checked` vs `trusted-circle-sharded`).
     - Add adversarial unit tests for divergence detection, peer penalty/blocking, and bound enforcement (oversized evidence rejected before parse).

### Deliverable B2: Honest `/api/ps` Residency Telemetry & Latency-Aware Multi-GPU Load Spreading
- **Problem Solved**:
  - `GET /api/ps` currently returns an empty list (`crates/lucidd/src/ollama/mod.rs`).
  - Run #8 proved reputation protects against slow CPU peers, and once Ubuntu Desktop's RTX 5060 Ti CUDA 12.8 comes online alongside Mac M1 Max Metal 4 and Mac M5 Max Metal 4, equal-tier GPUs should spread concurrent (`c=4+`) requests cleanly based on live in-flight load + observed TTFT/tok/s!
- **Concrete Implementation**:
  1. Expose real loaded models from `LlamaCppWorker` / `MlxWorker` (active loaded CID, alias, backend, last-used timestamp — never inventing fake memory numbers) through `GET /api/ps`.
  2. Refine `crates/lucidd/src/router/mod.rs` candidate ranking so peers within the same performance tier spread concurrent `in_flight` requests smoothly while still avoiding dramatically slower CPU-fallback peers (mirroring Run #8 + multi-GPU tests in `crates/lucidd/src/router/tests.rs`).

### Deliverable B3: `apps/lucid-desktop` Live Trusted-Circle Cluster & Sliced-Model UI
- **Problem Solved**: The Tauri v2 + Svelte 5 Cluster modal currently creates plans (`POST /api/lucid/cluster/plan`) but does not surface `shard_stage_bridge_configured`, live `POST /api/lucid/cluster/execute` verification, or pooled GPU memory badges.
- **Concrete Implementation**:
  1. Add Tauri Rust command (`apps/lucid-desktop/src-tauri/src/`) proxying `POST /api/lucid/cluster/execute` strictly via the Rust backend (preserving WebView token isolation & `#![deny(unsafe_code)]`).
  2. Update the Cluster modal (`apps/lucid-desktop/src/`) to display:
     - Pooled cluster GPU memory across connected peers (e.g., `135.6 GiB`),
     - Explicit **"Trusted Circle Sharded"** badge (`trusted-circle-sharded`) and clear disclosure that Tier-2 untrusted sharding is disabled,
     - One-click `cluster_enabled` toggle + live stage-verification test button showing per-stage Ed25519 receipt status.
  3. Verify `npm run check` (0 errors/warnings) and `cargo test --manifest-path apps/lucid-desktop/src-tauri/Cargo.toml --locked`.

### Done Criteria for Track B
- Opaque evidence seam passes protocol + adversarial validator tests with truthful trust labels.
- `GET /api/ps` reports real resident worker models; multi-GPU load-spread tests verify both slow-CPU avoidance and equal-tier GPU concurrency spread.
- `apps/lucid-desktop` passes `npm run check` + `src-tauri` tests/clippy with strict CSP & token isolation intact.

---

## Track C — Physical Fleet Qualification, RTX 5060 Ti CUDA, & `v0.2.0-rc9` Release Ops
**Recommended Agent:** **Antigravity / Operator (after Ubuntu Desktop reboot + Tracks A & B merge)**

### Deliverables
1. **Ubuntu Desktop RTX 5060 Ti CUDA 12.8 Qualification (post-reboot)**:
   - Confirm `nvidia-smi` clears `NVRM Status 62` (`Fan`/`ECC` healthy), verify `v0.2.0-rc8+` auto-loads Ollama's bundled CUDA 12.8 libraries (`CUDA`), run whole-model RTX 5060 Ti benchmark, and run true **2-GPU (RTX 5060 Ti CUDA + Mac M1 Max Metal 4)** and **3-GPU (5060 Ti + M1 Max + M5 Max)** load-spreading benchmarks!
2. **Real Native-Tunneled Sliced `80.76 GB` `DeepSeek-V4-Flash` Run**:
   - Once Track A's `/phase/shard-rpc/1.0.0` merges, slice `DeepSeek-V4-Flash` across **Mac M5 Max + Mac M1 Max + Ubuntu Desktop (5060 Ti)** with zero SSH tunnels and record end-to-end metrics + chained receipts!
3. **Final v0.2 Release Matrix**:
   - Merge Tracks A & B → tag `v0.2.0-rc9` → deploy fleet-wide via `scripts/install-lucid.sh`.
   - Run Linux ARM64 VM qualification (`PHASE_LINUX_AARCH64_EVIDENCE`), 100+ concurrent stream Cloud Relay saturation drill, and keep the private operator progress dashboard updated continuously.

---

## Merge & Integration Order
1. **Track A** and **Track B** develop & test independently in parallel from `feat/lucid-consumer-experience` (`135ef26+`).
2. Merge **Track A** first (adds `/phase/shard-rpc/1.0.0` + rendezvous quotas) → run full workspace CI.
3. Rebase & merge **Track B** (adds correctness-evidence validator + `/api/ps` + Desktop Cluster UI) → run full workspace + Tauri CI.
4. Execute **Track C** physical qualification across all 5 machines and cut `v0.2.0-rc9`!
