<?php

namespace Plasm;

use Plasm\Transport\LocalTransport;
use Plasm\Transport\TransportInterface;

/**
 * Phase WASM execution client
 */
class Client
{
    private TransportInterface $transport;
    private array $options;

    /**
     * Create a new Plasm client
     *
     * @param array $options Configuration options
     *   - mode: 'local' or 'remote' (default: 'local')
     *   - plasmd_path: Path to plasmd binary (default: 'plasmd')
     */
    public function __construct(array $options = [])
    {
        $this->options = array_merge([
            'mode' => 'local',
            'plasmd_path' => 'plasmd',
        ], $options);

        // Create appropriate transport
        $this->transport = match($this->options['mode']) {
            'local' => new LocalTransport($this->options['plasmd_path']),
            'remote' => throw new \RuntimeException('Remote transport not yet implemented (Milestone 2+)'),
            default => throw new \InvalidArgumentException("Invalid mode: {$this->options['mode']}"),
        };
    }

    /**
     * Create a new job
     *
     * @param string $wasmPath Path to WASM file
     * @return Job
     */
    public function createJob(string $wasmPath): Job
    {
        return new Job($this->transport, $wasmPath);
    }

    /**
     * Get transport (for testing)
     *
     * @return TransportInterface
     */
    public function getTransport(): TransportInterface
    {
        return $this->transport;
    }
}
