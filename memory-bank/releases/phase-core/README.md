# Release: Phase Core (Library Extraction & Generic Worker)

**Status: RELEASED 2026-05-27.** All 8 milestones (M1–M8) shipped. Workspace builds clean, 152 tests passing at the close of this release (210 after LUCID software landed in the same sprint), zero clippy warnings, legacy `daemon/` removed. See `memory-bank/tasks/2026-05/README.md` for the full sprint narrative and `index.yaml` for machine-readable milestone status.

**Scope:** In-place refactor of the existing `daemon/` source tree into properly-structured library crates plus a generic `Worker` trait. No new functional code in this release — every line either moves or generalizes.

**Outcome:** Phase ships as a Cargo workspace of publishable library crates (`phase-net`, `phase-identity`, `phase-manifest`, `phase-receipt`, `phase-protocol`, `phase-artifact-server`), plus a repositioned `plasm` reference WASM node at `crates/plasm/`. LUCID and any future Phase node implementation can depend on these crates by path (during monorepo phase) or by crates.io (post-split).

**Target Platforms:** macOS ARM64, Linux x86_64, Linux ARM64 (unchanged from prior releases)

---

## Vision

```
                 BEFORE (Nov 2025)                          AFTER (this release)
                 ─────────────────                          ────────────────────

  ┌──────────────────────────────────┐         ┌────────────────────────────────────┐
  │           daemon/                 │         │            crates/                   │
  │  ┌────────────────────────────┐  │         │  ┌──────────────────────────────┐  │
  │  │ src/network/  (libp2p)      │  │   ───>  │  │ phase-net/                    │  │
  │  │ src/wasm/     (wasmtime)    │  │         │  │ phase-identity/  ← key fix    │  │
  │  │ src/provider/ (HTTP+sign)   │  │         │  │ phase-manifest/  ← <T> generic│  │
  │  │ src/main.rs   (plasmd CLI)  │  │         │  │ phase-receipt/   ← <T> generic│  │
  │  └────────────────────────────┘  │         │  │ phase-protocol/  ← NEW: trait │  │
  │  One crate, tightly coupled.     │         │  │ phase-artifact-server/        │  │
  │  WASM execution baked into       │         │  │ plasm/  ← old daemon, now a   │  │
  │  the protocol layer.             │         │  │           reference Phase node│  │
  └──────────────────────────────────┘         │  └──────────────────────────────┘  │
                                               │  Library crates publish independently;│
                                               │  Plasm is one Worker implementation. │
                                               └────────────────────────────────────┘
```

**The protocol stops being a WASM daemon and becomes a substrate.** Plasm becomes one of potentially many Phase node implementations — equal-citizen to LUCID, which arrives in the [next release](../lucid/).

---

## Why this release exists

