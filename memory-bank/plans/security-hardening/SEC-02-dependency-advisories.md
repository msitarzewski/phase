# SEC-02 — Upgrade wasmtime + close dependency advisories

**Severity:** 🔴 CRITICAL (C2) + 🟡 M5 + 🔵 L1 | **Phase:** 1 | **Effort:** medium — *possible real blocker* | **Status:** planned

## Why
`cargo audit` (run by hand 2026-05-28) found **20 vulnerabilities**. The serious ones:
- **wasmtime 27.0.0 — 16 advisories**, incl. **sandbox escapes**: RUSTSEC-2026-0096 (aarch64 Cranelift miscompile → guest heap escape — you run ARM64 everywhere), RUSTSEC-2026-0095 (Winch sandbox escape), RUSTSEC-2026-0087, RUSTSEC-2025-0118; plus RUSTSEC-2026-0149 (`wasmtime-wasi` path_open TRUNCATE bypasses FilePerms::WRITE) and ~11 panic/DoS/data-leak issues.
- **hickory-proto** (DNS, freshly added in `--bootstrap-dns`): RUSTSEC-2026-0119 (O(n²) CPU exhaustion, both 0.24.4 + 0.25.2 present), RUSTSEC-2026-0118 (NSEC3 unbounded loop, 0.25.2).
- **nix 0.19.1** — RUSTSEC-2021-0119 (OOB write in `getgrouplist`), old transitive.

wasmtime sandbox escape + SEC-01's anonymous-WASM-execution = anonymous remote host RCE on any exposed plasm node. This is the highest-impact dependency issue.

## Scope
- `crates/plasm/Cargo.toml` — bump wasmtime (and wasmtime-wasi) to the patched version. Check each advisory's "Patched versions" — likely the latest 27.x point release fixes most; some may require ≥28/29. **This is where a real blocker may surface:** a major wasmtime bump can change the `Engine`/`Store`/`Linker`/WASI API. Budget for API migration in `plasm/src/wasm/runtime.rs`, not just a version string.
- `crates/lucidd/Cargo.toml` — bump hickory-resolver to a line pulling patched hickory-proto; dedupe the two hickory versions.
- Find the `nix 0.19.1` dependent (`cargo tree -i nix@0.19.1`) and bump whatever pulls it (likely an old transitive of a libp2p or system crate).

## Approach
1. `cargo audit` → get exact patched-version targets per advisory.
2. wasmtime: try the minimal bump that clears the advisories first (`cargo update -p wasmtime --precise <ver>`); if that pulls a semver-incompatible line, update Cargo.toml and migrate the runtime API. **Verify hello.wasm still produces byte-identical `dlroW ,olleH`** (the existing M7 acceptance test) and module hash unchanged after the bump.
3. hickory: `cargo update -p hickory-resolver`; confirm `--bootstrap-dns bootstrap.phasebased.net` still resolves + dials (the live test from this session).
4. nix: bump the dependent; rebuild.
5. Rebuild all three dist targets (aarch64-darwin, aarch64-linux, x86_64-linux), refresh `dist/`, redeploy umbp's relay binary.
6. `cargo audit` → confirm the vuln count drops to 0 (unmaintained warnings remain → SEC-12).

## Acceptance criteria
- `cargo audit` reports **0 vulnerabilities** (4 unmaintained warnings acceptable, tracked in SEC-12).
- hello.wasm round-trip byte-identical post-wasmtime-bump.
- `--bootstrap-dns` still works against the live TXT record.
- All three dist binaries rebuilt; umbp relay restarted on the new binary.
- 210 tests pass.

## Test plan
- `cargo audit` before/after.
- plasm hello.wasm round-trip (`crates/plasm/examples/peer_id_check.rs` + the WASM test).
- lucidd DNS bootstrap live test (Mac → umbp via `--bootstrap-dns`).

## Dependencies
None. Run parallel to SEC-01/SEC-03. **But SEC-00's CI gate should turn green only after this lands.**

## Notes / blocker flag
The wasmtime major-version migration is the single most likely place in the whole plan to need real debugging rather than mechanical edits. If the patched line is a major bump with breaking API, treat the runtime.rs migration as its own sub-effort and re-verify the WASM sandbox behavior carefully — ironically, a sloppy wasmtime upgrade could introduce the very sandbox issues we're closing.
