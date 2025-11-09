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

use PhaseBased\Plasm\Client;
use PhaseBased\Plasm\Manifest;

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

    if ($receipt->verify()) {
        success("Signature valid");
        success("Module hash matches");
        success("Receipt verified");
    } else {
        error("Receipt verification failed");
    }

    // Display receipt details
    echo "\nReceipt Details:\n";
    info("Job ID: " . $receipt->getJobId());
    info("Module Hash: " . substr($receipt->getModuleHash(), 0, 16) . "...");
    info("Node ID: " . substr($receipt->getNodeId(), 0, 16) . "...");
    info("Timestamp: " . date('Y-m-d H:i:s', $receipt->getTimestamp()));
} catch (Exception $e) {
    error("Verification failed: " . $e->getMessage());
}

section("Test complete!");
echo "\n";
