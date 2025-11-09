<?php

namespace Plasm;

/**
 * Cryptographic utilities for Phase (Ed25519 signatures)
 */
class Crypto
{
    /**
     * Verify an Ed25519 signature on a receipt
     *
     * @param Receipt $receipt Receipt to verify
     * @return bool True if signature is valid
     * @throws \RuntimeException if verification fails
     */
    public static function verifyReceipt(Receipt $receipt): bool
    {
        // Get public key from receipt (hex-encoded)
        $pubkeyHex = $receipt->getNodePubkey();
        if (empty($pubkeyHex)) {
            throw new \RuntimeException("Receipt has no public key");
        }

        // Get signature from receipt (hex-encoded)
        $signatureHex = $receipt->getSignature();
        if (empty($signatureHex)) {
            throw new \RuntimeException("Receipt has no signature");
        }

        // Decode hex to binary
        $pubkeyBin = hex2bin($pubkeyHex);
        $signatureBin = hex2bin($signatureHex);

        if ($pubkeyBin === false || strlen($pubkeyBin) !== SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) {
            throw new \RuntimeException("Invalid public key format");
        }

        if ($signatureBin === false || strlen($signatureBin) !== SODIUM_CRYPTO_SIGN_BYTES) {
            throw new \RuntimeException("Invalid signature format");
        }

        // Recreate the canonical message (matches Rust implementation)
        $message = self::getCanonicalMessage($receipt);

        // Hash the message (defense in depth, matches Rust)
        $messageHash = hash('sha256', $message, true);

        // Verify signature
        return sodium_crypto_sign_verify_detached($signatureBin, $messageHash, $pubkeyBin);
    }

    /**
     * Get canonical message from receipt (must match Rust implementation)
     *
     * @param Receipt $receipt
     * @return string
     */
    private static function getCanonicalMessage(Receipt $receipt): string
    {
        // Format: version|module_hash|exit_code|wall_time_ms|timestamp
        return sprintf(
            "%s|%s|%d|%d|%d",
            $receipt->getVersion(),
            $receipt->getModuleHash(),
            $receipt->getExitCode(),
            $receipt->getWallTimeMs(),
            $receipt->getTimestamp()
        );
    }

    /**
     * Verify a hex-encoded signature against a message
     *
     * @param string $message Message that was signed
     * @param string $signatureHex Hex-encoded signature
     * @param string $pubkeyHex Hex-encoded public key
     * @return bool True if signature is valid
     * @throws \RuntimeException if verification fails
     */
    public static function verifySignature(
        string $message,
        string $signatureHex,
        string $pubkeyHex
    ): bool {
        // Decode hex to binary
        $pubkeyBin = hex2bin($pubkeyHex);
        $signatureBin = hex2bin($signatureHex);

        if ($pubkeyBin === false || strlen($pubkeyBin) !== SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) {
            throw new \RuntimeException("Invalid public key format");
        }

        if ($signatureBin === false || strlen($signatureBin) !== SODIUM_CRYPTO_SIGN_BYTES) {
            throw new \RuntimeException("Invalid signature format");
        }

        // Hash the message
        $messageHash = hash('sha256', $message, true);

        // Verify signature
        return sodium_crypto_sign_verify_detached($signatureBin, $messageHash, $pubkeyBin);
    }
}
