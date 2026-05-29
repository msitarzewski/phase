#!/usr/bin/env php
<?php
/**
 * Phase Local WASM Execution Example
 *
 * This demonstrates local execution of WASM modules using the PHP SDK.
 */

require_once __DIR__ . '/../php-sdk/src/Client.php';
require_once __DIR__ . '/../php-sdk/src/Job.php';
require_once __DIR__ . '/../php-sdk/src/Manifest.php';
require_once __DIR__ . '/../php-sdk/src/Receipt.php';
require_once __DIR__ . '/../php-sdk/src/Result.php';
require_once __DIR__ . '/../php-sdk/src/Transport/TransportInterface.php';
require_once __DIR__ . '/../php-sdk/src/Transport/LocalTransport.php';

use Plasm\Client;

// Create client in local mode. As of phase-core M7 the daemon binary lives
// at `target/release/plasm` in the workspace target dir; we also try
// `target/release/plasmd` for the explicit bin name.
$plasmdBin = __DIR__ . '/../target/release/plasmd';
if (!is_executable($plasmdBin)) {
    $plasmdBin = __DIR__ . '/../target/release/plasm';
}
$client = new Client([
    'mode' => 'local',
    'plasmd_path' => $plasmdBin,
]);

echo "Phase Local WASM Execution Demo\n";
echo "================================\n\n";

// Create and submit job
echo "1. Creating job for hello.wasm...\n";
$job = $client->createJob(__DIR__ . '/hello.wasm')
    ->withCpu(1)
    ->withMemory(128)
    ->withTimeout(30);

echo "2. Submitting job with input: 'Hello, World'\n";
$result = $job->submit("Hello, World");

echo "3. Execution complete!\n\n";

// Display results
echo "Results:\n";
echo "--------\n";
echo "Output: " . $result->stdout() . "\n";
echo "Exit code: " . $result->exitCode() . "\n";
echo "Wall time: " . $result->receipt()->getWallTimeMs() . "ms\n";

// Trust for LOCAL execution comes from the transport context, NOT from the
// receipt object. We ran this job ourselves through LocalTransport, so we
// already know it executed locally — there is no remote signer to pin.
//
// Receipt::verify() now REQUIRES a pinned public key and only trusts a real
// `phase-receipt:v1:` Ed25519 envelope. A local mock receipt is unsigned, so
// verify() would (correctly) return false. We therefore do NOT call verify()
// here; the security guarantee is "we executed it", established by transport.
//
// If you ever receive a receipt over an untrusted channel, see remote_test.php
// for the secure pinned-key verification pattern.
echo "Execution trusted via local transport context (no remote signature).\n";

if ($result->isSuccess()) {
    echo "\n✓ Success!\n";
    exit(0);
} else {
    echo "\n✗ Failed with exit code " . $result->exitCode() . "\n";
    exit(1);
}
