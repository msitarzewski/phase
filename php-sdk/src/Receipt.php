<?php

namespace Plasm;

/**
 * Execution receipt proving work was done.
 *
 * As of phase-core M7 the canonical Phase receipt is
 * `SignedReceipt<JobResult>` with fields:
 *
 *     {
 *       "schema_version": 1,
 *       "result":         { ... JobResult ... },
 *       "job_id":         "hex-32-bytes",
 *       "worker_pubkey":  "hex-32-bytes",
 *       "signature":      "hex-64-bytes",
 *       "completed_at":   "2026-05-27T..."
 *     }
 *
 * The legacy November-2025 shape used by the local-transport mock path is:
 *
 *     {
 *       "version":      "0.1",
 *       "module_hash":  "sha256:...",
 *       "exit_code":    0,
 *       "wall_time_ms": 8,
 *       "timestamp":    1779896995,
 *       "node_pubkey":  "...",
 *       "signature":    "..."
 *     }
 *
 * This class accepts both. `isSignedEnvelope()` tells the caller which
 * shape is in play; the convenience getters expose the union of both shapes.
 */
class Receipt
{
    // ---- M7 SignedReceipt<JobResult> fields ----
    private ?int $schemaVersion = null;
    /** @var mixed Decoded JobResult body (or null for legacy receipts). */
    private $result = null;
    private string $jobId = '';
    private string $workerPubkey = '';
    private string $completedAt = '';

    // ---- legacy daemon-receipt fields (still used by LocalTransport mock) ----
    private string $version = '';
    private string $moduleHash = '';
    private int $exitCode = 0;
    private int $wallTimeMs = 0;
    private int $timestamp = 0;
    private string $nodePubkey = '';

    // ---- shared ----
    private string $signature = '';

    /** True when this receipt was loaded as an M7 SignedReceipt<JobResult>. */
    private bool $signedEnvelope = false;

    private function __construct(array $data)
    {
        // Detect format. The M7 envelope has both `schema_version` and `result`.
        if (array_key_exists('schema_version', $data)
            && array_key_exists('result', $data)) {
            $this->signedEnvelope = true;
            $this->schemaVersion = (int) $data['schema_version'];
            $this->result        = $data['result'];
            $this->jobId         = (string) ($data['job_id'] ?? '');
            $this->workerPubkey  = (string) ($data['worker_pubkey'] ?? '');
            $this->signature     = (string) ($data['signature'] ?? '');
            $this->completedAt   = (string) ($data['completed_at'] ?? '');

            // Surface the most commonly-needed JobResult fields on the receipt
            // so existing example code (`$receipt->getModuleHash()` etc.) keeps
            // working without forcing every caller through `getResult()`.
            $result = is_array($this->result) ? $this->result : [];
            $this->moduleHash = (string) ($result['metrics']['extra']['module_hash'] ?? '');
            $this->exitCode   = (int) ($result['metrics']['extra']['exit_code'] ?? 0);
            $this->wallTimeMs = (int) ($result['metrics']['total_duration_ms'] ?? 0);
        } else {
            // Legacy shape — no envelope.
            $this->signedEnvelope = false;
            $this->version      = (string) ($data['version'] ?? '');
            $this->moduleHash   = (string) ($data['module_hash'] ?? '');
            $this->exitCode     = (int) ($data['exit_code'] ?? 0);
            $this->wallTimeMs   = (int) ($data['wall_time_ms'] ?? 0);
            $this->timestamp    = (int) ($data['timestamp'] ?? 0);
            $this->nodePubkey   = (string) ($data['node_pubkey'] ?? '');
            $this->signature    = (string) ($data['signature'] ?? '');
        }
    }

    /**
     * Create a receipt from a JSON string. Auto-detects M7 vs legacy shape.
     */
    public static function fromJson(string $json): self
    {
        $data = json_decode($json, true);
        if (json_last_error() !== JSON_ERROR_NONE) {
            throw new \InvalidArgumentException(
                'Invalid JSON: ' . json_last_error_msg()
            );
        }
        return new self(is_array($data) ? $data : []);
    }