The Phase Open MVP and Phase Boot shipped in November 2025 but bundled their reusable substrate (libp2p, signed manifests, DHT, identity) inside a single `daemon/` crate tightly coupled to wasmtime execution. This release acknowledges that the substrate **is the protocol**, and that WASM execution is just one workload type among many (LUCID's inference workers being the next).

Extracting clean libraries unblocks every future Phase node implementation. It also closes a known issue from `progress.md` — ephemeral Ed25519 keys regenerated per session — by giving `phase-identity` a proper persistent-storage home.

---

## Milestones

| ID | Name | Description |
|----|------|-------------|
| M1 | Workspace Scaffold | Cargo workspace at repo root; empty crate skeletons with per-crate `Cargo.toml`; SPDX headers |
| M2 | Extract `phase-net` | libp2p, Kademlia DHT, mDNS, Noise+QUIC — move from `daemon/src/network/` |
| M3 | Extract `phase-identity` | Ed25519 keypair + persistent on-disk storage (fixes the ephemeral-key bug) |
| M4 | Define `phase-protocol` | NEW crate: generic `JobSpec` enum + `Worker` trait (the new abstraction) |
| M5 | Extract `phase-manifest` + `phase-receipt` | Unify and generalize as `SignedManifest<T>` / `SignedReceipt<T>` |
| M6 | Extract `phase-artifact-server` | Content-addressed HTTP server — currently boot-specific in `daemon/src/provider/`, made generic |
| M7 | Reposition Plasm | Move existing wasmtime daemon into `crates/plasm/`; `WasmtimeWorker` impls `phase-protocol::Worker` |
| M8 | Verification & Docs | All 80+ existing tests pass; libp2p upgraded; README reflects new structure; old `daemon/` removed |

---

## Definition of Done

1. **`cargo build --workspace` succeeds clean** with all crates extracted into `crates/`
2. **All 80 existing tests pass** — regressions are blockers
3. **`crates/plasm` produces a `plasmd` binary** functionally identical to the pre-refactor version (no behavioral change)
4. **`phase-protocol` defines `JobSpec` and `Worker`** in a way that supports both `WasmJobSpec` (Plasm) and a placeholder `InferenceJobSpec` (LUCID-ready)
5. **Persistent Ed25519 keypair** — node identity survives daemon restart
6. **Per-crate license declarations** — Apache-2.0 for all `phase-*` crates and `plasm`; LUCID's eventual AGPL-3.0 declared per-crate when added
7. **libp2p upgraded to current stable** (was 0.54 in Nov 2025; verify and upgrade during M2)
8. **README and Memory Bank updated** to reflect new structure; old `daemon/` directory removed from working tree (history preserved in git)

---

## The Generic `Worker` Trait (the one piece of new code in this release)

```rust
// crates/phase-protocol/src/lib.rs

pub enum JobSpec {
    Wasm(WasmJobSpec),
    Inference(InferenceJobSpec),  // LUCID will use this
    // Future: ImageGen, FineTune, Render, Science, ...
}

#[async_trait::async_trait]
pub trait Worker: Send + Sync {
    fn supported_kinds(&self) -> &[JobSpecKind];
    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<SignedReceipt<JobResult>>;
}
```

The existing wasmtime execution code becomes `WasmtimeWorker: Worker` inside `crates/plasm/`. LUCID's `LlamaCppWorker: Worker` and `MlxWorker: Worker` slot in alongside without any further protocol changes. New workload types (image generation, scientific compute) become new `JobSpec` variants without breaking anything that already works.

---

## Architecture Integration

### Existing Components (preserved)

- `phase-discover`, `phase-verify`, `phase-fetch` binaries — kept under `boot/` as before
- Phase Boot's netboot infrastructure — untouched in this release
- The wasmtime runtime, WASI preview1 support, hello.wasm example — all preserved inside `crates/plasm/`
- libp2p / DHT / mDNS / Noise+QUIC behavior — bit-for-bit identical, just relocated

### New Components

- `phase-protocol` crate — `JobSpec` enum, `Worker` trait, `JobSpecKind` enum
- Persistent keypair storage in `phase-identity` (~/.config/phase/identity.key or similar; platform-aware via `dirs` crate)

### Removed

- Top-level `daemon/` directory (contents relocated; the directory itself goes away)
- `#[allow(dead_code)]` suppressions — already zero per November cleanup, must stay zero

---

## Dependencies

- **None upstream** — this is a pure refactor of existing code
- **Downstream consumers** — the [LUCID release](../lucid/) depends on the crates this release produces

---

## Test Scenarios

| Scenario | Description | Expected |
|---|---|---|
| Pre-refactor baseline | Run full test suite on current `main` | 80 passing, 0 warnings |
| Per-milestone checkpoint | Run full test suite after each `M*` | 80 passing, 0 warnings |
| `plasmd run hello.wasm` | Run a WASM job through repositioned Plasm | Returns "dlroW ,olleH" identical to pre-refactor |
| Cross-crate dependency | LUCID skeleton `lucidd` depends on `phase-net` by path | `cargo build -p lucidd` succeeds |
| Persistent identity | Restart `plasmd`, verify peer ID unchanged | Same Ed25519 public key across restarts |
| Workspace publish dry-run | `cargo publish --dry-run -p phase-net` etc. | All `phase-*` crates publish-ready (no path-only deps, etc.) |

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| libp2p major-version drift since Nov 2025 (was 0.54) | Upgrade during M2 extraction, not before or after — pay the migration cost once |
| Tests break during crate split | Move file-by-file with `git mv`; run tests after each `M*`; treat any regression as a blocker not a "fix later" |
| `JobSpec` enum design proves wrong for LUCID | Treat M4 as a checkpoint — pause LUCID work until trait shape validated against a stub `LlamaCppWorker` |
| Plasm binary diverges from pre-refactor behavior | M7 acceptance requires byte-identical output for `hello.wasm` round-trip test |
| Crate dependency graph becomes circular | Define dependency direction once: `phase-net` → `phase-identity` → `phase-manifest`/`phase-receipt` → `phase-protocol`; no upward references |

---

## Release Owner

**Owner:** Michael S.
**Contributors:** Library extraction (all crates), Protocol design (phase-protocol), Identity persistence (phase-identity)

---

## Files

- [index.yaml](./index.yaml) — Machine-readable milestone index
- (tasks/M1/ through tasks/M8/ subdirectories will be created as work begins on each milestone)
