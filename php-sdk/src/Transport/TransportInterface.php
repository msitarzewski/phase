<?php

namespace Plasm\Transport;

use Plasm\Manifest;
use Plasm\Result;

/**
 * Transport interface for WASM execution
 */
interface TransportInterface
{
    /**
     * Execute a WASM module
     *
     * @param string $wasmPath Path to WASM file
     * @param Manifest $manifest Job manifest
     * @param string|null $input Optional stdin input
     * @return Result
     */
    public function execute(string $wasmPath, Manifest $manifest, ?string $input = null): Result;
}
