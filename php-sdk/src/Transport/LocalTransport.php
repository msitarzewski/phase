<?php

namespace Plasm\Transport;

use Plasm\Manifest;
use Plasm\Receipt;
use Plasm\Result;

/**
 * Local transport executing WASM via plasmd CLI
 */
class LocalTransport implements TransportInterface
{
    private string $plasmdPath;

    public function __construct(string $plasmdPath = 'plasmd')
    {
        $this->plasmdPath = $plasmdPath;
    }

    /**
     * Execute a WASM module locally
     *
     * @param string $wasmPath Path to WASM file
     * @param Manifest $manifest Job manifest
     * @param string|null $input Optional stdin input
     * @return Result
     */
    public function execute(string $wasmPath, Manifest $manifest, ?string $input = null): Result
    {
        $startTime = microtime(true);

        // Build plasmd command with --quiet flag to suppress logs
        $cmd = escapeshellarg($this->plasmdPath) . ' run --quiet ' . escapeshellarg($wasmPath) . ' 2>&1';

        // Execute with input piped to stdin
        $descriptors = [
            0 => ['pipe', 'r'], // stdin
            1 => ['pipe', 'w'], // stdout
            2 => ['pipe', 'w'], // stderr
        ];

        $process = proc_open($cmd, $descriptors, $pipes);

        if (!is_resource($process)) {
            throw new \RuntimeException("Failed to start plasmd process");
        }

        // Write input to stdin
        if ($input !== null) {
            fwrite($pipes[0], $input);
        }
        fclose($pipes[0]);

        // Read stdout and stderr
        $stdout = stream_get_contents($pipes[1]);
        $stderr = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);

        // Get exit code
        $exitCode = proc_close($process);

        // Calculate wall time
        $wallTimeMs = (int)((microtime(true) - $startTime) * 1000);

        // Create receipt
        $receipt = Receipt::createMock(
            $manifest->getModuleHash(),
            $exitCode,
            $wallTimeMs
        );

        // Extract actual output from plasmd logs
        // plasmd outputs logs to stderr, actual WASM stdout to stdout
        $actualStdout = $this->extractWasmOutput($stdout);

        return new Result($actualStdout, $stderr, $receipt);
    }

    /**
     * Extract WASM output from plasmd output
     *
     * WASM output appears inline before timestamp logs (wasmtime inherits stdio)
     *
     * @param string $output
     * @return string
     */
    private function extractWasmOutput(string $output): string
    {
        // Remove ANSI escape codes
        $output = preg_replace('/\x1b\[[0-9;]*m/', '', $output);

        // Extract content that appears BEFORE timestamp patterns on each line
        // Example: "dlroW ,olleH2025-11-09T04:50:35..." -> "dlroW ,olleH"
        $lines = explode("\n", $output);
        $wasmLines = [];

        foreach ($lines as $line) {
            // Skip lines that are pure logs (start with timestamp)
            if (preg_match('/^\d{4}-\d{2}-\d{2}T/', $line)) {
                continue;
            }

            // Extract content before any embedded timestamp
            if (preg_match('/^(.*?)\d{4}-\d{2}-\d{2}T/', $line, $matches)) {
                $content = trim($matches[1]);
                if ($content !== '') {
                    $wasmLines[] = $content;
                }
            } else {
                // No timestamp on this line - might be pure WASM output
                $trimmed = trim($line);
                if ($trimmed !== '') {
                    $wasmLines[] = $trimmed;
                }
            }
        }

        return implode("\n", $wasmLines);
    }
}
