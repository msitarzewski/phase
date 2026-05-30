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
 * `crates/phase-receipt/src/canonical.rs`.
 *
 * SEC-03 / audit C3: the legacy pipe-separated SHA-256 format that earlier
 * versions accepted as a fallback has been REMOVED from the trust path. It
 * was a downgrade vector — an attacker could omit `schema_version`/`result`
 * to force the weak path and forge any field. Only the `phase-receipt:v1:`
 * Ed25519-over-canonical-JSON path is trusted now.
 */
class Crypto
{
    /** Domain-separation prefix included in every signed-bytes message. */
    public const SIGNING_DOMAIN = 'phase-receipt:v1:';

    /**
     * Highest envelope schema version this SDK understands. Mirrors
     * `phase-receipt::SCHEMA_VERSION` on the Rust side.
     */
    public const SCHEMA_VERSION = 1;

    /**
     * Build the canonical signing message for an M7 signed receipt.
     *
     * The message is:
     *     "phase-receipt:v1:" || canonical_json(SigningEnvelope)
     * where SigningEnvelope has fields {completed_at, job_id, result, schema_version}.
     *
     * There is no legacy fallback: callers must only invoke this for a v1
     * signed envelope (`Receipt::isSignedEnvelope()` true).
     *
     * @throws \RuntimeException if the receipt is not a v1 signed envelope.
     */
    public static function getCanonicalMessage(Receipt $receipt): string
    {
        if (!$receipt->isSignedEnvelope()) {
            throw new \RuntimeException(
                'Refusing to build a signing message for a non-v1 receipt: '
                . 'legacy receipts are not trusted (SEC-03).'
            );
        }

        $envelope = [
            'completed_at'   => $receipt->getCompletedAt(),
            'job_id'         => $receipt->getJobId(),
            'result'         => $receipt->getResult(),
            'schema_version' => $receipt->getSchemaVersion(),
        ];
        return self::SIGNING_DOMAIN . self::canonicalJsonEncode($envelope);
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
