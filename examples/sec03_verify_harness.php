#!/usr/bin/env php
<?php
/**
 * SEC-03 regression harness — proves the three receipt-verification bypasses
 * are closed and that a genuine v1 signed receipt still verifies.
 *
 * No PHPUnit suite is wired into php-sdk/ (composer.json references a tests/
 * dir that does not exist and PHPUnit is not configured for these classes),
 * so this runnable script is the sanctioned harness per SEC-03's test plan.
 * It self-contains its fixtures (signs a real Ed25519 receipt with libsodium)
 * and prints PASS/FAIL for each case, exiting non-zero on any failure.
 *
 * Usage:  php examples/sec03_verify_harness.php
 */

require_once __DIR__ . '/../php-sdk/src/Crypto.php';
require_once __DIR__ . '/../php-sdk/src/Receipt.php';

use Plasm\Crypto;
use Plasm\Receipt;

$failures = 0;
function check(bool $cond, string $label): void
{
    global $failures;
    echo ($cond ? "PASS" : "FAIL") . ": $label\n";
    if (!$cond) {
        $failures++;
    }
}

echo "SEC-03 receipt-verification bypass harness\n";
echo "==========================================\n\n";

// ---------------------------------------------------------------------------
// Build a GENUINE v1 signed receipt with a known keypair (key "X").
// We construct the exact canonical signing message the SDK verifies against
// and sign it, so this is a real Ed25519 signature over real canonical bytes.
// ---------------------------------------------------------------------------
$kpX = sodium_crypto_sign_keypair();
$skX = sodium_crypto_sign_secretkey($kpX);
$pkXhex = bin2hex(sodium_crypto_sign_publickey($kpX));

$kpY = sodium_crypto_sign_keypair();
$pkYhex = bin2hex(sodium_crypto_sign_publickey($kpY)); // a DIFFERENT trusted key

$result = [
    'metrics' => [
        'total_duration_ms' => 8,
        'extra' => ['module_hash' => 'sha256:deadbeef', 'exit_code' => 0],
    ],
];
$jobId = str_repeat('11', 32);
$completedAt = '2026-05-28T12:00:00Z';

// Mirror Crypto::getCanonicalMessage's envelope so we sign the right bytes.
$signingEnvelope = [
    'completed_at'   => $completedAt,
    'job_id'         => $jobId,
    'result'         => $result,
    'schema_version' => 1,
];
$message = Crypto::SIGNING_DOMAIN . Crypto::canonicalJsonEncode($signingEnvelope);
$sigXhex = bin2hex(sodium_crypto_sign_detached($message, $skX));

$genuineJson = json_encode([
    'schema_version' => 1,
    'result'         => $result,
    'job_id'         => $jobId,
    'worker_pubkey'  => $pkXhex,
    'signature'      => $sigXhex,
    'completed_at'   => $completedAt,
]);

// ---------------------------------------------------------------------------
// Sanity: the genuine receipt verifies against its real signer (key X).
// ---------------------------------------------------------------------------
$genuine = Receipt::fromJson($genuineJson);
check(
    $genuine->verify($pkXhex) === true,
    "genuine v1 receipt verifies against the correct pinned key X"
);

// ===========================================================================
// CASE (c): M7 receipt signed by X, verified against Y -> false; against X -> true.
// ===========================================================================
check(
    $genuine->verify($pkYhex) === false,
    "(c) receipt signed by X is REJECTED when verified against a different key Y"
);
check(
    $genuine->verify($pkXhex) === true,
    "(c) same receipt is ACCEPTED when verified against the correct key X"
);

// ===========================================================================
// CASE (1): dual-format downgrade. An attacker takes the genuine result data
// but ships it in the LEGACY shape (no schema_version/result keys) with their
// OWN pubkey + a self-made "signature". Must be rejected (legacy never trusts).
// ===========================================================================
$forgedLegacy = Receipt::fromJson(json_encode([
    'version'      => '0.1',
    'module_hash'  => 'sha256:deadbeef',
    'exit_code'    => 0,
    'wall_time_ms' => 8,
    'timestamp'    => 1779896995,
    'node_pubkey'  => $pkXhex,        // claims to be X
    'signature'    => str_repeat('00', 64),
]));
check(!$forgedLegacy->isSignedEnvelope(), "(1) forged legacy receipt parses as a non-envelope (legacy) shape");
// Even pinning the *real* key X must not rescue a legacy receipt.
check(
    $forgedLegacy->verify($pkXhex) === false,
    "(1) forged legacy receipt is REJECTED even against the genuine key X (no downgrade-to-trust path)"
);

// ===========================================================================
// CASE (2): key substitution. Old API allowed verify() with no pinned key,
// checking the receipt's own embedded key. That zero-arg form no longer exists
// (verify() requires a pinned key), so an attacker-self-signed receipt cannot
// be trusted by omitting the key.
// ===========================================================================
// Attacker signs a receipt with their OWN key but stamps key X as worker_pubkey.
$attackerKp  = sodium_crypto_sign_keypair();
$attackerSk  = sodium_crypto_sign_secretkey($attackerKp);
$attackerSig = bin2hex(sodium_crypto_sign_detached($message, $attackerSk));
$substituted = Receipt::fromJson(json_encode([
    'schema_version' => 1,
    'result'         => $result,
    'job_id'         => $jobId,
    'worker_pubkey'  => $pkXhex,         // LIES: claims X signed it
    'signature'      => $attackerSig,    // actually signed by attacker
    'completed_at'   => $completedAt,
]));
// Verifying against the trusted key X must fail: the signature isn't X's.
check(
    $substituted->verify($pkXhex) === false,
    "(2) attacker self-signed receipt claiming to be X is REJECTED against pinned key X"
);
// And the embedded key is never auto-trusted — confirm no zero-arg form exists.
$zeroArgForbidden = false;
try {
    // @phpstan-ignore-next-line — intentionally calling with no argument.
    $substituted->verify();
} catch (\ArgumentCountError $e) {
    $zeroArgForbidden = true;
}
check($zeroArgForbidden, "(2) verify() cannot be called without a pinned key (no insecure zero-arg form)");

// ===========================================================================
// CASE (3): local_execution magic string. A legacy receipt with
// node_pubkey === "local_execution" used to auto-return true. The branch is
// deleted; it must now behave like any other legacy receipt (rejected).
// ===========================================================================
$localExec = Receipt::createMock('sha256:deadbeef', 0, 8); // sets node_pubkey=local_execution
check(
    $localExec->getNodePubkey() === 'local_execution',
    "(3) mock receipt still carries node_pubkey=local_execution (display field intact)"
);
check(
    $localExec->verify($pkXhex) === false,
    "(3) local_execution receipt no longer auto-verifies — REJECTED"
);
// Also reject when caller passes the literal magic string as a 'key'.
$magicRejected = false;
try {
    $magicRejected = ($localExec->verify('local_execution') === false);
} catch (\Throwable $e) {
    $magicRejected = true; // throwing on a bogus key is also "not trusted"
}
check($magicRejected, "(3) passing 'local_execution' as the pinned key does not bypass verification");

echo "\n";
if ($failures === 0) {
    echo "ALL CHECKS PASSED\n";
    exit(0);
}
echo "$failures CHECK(S) FAILED\n";
exit(1);
