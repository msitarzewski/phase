# SEC-00 — Wire cargo-audit + cargo-deny into CI

**Severity:** Process gap (enabler) | **Phase:** 0 — do first | **Effort:** small (1 agent-wave) | **Status:** planned

## Why
This audit found 20 dependency advisories — including wasmtime sandbox escapes — only because `cargo audit` was run by hand. `cargo-audit` wasn't even installed. There is currently **no automated dependency-advisory check**, which is how a vulnerable wasmtime line slipped in unnoticed. Fixing the deps (SEC-02) without this gate means they'll silently rot again.

## Scope
- New: `deny.toml` at repo root (cargo-deny config)
- New: CI workflow step (or `.git/hooks/pre-push` if no CI yet — check whether `.github/workflows/` exists; if the repo has no CI, add a `Makefile`/`justfile` target `make audit` and a documented pre-release checklist instead)
- Update: `memory-bank/projectRules.md` — add "run `cargo audit` before any dependency change or release"

## Approach
1. `cargo install cargo-audit cargo-deny` (audit already installed locally this session).
2. Author `deny.toml`:
   - `[advisories]` — `vulnerability = "deny"`, `unmaintained = "warn"` (so SEC-12's bincode/fxhash/mach/paste don't block the build but stay visible).
   - `[bans]` — deny multiple versions of security-critical crates (we have duplicate hickory + base64); allow-list the known dupes with justification.
   - `[licenses]` — allow Apache-2.0, MIT, BSD, ISC, Unicode; this also surfaces any GPL contamination that would conflict with Phase's Apache-2.0 / LUCID's AGPL-3.0 split.
3. Add CI: if `.github/workflows/` exists, add a `security.yml` running `cargo audit --deny warnings` (after SEC-02/SEC-12 land — until then it fails on the known 20) and `cargo deny check`. If no CI exists yet, add `make audit` + note in projectRules.
4. **Ordering caveat:** the `cargo audit` gate will RED until SEC-02 + SEC-12 land. Either (a) land SEC-02 first then turn the gate on, or (b) commit the gate with the current advisories explicitly allow-listed in `deny.toml` and remove them as each is fixed. Prefer (a) for honesty.

## Acceptance criteria
- `deny.toml` exists and `cargo deny check` runs.
- `cargo audit` is invocable via a documented command (`make audit` or CI).
- projectRules.md documents the policy.
- After SEC-02 + SEC-12: `cargo audit` exits 0.

## Test plan
- Run `cargo deny check` locally — confirm it parses and reports.
- Confirm the CI step (or make target) fails when a known-vuln crate is present and passes once clean.

## Dependencies
None to start. The "turn the gate green" step depends on SEC-02 + SEC-12.

## Notes
Check first: does this repo have CI at all? `ls .github/workflows/`. The audit + this plan assume it may not — if not, SEC-00 also seeds the first CI workflow, which is independently valuable.
