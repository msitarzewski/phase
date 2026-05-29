#!/usr/bin/env php
<?php
/**
 * Phase Core M7 — PHP-side verification of a Rust-signed receipt.
 *
 * Companion to crates/plasm/tests/boundary_php_compat_receipt.rs. Run that
 * test first; it writes a `SignedReceipt<JobResult>` JSON envelope plus the
 * worker's hex public key into `target/phase-m7-php-compat/`. This script
 * reads those, runs them through the migrated PHP `Crypto::verifyReceipt`
 * path, and reports pass/fail.
 *
 * Usage:
 *     cargo test -p plasm --test boundary_php_compat_receipt
 *     php examples/php_compat_receipt_check.php
 */

require_once __DIR__ . '/../php-sdk/src/Crypto.php';
require_once __DIR__ . '/../php-sdk/src/Receipt.php';

use Plasm\Crypto;
use Plasm\Receipt;

$failures = 0;
function check(bool $cond, string $label): void
{
    global $failures;
    if ($cond) {
        echo "PASS: $label\n";
    } else {
        echo "FAIL: $label\n";
        $failures++;
    }
}

// ---------------------------------------------------------------------------
// Part 1 (L0): canonical-JSON BYTE equality.
//
// The Ed25519 signature is computed over the exact canonical-JSON bytes, so a
// single-byte divergence between Rust's `to_canonical_bytes` and PHP's
// `canonicalJsonEncode` is a silent forgery/availability vector. We assert the
// PHP output byte-for-byte against a hand-constructed expected string. The
// fixture deliberately includes:
//   - a float (`3.5`, `0.5`) to pin float formatting,
//   - numeric-string keys (`"10"`, `"2"`) to pin key coercion + sort order,
//   - unsorted nested keys to pin recursive lexicographic sorting.
//
// Sort order note: serde_json sorts object keys as STRINGS. PHP coerces the
// numeric-string keys "10"/"2" to ints, and we ksort(SORT_STRING), so "10"
// orders before "2" — identical to serde_json's lexicographic ordering.
//
// CAVEAT (documented divergence): PHP coerces whole-valued floats like 1.0 to
// the integer 1, emitting `1`, whereas Rust serde_json emits `1.0`. The fixture
// avoids whole-valued floats for that reason. A future hardening of
// canonicalJsonEncode should force float formatting to match serde_json; for
// the M7 JobResult shape (integer metrics + strings) this does not arise.
$fixture = [
    'zeta'   => 1,
    'alpha'  => 2,
    'nested' => ['10' => 'ten', '2' => 'two', 'b' => 3.5, 'a' => true],
    'ratio'  => 0.5,
];
$expectedBytes = '{"alpha":2,"nested":{"10":"ten","2":"two","a":true,"b":3.5},"ratio":0.5,"zeta":1}';
$actualBytes = Crypto::canonicalJsonEncode($fixture);
check(
    $actualBytes === $expectedBytes,
    "canonical-JSON bytes match expected fixture\n      expected: $expectedBytes\n      actual:   $actualBytes"
);

// ---------------------------------------------------------------------------
// Part 2: Rust -> PHP cross-impl receipt verification (requires the boundary
// test to have produced the fixture). Skipped (not failed) if absent.
$artifactsDir = __DIR__ . '/../target/phase-m7-php-compat';
$receiptPath = $artifactsDir . '/receipt.json';
$pkPath = $artifactsDir . '/worker_pubkey.hex';

if (!is_file($receiptPath) || !is_file($pkPath)) {
    echo "SKIP: Rust fixture not present ($artifactsDir).\n";
    echo "      Run: cargo test -p plasm --test boundary_php_compat_receipt\n";
    exit($failures === 0 ? 0 : 1);
}

$json = file_get_contents($receiptPath);
$pubkeyHex = trim(file_get_contents($pkPath));
$receipt = Receipt::fromJson($json);

check($receipt->isSignedEnvelope(), "Rust receipt detected as a v1 SignedReceipt envelope");

// (a) Genuine receipt verifies against the correct pinned key.
check($receipt->verify($pubkeyHex) === true, "genuine Rust receipt verifies against the correct pinned key");

// (b) Same receipt MUST fail against a different (wrong) pinned key. We flip
//     the last hex nibble to derive a syntactically-valid but wrong key.
$wrongKey = substr($pubkeyHex, 0, -1) . (substr($pubkeyHex, -1) === 'a' ? 'b' : 'a');
$wrongResult = false;
try {
    $wrongResult = $receipt->verify($wrongKey);
} catch (\Throwable $e) {
    $wrongResult = false; // a bad-key shape throwing is also "not trusted"
}
check($wrongResult === false, "Rust receipt does NOT verify against a wrong pinned key");

echo "\n";
echo "  worker_pubkey (claimed) = " . $receipt->getNodePubkey() . "\n";
echo "  job_id                  = " . $receipt->getJobId() . "\n";
echo "  completed_at            = " . $receipt->getCompletedAt() . "\n";
echo "  schema                  = phase-receipt:v" . $receipt->getSchemaVersion() . "\n";

exit($failures === 0 ? 0 : 1);
