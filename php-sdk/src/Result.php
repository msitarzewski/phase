<?php

namespace Plasm;

/**
 * Execution result containing output and receipt
 */
class Result
{
    private string $stdout;
    private string $stderr;
    private Receipt $receipt;

    public function __construct(string $stdout, string $stderr, Receipt $receipt)
    {
        $this->stdout = $stdout;
        $this->stderr = $stderr;
        $this->receipt = $receipt;
    }

    /**
     * Get stdout output
     *
     * @return string
     */
    public function stdout(): string
    {
        return $this->stdout;
    }

    /**
     * Get stderr output
     *
     * @return string
     */
    public function stderr(): string
    {
        return $this->stderr;
    }

    /**
     * Get exit code
     *
     * @return int
     */
    public function exitCode(): int
    {
        return $this->receipt->getExitCode();
    }

    /**
     * Get execution receipt
     *
     * @return Receipt
     */
    public function receipt(): Receipt
    {
        return $this->receipt;
    }

    /**
     * Check if execution was successful
     *
     * @return bool
     */
    public function isSuccess(): bool
    {
        return $this->receipt->isSuccess();
    }
}
