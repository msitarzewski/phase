<?php

namespace Plasm;

use Plasm\Transport\TransportInterface;

/**
 * Represents a WASM execution job
 */
class Job
{
    private TransportInterface $transport;
    private string $wasmPath;
    private Manifest $manifest;

    public function __construct(TransportInterface $transport, string $wasmPath)
    {
        $this->transport = $transport;
        $this->wasmPath = $wasmPath;

        // Create manifest from WASM file
        $this->manifest = Manifest::fromWasmFile($wasmPath);
    }

    /**
     * Set CPU cores requirement
     *
     * @param int $cores
     * @return self
     */
    public function withCpu(int $cores): self
    {
        $this->manifest->setCpuCores($cores);
        return $this;
    }

    /**
     * Set memory limit (MB)
     *
     * @param int $memoryMb
     * @return self
     */
    public function withMemory(int $memoryMb): self
    {
        $this->manifest->setMemoryMb($memoryMb);
        return $this;
    }

    /**
     * Set timeout (seconds)
     *
     * @param int $timeoutSeconds
     * @return self
     */
    public function withTimeout(int $timeoutSeconds): self
    {
        $this->manifest->setTimeoutSeconds($timeoutSeconds);
        return $this;
    }

    /**
     * Submit job for execution
     *
     * @param string|null $input Optional stdin input
     * @return Result
     */
    public function submit(?string $input = null): Result
    {
        return $this->transport->execute($this->wasmPath, $this->manifest, $input);
    }

    /**
     * Submit job and wait for result
     *
     * @param string|null $input Optional stdin input
     * @return Result
     */
    public function wait(?string $input = null): Result
    {
        return $this->submit($input);
    }
}
