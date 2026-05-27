<?php

namespace Plasm;

/**
 * Cryptographic utilities for Phase (Ed25519 signatures).
 *
 * As of phase-core M7 the canonical signing format is:
 *
 *     b"phase-receipt:v1:" || canonical_json({
 *         "completed_at": ...,
 *         "job_id":       ...,
 *         "result":       ...,
 *         "schema_version": 1
 *     })
 *
 * Canonical JSON = serde_json's value re-serialized with object keys sorted
 * lexicographically at every nesting level, no whitespace, no trailing
 * commas. This matches the Rust algorithm in
 * `crates/phase-receipt/src/canonical.rs`. The legacy pipe-separated format
 * `version|module_hash|exit_code|wall_time_ms|timestamp` is retained as a
 * fallback for receipts that arrive in the November 2025 wire shape.
 */
class Crypto
{
    /** Domain-separation prefix included in every signed-bytes message. */
    public const SIGNING_DOMAIN = 'phase-receipt:v1:';

    /**
     * Verify a Phase receipt.
     *
     * Dispatches to whichever signing-format the receipt is carrying:
     * - If `Receipt::isSignedEnvelope()` returns true (M7 SignedReceipt<JobResult>)
     *   we verify against the canonical-JSON signing message.
     * - Otherwise we fall back to the legacy pipe-separated message, which
     *   keeps unit fixtures + legacy plasmd receipts verifiable.
     *
     * @throws \RuntimeException if verification fails
     */
    public static function verifyReceipt(Receipt $receipt): bool
    {
        $pubkeyHex = $receipt->getWorkerPubkey() ?: $receipt->getNodePubkey();
        $signatureHex = $receipt->getSignature();

        if (empty($pubkeyHex)) {
            throw new \RuntimeException('Receipt has no public key');
        }
        if (empty($signatureHex)) {
            throw new \RuntimeException('Receipt has no signature');
        }

        $pubkeyBin = hex2bin($pubkeyHex);
        $signatureBin = hex2bin($signatureHex);

        if ($pubkeyBin === false
            || strlen($pubkeyBin) !== SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) {
            throw new \RuntimeException('Invalid public key format');
        }
        if ($signatureBin === false
            || strlen($signatureBin) !== SODIUM_CRYPTO_SIGN_BYTES) {
            throw new \RuntimeException('Invalid signature format');
        }

        $message = self::getCanonicalMessage($receipt);

        return sodium_crypto_sign_verify_detached($signatureBin, $message, $pubkeyBin);
    }

    /**
     * Build the canonical signing message for a receipt.
     *
     * For M7 SignedReceipt<JobResult> envelopes this is:
     *     "phase-receipt:v1:" || canonical_json(SigningEnvelope)
     * where SigningEnvelope has fields {completed_at, job_id, result, schema_version}.
     *
     * For legacy receipts this falls back to:
     *     SHA256(version|module_hash|exit_code|wall_time_ms|timestamp)
     * to preserve compatibility with the November 2025 daemon receipts.
     */
    public static function getCanonicalMessage(Receipt $receipt): string
    {
        if ($receipt->isSignedEnvelope()) {
            $envelope = [
                'completed_at'   => $receipt->getCompletedAt(),
                'job_id'         => $receipt->getJobId(),
                'result'         => $receipt->getResult(),
                'schema_version' => $receipt->getSchemaVersion(),
            ];
            return self::SIGNING_DOMAIN . self::canonicalJsonEncode($envelope);
        }

        // Legacy path: SHA-256 over pipe-separated fields.
        $message = sprintf(
            '%s|%s|%d|%d|%d',
            $receipt->getVersion(),
            $receipt->getModuleHash(),
            $receipt->getExitCode(),
            $receipt->getWallTimeMs(),
            $receipt->getTimestamp()
        );
        return hash('sha256', $message, true);
    }

    /**
     * Encode a value as canonical JSON: object keys sorted lexicographically
     * at every nesting level, no whitespace, slashes / unicode unescaped.
     * Mirrors `serde_json` with sorted keys as used by phase-receipt /
     * phase-manifest.
     *
     * @param mixed $value
     */
    public static function canonicalJsonEncode($value): string
    {
        $sorted = self::sortValue($value);
        $encoded = json_encode(
            $sorted,
            JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE
        );
        if ($encoded === false) {
            throw new \RuntimeException(
                'Canonical JSON encoding failed: ' . json_last_error_msg()
            );
        }
        return $encoded;
    }

    /**
     * Recursive key-sort for associative arrays / objects. Numerically
     * indexed arrays preserve their order (they are sequences, not objects).
     *
     * @param mixed $value
     * @return mixed
     */
    private static function sortValue($value)
    {
        if (is_object($value)) {
            $value = (array) $value;
        }
        if (!is_array($value)) {
            return $value;
        }
        if (self::isList($value)) {
            return array_map([self::class, 'sortValue'], $value);
        }
        ksort($value, SORT_STRING);
        $out = [];
        foreach ($value as $k => $v) {
            $out[$k] = self::sortValue($v);
        }
        // json_encode on an associative array emits an object. Wrap in
        // (object) so empty associatives encode as `{}` rather than `[]`.
        return (object) $out;
    }

    /**
     * PHP 8.1+ has `array_is_list`; we ship a polyfill so the SDK runs on
     * 7.4+ which the existing examples target.
     *
     * @param array $arr
     */
    private static function isList(array $arr): bool
    {
        if (function_exists('array_is_list')) {
            return array_is_list($arr);
        }
        $i = 0;
        foreach ($arr as $k => $_) {
            if ($k !== $i) {
                return false;
            }
            $i++;
        }
        return true;
    }

    /**
     * Verify a hex-encoded signature against an arbitrary message. Caller is
     * responsible for hashing / prefixing as required by the signing format.
     */
    public static function verifySignature(
        string $message,
        string $signatureHex,
        string $pubkeyHex
    ): bool {
        $pubkeyBin = hex2bin($pubkeyHex);
        $signatureBin = hex2bin($signatureHex);

        if ($pubkeyBin === false
            || strlen($pubkeyBin) !== SODIUM_CRYPTO_SIGN_PUBLICKEYBYTES) {
            throw new \RuntimeException('Invalid public key format');
        }
        if ($signatureBin === false
            || strlen($signatureBin) !== SODIUM_CRYPTO_SIGN_BYTES) {
            throw new \RuntimeException('Invalid signature format');
        }

        return sodium_crypto_sign_verify_detached(
            $signatureBin,
            $message,
            $pubkeyBin
        );
    }
}
