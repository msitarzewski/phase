<?php

namespace Plasm;

/**
 * Job manifest describing resource requirements
 */
class Manifest
{
    private string $version = '0.1';
    private string $moduleHash;
    private int $cpuCores = 1;
    private int $memoryMb = 128;
    private int $timeoutSeconds = 300;

    private function __construct(string $moduleHash)
    {
        $this->moduleHash = $moduleHash;
    }

    /**
     * Create manifest from WASM file
     *
     * @param string $wasmPath
     * @return self
     */
    public static function fromWasmFile(string $wasmPath): self
    {
        if (!file_exists($wasmPath)) {
            throw new \InvalidArgumentException("WASM file not found: $wasmPath");
        }

        $wasmBytes = file_get_contents($wasmPath);
        $hash = 'sha256:' . hash('sha256', $wasmBytes);

        return new self($hash);
    }

    /**
     * Create manifest from JSON
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

        $manifest = new self($data['module_hash']);
        $manifest->version = $data['version'] ?? '0.1';
        $manifest->cpuCores = $data['cpu_cores'] ?? 1;
        $manifest->memoryMb = $data['memory_mb'] ?? 128;
        $manifest->timeoutSeconds = $data['timeout_seconds'] ?? 300;

        return $manifest;
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
            'cpu_cores' => $this->cpuCores,
            'memory_mb' => $this->memoryMb,
            'timeout_seconds' => $this->timeoutSeconds,
        ], JSON_PRETTY_PRINT);
    }

    // Getters and setters
    public function getModuleHash(): string { return $this->moduleHash; }
    public function getCpuCores(): int { return $this->cpuCores; }
    public function getMemoryMb(): int { return $this->memoryMb; }
    public function getTimeoutSeconds(): int { return $this->timeoutSeconds; }

    public function setCpuCores(int $cores): void { $this->cpuCores = $cores; }
    public function setMemoryMb(int $mb): void { $this->memoryMb = $mb; }
    public function setTimeoutSeconds(int $seconds): void { $this->timeoutSeconds = $seconds; }
}
