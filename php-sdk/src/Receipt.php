<?php

namespace Plasm;

/**
 * Execution receipt proving work was done
 */
class Receipt
{
    private string $version;
    private string $moduleHash;
    private int $exitCode;
    private int $wallTimeMs;
    private int $timestamp;
    private string $nodePubkey;
    private string $signature;

    private function __construct(array $data)
    {
        $this->version = $data['version'];
        $this->moduleHash = $data['module_hash'];
        $this->exitCode = $data['exit_code'];
        $this->wallTimeMs = $data['wall_time_ms'];
        $this->timestamp = $data['timestamp'];
        $this->nodePubkey = $data['node_pubkey'] ?? '';
        $this->signature = $data['signature'] ?? '';
    }

    /**
     * Create receipt from JSON
     *
     * @param string $json
     * @return self
     */
    public static function fromJson(string $json): self
    {
        $data = json_decode($json, true);
        if (json_last_error() !== JSON_ERROR_NONE) {
            throw new \InvalidArgumentException('Invalid JSON: ' . json_last_error_msg());
        }

        return new self($data);
    }

    /**
     * Create a mock receipt (for local execution without signing)
     *
     * @param string $moduleHash
     * @param int $exitCode
     * @param int $wallTimeMs
     * @return self
     */
    public static function createMock(string $moduleHash, int $exitCode, int $wallTimeMs): self
    {
        return new self([
            'version' => '0.1',
            'module_hash' => $moduleHash,
            'exit_code' => $exitCode,
            'wall_time_ms' => $wallTimeMs,
            'timestamp' => time(),
            'node_pubkey' => 'local_execution',
            'signature' => 'unsigned',
        ]);
    }

    /**
     * Convert to JSON
     *
     * @return string
     */
    public function toJson(): string
    {
        return json_encode([
            'version' => $this->version,
            'module_hash' => $this->moduleHash,
            'exit_code' => $this->exitCode,
            'wall_time_ms' => $this->wallTimeMs,
            'timestamp' => $this->timestamp,
            'node_pubkey' => $this->nodePubkey,
            'signature' => $this->signature,
        ], JSON_PRETTY_PRINT);
    }

    /**
     * Verify the receipt signature
     *
     * @param string|null $publicKey Optional public key (hex-encoded)
     * @return bool
     */
    public function verify(?string $publicKey = null): bool
    {
        // For local execution, always return true
        if ($this->nodePubkey === 'local_execution') {
            return true;
        }

        // Use provided public key or the one from receipt
        if ($publicKey === null) {
            return Crypto::verifyReceipt($this);
        }

        // Verify with provided public key
        $message = sprintf(
            "%s|%s|%d|%d|%d",
            $this->version,
            $this->moduleHash,
            $this->exitCode,
            $this->wallTimeMs,
            $this->timestamp
        );

        return Crypto::verifySignature($message, $this->signature, $publicKey);
    }

    // Getters
    public function getVersion(): string { return $this->version; }
    public function getModuleHash(): string { return $this->moduleHash; }
    public function getExitCode(): int { return $this->exitCode; }
    public function getWallTimeMs(): int { return $this->wallTimeMs; }
    public function getTimestamp(): int { return $this->timestamp; }
    public function getNodePubkey(): string { return $this->nodePubkey; }
    public function getSignature(): string { return $this->signature; }
    public function isSuccess(): bool { return $this->exitCode === 0; }
}
