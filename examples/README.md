# Phase Examples

This directory contains example files for Phase distributed WASM execution.

## Schemas

### manifest.schema.json

JSON Schema definition for job manifests. A manifest describes:
- **version**: Schema version (e.g., "0.1")
- **module_hash**: SHA-256 hash of WASM module (format: `sha256:HEX`)
- **cpu_cores**: CPU cores required (1-64)
- **memory_mb**: Memory limit in MB (1-32768)
- **timeout_seconds**: Max execution time in seconds (1-3600)

**Example**: See `manifest.example.json`

### receipt.schema.json

JSON Schema definition for execution receipts. A receipt proves:
- **version**: Schema version
- **module_hash**: Hash of executed module
- **exit_code**: Exit code (0 = success)
- **wall_time_ms**: Execution time in milliseconds
- **timestamp**: Unix epoch when execution completed
- **node_pubkey**: Ed25519 public key of node (hex, 64 chars)
- **signature**: Ed25519 signature over receipt (hex, 128 chars)

**Example**: See `receipt.example.json`

## WASM Modules

### hello.wasm (coming soon)

Example WASM module that reverses a string. This demonstrates:
- WASI stdio
- String manipulation
- Exit codes

## PHP Examples

### local_test.php (coming soon)

Local execution example using PHP client SDK:
```php
$client = new Plasm\Client(['mode' => 'local']);
$job = $client->createJob('hello.wasm')->submit();
$result = $job->wait();
echo $result->stdout();
```

### remote_test.php (coming soon)

Remote execution example via P2P network:
```php
$client = new Plasm\Client(['mode' => 'remote']);
$job = $client->createJob('hello.wasm')->submit();
$result = $job->wait();
echo $result->stdout();
echo $result->receipt()->verify() ? "✓ Verified" : "✗ Failed";
```

## Validation

Validate manifests and receipts against schemas:

```bash
# Using jq + jsonschema (Python package)
jsonschema -i manifest.example.json manifest.schema.json
jsonschema -i receipt.example.json receipt.schema.json
```

## License

Apache 2.0 © 2025 PhaseBased
