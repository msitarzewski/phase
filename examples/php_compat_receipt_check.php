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

$artifactsDir = __DIR__ . '/../target/phase-m7-php-compat';
$receiptPath = $artifactsDir . '/receipt.json';
$pkPath = $artifactsDir . '/worker_pubkey.hex';

if (!is_file($receiptPath)) {
    fwrite(STDERR, "Missing $receiptPath. Run the Rust boundary test first.\n");
    exit(2);
}
if (!is_file($pkPath)) {
    fwrite(STDERR, "Missing $pkPath. Run the Rust boundary test first.\n");
    exit(2);
}

$json = file_get_contents($receiptPath);
$pubkeyHex = trim(file_get_contents($pkPath));

$receipt = \Plasm\Receipt::fromJson($json);

if (!$receipt->isSignedEnvelope()) {
    fwrite(STDERR, "FAIL: receipt JSON was not detected as a SignedReceipt envelope.\n");
    exit(1);
}

// Verify against the pinned public key (independent of what's in the JSON).
$ok = $receipt->verify($pubkeyHex);
if (!$ok) {
    fwrite(STDERR, "FAIL: PHP-side signature verification rejected the receipt.\n");
    exit(1);
}

echo "PASS: PHP SDK verified Rust-signed M7 SignedReceipt<JobResult>\n";
echo "  worker_pubkey = " . $receipt->getWorkerPubkey() . "\n";
echo "  job_id        = " . $receipt->getJobId() . "\n";
echo "  completed_at  = " . $receipt->getCompletedAt() . "\n";
echo "  schema        = phase-receipt:v" . $receipt->getSchemaVersion() . "\n";
exit(0);