    /**
     * Create a legacy "mock" receipt for local execution. Used by
     * `LocalTransport` where there's no signed envelope to verify.
     */
    public static function createMock(string $moduleHash, int $exitCode, int $wallTimeMs): self
    {
        return new self([
            'version'      => '0.1',
            'module_hash'  => $moduleHash,
            'exit_code'    => $exitCode,
            'wall_time_ms' => $wallTimeMs,
            'timestamp'    => time(),
            'node_pubkey'  => 'local_execution',
            'signature'    => 'unsigned',
        ]);
    }

    /**
     * Serialize back to JSON in whichever shape this receipt was built from.
     */
    public function toJson(): string
    {
        if ($this->signedEnvelope) {
            return json_encode([
                'schema_version' => $this->schemaVersion,
                'result'         => $this->result,
                'job_id'         => $this->jobId,
                'worker_pubkey'  => $this->workerPubkey,
                'signature'      => $this->signature,
                'completed_at'   => $this->completedAt,
            ], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
        }
        return json_encode([
            'version'      => $this->version,
            'module_hash'  => $this->moduleHash,
            'exit_code'    => $this->exitCode,
            'wall_time_ms' => $this->wallTimeMs,
            'timestamp'    => $this->timestamp,
            'node_pubkey'  => $this->nodePubkey,
            'signature'    => $this->signature,
        ], JSON_PRETTY_PRINT);
    }

    /**
     * Verify the receipt signature.
     *
     * If `$publicKey` is supplied, it is used instead of the key carried in
     * the receipt; this is how a verifier pins to a known worker identity.
     */
    public function verify(?string $publicKey = null): bool
    {
        // Local-execution mock always verifies — there is no real signature.
        if (!$this->signedEnvelope && $this->nodePubkey === 'local_execution') {
            return true;
        }

        if ($publicKey === null) {
            return Crypto::verifyReceipt($this);
        }

        // Caller pinned an explicit public key. Build the canonical message
        // appropriate for this receipt's shape and verify against it.
        $message = Crypto::getCanonicalMessage($this);
        return Crypto::verifySignature($message, $this->signature, $publicKey);
    }

    // ------------------------------------------------------------------
    // Format-disambiguation
    // ------------------------------------------------------------------

    public function isSignedEnvelope(): bool { return $this->signedEnvelope; }
    public function getSchemaVersion(): int { return $this->schemaVersion ?? 0; }
    public function getResult() { return $this->result; }
    public function getCompletedAt(): string { return $this->completedAt; }
    public function getWorkerPubkey(): string { return $this->workerPubkey; }

    // ------------------------------------------------------------------
    // Compatibility getters — work for both shapes where possible
    // ------------------------------------------------------------------

    public function getVersion(): string
    {
        if ($this->signedEnvelope) {
            return 'phase-receipt:v' . ($this->schemaVersion ?? 1);
        }
        return $this->version;
    }

    public function getModuleHash(): string { return $this->moduleHash; }
    public function getExitCode(): int { return $this->exitCode; }
    public function getWallTimeMs(): int { return $this->wallTimeMs; }

    public function getTimestamp(): int
    {
        if ($this->signedEnvelope) {
            // M7 envelope carries an ISO-8601 timestamp; surface a Unix epoch
            // for callers that expect the legacy field shape.
            $t = strtotime($this->completedAt);
            return $t === false ? 0 : $t;
        }
        return $this->timestamp;
    }

    /**
     * The node / worker public key, hex-encoded. Returns `worker_pubkey`
     * for M7 envelopes and `node_pubkey` for legacy receipts.
     */
    public function getNodePubkey(): string
    {
        return $this->signedEnvelope ? $this->workerPubkey : $this->nodePubkey;
    }

    public function getSignature(): string { return $this->signature; }

    public function getJobId(): string
    {
        if ($this->signedEnvelope) {
            return $this->jobId;
        }
        return $this->moduleHash;
    }

    /** Alias retained for the remote_test.php example. */
    public function getNodeId(): string
    {
        return $this->getNodePubkey();
    }

    public function isSuccess(): bool { return $this->exitCode === 0; }
}
