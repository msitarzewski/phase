# Security Hardening Plan

**Created:** 2026-05-28
**Source:** [SECURITY-AUDIT-2026-05-28.md](../../../SECURITY-AUDIT-2026-05-28.md) (repo root)
**Owner:** Michael S.
**Status:** ✅ COMPLETE (2026-05-28) — SEC-00 through SEC-12 shipped on branch `security-hardening` in 5 waves; SEC-13 deferred to v0.2. Workspace: 246 tests pass, clippy `-D warnings` clean, `cargo audit` 0 vulns, `cargo deny check` ok. See the remediation table at the top of the audit doc.

This plan turns the 2026-05-28 security audit findings into discrete, independently-shippable tasks. Each `SEC-NN-*.md` file is one task: objective, the finding it closes, exact files, fix approach, acceptance criteria, test plan, dependencies, effort.

---

## Why this exists

The audit found the cryptographic primitives are sound but the **trust model around them is not enforced**, the dependency tree carries 20 advisories (incl. wasmtime sandbox escapes), and DoS guards are missing. The currently-internet-exposed node (umbp) runs `--mode relay` with no worker, so it refuses all jobs — exposure is **latent**. These tasks must land before any worker node is reachable from the internet.

---

## Severity → task map

| Sev | Finding | Task |
|-----|---------|------|
| 🔴 C1 | No signer authorization (Rust execute paths) | [SEC-01](SEC-01-signer-authorization-rust.md) |
| 🔴 C2 | wasmtime 27 sandbox escapes + dep advisories | [SEC-02](SEC-02-dependency-advisories.md) |
| 🔴 C3 | PHP SDK verification bypass (downgrade + magic-string) | [SEC-03](SEC-03-php-sdk-verification.md) |
| 🟠 H1 | Path traversal via model_id + subprocess env | [SEC-04](SEC-04-model-path-subprocess.md) |
| 🟠 H2 | Receipts never verified or bound | [SEC-05](SEC-05-receipt-verification-binding.md) |
| 🟡 M1 | Inbound DoS: no size/concurrency cap, driver-block | [SEC-06](SEC-06-inbound-dos-caps.md) |
| 🟡 M2 | Unbounded model loading + port exhaustion | [SEC-07](SEC-07-worker-resource-limits.md) |
| 🟡 M3 | Non-atomic private-key write | [SEC-08](SEC-08-atomic-key-write.md) |
| 🟡 M4 | DNS bootstrap unauthenticated + unbounded | [SEC-09](SEC-09-dns-bootstrap-hardening.md) |
| 🟡 M6 | Ollama log injection via URI | [SEC-10](SEC-10-log-injection.md) |
| 🔵 L2/L5/L7/L9 | Defense-in-depth hygiene bundle | [SEC-11](SEC-11-hardening-hygiene.md) |
| 🔵 L3/L4 | Dependency hygiene (bincode, unmaintained) | [SEC-12](SEC-12-dependency-hygiene.md) |
| ⚪ — | Wire cargo-audit + cargo-deny into CI | [SEC-00](SEC-00-ci-advisory-gate.md) |
| ⚪ L6 | Content-addressed ModelCid (v0.2) | [SEC-13](SEC-13-content-addressed-cid.md) |

(M5 hickory upgrade and L1 nix upgrade are folded into SEC-02; L8 subprocess env hardening folds into SEC-04.)

---

## Sequencing & dependency graph

```
SEC-00 (CI advisory gate) ───────────── do FIRST; surfaces regressions for everything below
   │
PHASE 1 — CRITICAL (before any worker node is internet-reachable)
   ├── SEC-01 (signer authz, Rust)      ← keystone
   ├── SEC-02 (wasmtime + dep upgrades) ← independent, can run parallel to SEC-01
   └── SEC-03 (PHP SDK)                 ← independent, parallel
   │
PHASE 2 — HIGH
   ├── SEC-04 (model path + subprocess) ← independent
   └── SEC-05 (receipt verify+bind)     ← builds on SEC-01's authz plumbing
   │
PHASE 3 — MEDIUM (DoS hardening, before public worker exposure)
   ├── SEC-06 (inbound DoS caps)        ← touches phase-net + router; coordinate w/ SEC-01
   ├── SEC-07 (worker resource limits)  ← worker_llama only; parallel
   ├── SEC-08 (atomic key write)        ← phase-identity only; parallel
   ├── SEC-09 (DNS bootstrap)           ← main.rs only; parallel
   └── SEC-10 (log injection)           ← ollama.rs only; parallel
   │
PHASE 4 — hygiene
   ├── SEC-11 (hardening bundle)
   └── SEC-12 (dependency hygiene)
   │
PHASE 5 — deferred
   └── SEC-13 (content CID)             ← v0.2, depends on /api/pull design
```

**Critical path to "safe to expose a worker on the internet":** SEC-01 + SEC-02 + SEC-04 + SEC-05 + SEC-06 + SEC-07. The rest hardens, but those six close the anonymous-RCE / compute-theft / DoS exposure.

---

## Coordination notes (avoid merge collisions)

- **`crates/lucidd/src/router.rs`** is touched by SEC-01, SEC-05, SEC-06. Land SEC-01 first, then SEC-05 and SEC-06 rebase onto it. Do not run all three as blind parallel agents on the same file.
- **`crates/lucidd/src/worker_llama.rs`** is touched by SEC-04 and SEC-07. Same crate, different functions — can parallelize carefully but review for collisions.
- **`crates/phase-net/src/discovery.rs`** is touched by SEC-06 (size caps, off-driver spawn). Single owner.
- Everything else is single-file / single-crate and safely parallel.

---

## Definition of done (whole plan)

1. `cargo audit` clean (or every remaining advisory explicitly allow-listed in `deny.toml` with a written justification).
2. `cargo test --workspace` still 210+ passing; each task adds regression tests for its fix.
3. `cargo clippy --workspace --all-targets -- -D warnings` clean.
4. A new integration test proves an unauthorized/self-signed remote job is **rejected** (closes C1 regression).
5. SECURITY-AUDIT doc updated with a "remediation status" column.
6. `cargo audit` runs in CI on every push.

---

## Effort (AI-time, per [[time-framing-ai-not-human]])

Most tasks are 1 focused agent-wave each. SEC-01 and SEC-05 are the meatiest (new authz plumbing + plumbing an expected-key down the call chain). SEC-02 may surface a real blocker if the patched wasmtime line requires API changes — that's the one place to expect friction, not hours of typing. Realistic shape: Phase 1 in one session, Phase 2–3 in a second, hygiene whenever. The blocker is not engineering time; it's deciding the **authorization policy** (allowlist? PeerID-bind? open-with-rate-limit?) in SEC-01 — that's a Michael decision, flagged in that task.
