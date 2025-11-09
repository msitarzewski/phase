<?php
require __DIR__ . '/../vendor/autoload.php';

use PhaseBased\Plasm\Client;

$client = new Client();
$jobId = $client->submit(__DIR__ . '/hello.wasm', ['cpu' => 1, 'max_seconds' => 15]);

echo "Job: $jobId\n";
echo "Waiting...\n";

$status = $client->wait($jobId);
$result = $client->result($jobId);
$receipt = $client->receipt($jobId);

echo "Result: " . $result . "\n";
echo "Receipt: " . $receipt . "\n";
