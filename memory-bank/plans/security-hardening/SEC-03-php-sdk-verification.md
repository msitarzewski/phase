# SEC-03 — Harden PHP SDK receipt verification

**Severity:** 🔴 CRITICAL (C3 + PHP face of C1) | **Phase:** 1 | **Effort:** small–medium (1 agent-wave) | **Status:** planned

## Why
The PHP SDK is a *verification* library that can be made to accept forged receipts three ways:
1. **Dual-format downgrade** (`Receipt.php:62-89`, `Crypto.php:80-102`): the strong Ed25519/canonical-JSON path is chosen only if `schema_version` AND `result` keys are present; otherwise it silently falls to a legacy SHA-256-of-pipe-fields path. An attacker omits those keys → legacy branch → supplies own pubkey+signature → forges any `module_hash`/`exit_code`.
2. **Key-substitution** (`Receipt.php:155-170`, `Crypto.php:42`): `verify()` with no pinned key checks the signature against the receipt's *own embedded* pubkey. Both shipped examples (`examples/remote_test.php:94`, `examples/local_test.php:52`) use this insecure no-arg form.
3. **`local_execution` magic string** (`Receipt.php:158-159`): any legacy receipt with `node_pubkey === "local_execution"` returns `true` with no signature check at all.

## Scope
- `php-sdk/src/Receipt.php` — verification logic, format detection, the `local_execution` branch.
- `php-sdk/src/Crypto.php` — `getCanonicalMessage`, `verifyReceipt`, the legacy path.
- `examples/remote_test.php`, `examples/local_test.php` — fix the demonstrated-insecure usage.
- `examples/php_compat_receipt_check.php` — extend to assert canonical-bytes equality (see SEC also L0 note below).

## Approach
1. **Make the pinned key mandatory for trust.** `verify(?string $expectedPubkeyHex)` — if `$expectedPubkeyHex` is null, either throw or return a clearly-named `UNVERIFIED` state, never `true`. The receipt-embedded key may be used only to *display* who claims to have signed, never to decide trust.
2. **Drop legacy verification entirely** for security decisions. The new `phase-receipt:v1:` Ed25519 path is the only trusted path. If legacy receipts must still be *parsed* for display, parse them but make `verify()` return false/throw for them. (Confirm with Michael whether any deployed PHP consumer still produces legacy receipts — the Rust side migrated to dual-format in M7, so legacy is inbound-compat only.)
3. **Delete the `local_execution` branch.** Local-execution trust must come from transport context (the caller knows it ran locally), never from a field inside the untrusted object.
4. **Constant-time + correctness already OK** — the audit confirmed `===` length checks and libsodium `sodium_crypto_sign_verify_detached` (constant-time). Don't regress these.
5. Update both examples to pass a known-good pubkey and show the secure pattern.

## Acceptance criteria
- `verify()` with no pinned key never returns `true`.
- A legacy-shaped receipt cannot pass verification (no downgrade path to trust).
- `node_pubkey: "local_execution"` no longer bypasses anything.
- A receipt forged with an attacker keypair fails verification against the expected (genuine) pubkey.
- A genuine Rust-produced `SignedReceipt<JobResult>` still verifies (the existing `boundary_php_compat_receipt` round-trip).
- Both examples demonstrate the secure pinned-key pattern.

## Test plan
- PHP test: forged legacy receipt → `verify()` is false/throws.
- PHP test: `local_execution` receipt → no longer auto-true.
- PHP test: M7 receipt signed by key X, verified against key Y → false; against key X → true.
- Cross-impl: Rust signs a receipt, PHP verifies against the published pubkey → true (existing harness).

## Dependencies
None — separate codebase from the Rust work. Fully parallel to SEC-01/SEC-02.

## Related (fold in if cheap): L0 canonical-bytes divergence
The audit flagged possible PHP↔Rust canonical-JSON byte divergence (numeric-string key coercion, float formatting — `Crypto.php:112-153` vs `phase-receipt/canonical.rs`). Extend `php_compat_receipt_check.php` to assert the canonical *bytes* match (not just verify-pass), with fixtures containing floats and numeric keys. A divergence is an availability bug at best and a forgery vector at worst.
