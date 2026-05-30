<?php
/**
 * Phase Remote Execution Test
 *
 * This example demonstrates:
 * - Node discovery via Kademlia DHT
 * - Job submission with resource requirements
 * - Remote WASM execution
 * - Receipt verification with Ed25519 signatures
 */

require __DIR__ . '/../php-sdk/vendor/autoload.php';

use Plasm\Client;
use Plasm\Manifest;

// SECURITY: the public key of the worker(s) we trust. In a real deployment
// this comes from an operator allowlist, a libp2p PeerId binding, or a
// pre-shared/published key — NEVER from the receipt itself. Replace this with
// the hex Ed25519 public key of the node you trust before running.
$EXPECTED_WORKER_PUBKEY = getenv('PHASE_EXPECTED_WORKER_PUBKEY') ?: '';

// Output helpers
function section($title) {
    echo "\n" . $title . "\n";
    echo str_repeat("=", strlen($title)) . "\n\n";
}

function success($msg) {
    echo "✓ " . $msg . "\n";
}

function info($msg, $indent = 2) {
    echo str_repeat(" ", $indent) . $msg . "\n";
}

function error($msg) {
    echo "✗ " . $msg . "\n";
    exit(1);
}

// Start test
section("Phase Remote Execution Test");

// Initialize client
$client = new Client(['mode' => 'remote']);

// Step 1: Node Discovery
echo "Discovering nodes...\n";
try {
    $nodes = $client->discover(['arch' => 'x86_64', 'runtime' => 'wasmtime']);

    if (empty($nodes)) {
        error("No nodes discovered. Ensure plasmd is running on a remote host.");
    }

    $node = $nodes[0];
    success("Node discovered: " . substr($node['peer_id'], 0, 16) . "...");
    info("Architecture: " . $node['arch']);
    info("Runtime: " . $node['runtime']);
} catch (Exception $e) {
    error("Discovery failed: " . $e->getMessage());
}

// Step 2: Job Submission
echo "\nSubmitting job: hello.wasm\n";
try {
    $manifest = new Manifest()
        ->setModulePath(__DIR__ . '/../wasm-examples/hello/target/wasm32-wasip1/release/hello.wasm')
        ->setCpuCores(1)
        ->setMemoryMb(128)
        ->setTimeoutSeconds(30);

    $jobId = $client->submit($manifest);
    success("Job submitted: " . $jobId);
} catch (Exception $e) {
    error("Submission failed: " . $e->getMessage());
}

// Step 3: Wait for Execution
echo "\nWaiting for execution...\n";
try {
    $result = $client->wait($jobId, ['timeout' => 60]);
    success("Execution complete");
} catch (Exception $e) {
    error("Execution failed: " . $e->getMessage());
}

// Step 4: Display Results
section("Result");
info("Output: " . $result->getStdout());
info("Exit code: " . $result->getExitCode());
info("Wall time: " . $result->getWallTimeMs() . "ms");

// Step 5: Verify Receipt
section("Receipt Verification");
try {
    $receipt = $result->getReceipt();

    if ($EXPECTED_WORKER_PUBKEY === '') {
        error(
            "Refusing to verify without a pinned worker public key. Set "
            . "PHASE_EXPECTED_WORKER_PUBKEY to the hex Ed25519 key of the "
            . "worker you trust. The key inside the receipt is attacker-"
            . "controlled and must never be used to decide trust."
        );
    }

    // SECURE PATTERN: verify against the PINNED key we already trust. The key
    // embedded in the receipt is shown only for display (`getNodeId()`), never
    // used for the trust decision. A forged or downgraded receipt fails here.
    if ($receipt->verify($EXPECTED_WORKER_PUBKEY)) {
        success("Signature valid (verified against pinned worker key)");
        success("Receipt verified");
    } else {
        error("Receipt verification failed — signature does not match the pinned key, or receipt is not a trusted v1 signed envelope.");
    }

    // Display receipt details. getNodeId()/getNodePubkey() are DISPLAY ONLY:
    // they report who *claims* to have signed, not who we trust.
    echo "\nReceipt Details:\n";
    info("Job ID: " . $receipt->getJobId());
    info("Module Hash: " . substr($receipt->getModuleHash(), 0, 16) . "...");
    info("Claimed signer (display only): " . substr($receipt->getNodeId(), 0, 16) . "...");
    info("Timestamp: " . date('Y-m-d H:i:s', $receipt->getTimestamp()));
} catch (Exception $e) {
    error("Verification failed: " . $e->getMessage());
}

section("Test complete!");
echo "\n";
