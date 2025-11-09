# Project Rules: Phase Coding Standards

**Last Updated**: 2025-11-08
**Version**: 0.1
**Status**: Initial Standards

---

## Table of Contents

1. [General Principles](#general-principles)
2. [Rust Coding Standards](#rust-coding-standards)
3. [PHP Coding Standards](#php-coding-standards)
4. [Error Handling](#error-handling)
5. [Testing Standards](#testing-standards)
6. [Security Standards](#security-standards)
7. [Documentation Standards](#documentation-standards)
8. [Git & Version Control](#git--version-control)
9. [Code Review Checklist](#code-review-checklist)

---

## General Principles

### The Three Laws

1. **Security First**: No shortcuts on security. Sandboxing, encryption, verification are non-negotiable.
2. **Simplicity Over Cleverness**: Clear code > clever code. Optimize only when measured.
3. **Fail Loudly**: Errors should be obvious and actionable. No silent failures.

### Code Organization

**Pattern**: Group by feature, not by type

```
✅ GOOD:
daemon/
├── wasm/          # WASM execution feature
│   ├── runtime.rs
│   ├── manifest.rs
│   └── receipt.rs
├── network/       # Networking feature
│   ├── discovery.rs
│   ├── transport.rs
│   └── handshake.rs

❌ BAD:
daemon/
├── models/        # All models together
├── services/      # All services together
└── utils/         # Grab bag
```

**Rationale**: Features change together; organize for cohesion.

---

## Rust Coding Standards

### Naming Conventions

```rust
// Types: PascalCase
struct JobManifest { }
enum ExecutionError { }

// Functions/methods: snake_case
fn execute_wasm() { }
fn verify_signature() { }

// Constants: SCREAMING_SNAKE_CASE
const MAX_MEMORY_MB: u64 = 512;
const DEFAULT_TIMEOUT_SEC: u64 = 30;

// Modules: snake_case
mod wasm_runtime;
mod peer_discovery;
```

### Result-Based Error Handling

**Rule**: No `unwrap()` or `expect()` in production code

```rust
// ✅ GOOD: Propagate errors with ?
fn load_wasm(path: &Path) -> Result<Vec<u8>, ExecutionError> {
    let bytes = fs::read(path)
        .map_err(|e| ExecutionError::ModuleLoadFailed {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    Ok(bytes)
}

// ❌ BAD: unwrap() in production
fn load_wasm(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()  // NEVER in production
}

// ✅ OK: unwrap() in tests only
#[test]
fn test_load_wasm() {
    let bytes = load_wasm(&test_path).unwrap();  // OK in tests
    assert!(!bytes.is_empty());
}
```

### Async/Await Patterns

**Rule**: Use async/await consistently; don't mix blocking and async

```rust
// ✅ GOOD: Fully async
async fn execute_remote_job(job: Job) -> Result<Receipt, Error> {
    let peer = discover_peer(&job.capability).await?;
    let stream = connect_to_peer(peer).await?;
    let result = send_job(stream, job).await?;
    Ok(result)
}

// ❌ BAD: Blocking in async
async fn execute_remote_job(job: Job) -> Result<Receipt, Error> {
    let peer = discover_peer(&job.capability).await?;
    let data = fs::read(&job.wasm_path)?;  // BLOCKING!
    // ...
}

// ✅ GOOD: Use spawn_blocking for CPU/IO work
async fn execute_remote_job(job: Job) -> Result<Receipt, Error> {
    let peer = discover_peer(&job.capability).await?;
    let data = tokio::task::spawn_blocking(|| {
        fs::read(&job.wasm_path)
    }).await??;
    // ...
}
```

### Type-Driven Design

**Rule**: Use the type system to prevent bugs

```rust
// ✅ GOOD: Newtype pattern for validation
#[derive(Debug, Clone)]
pub struct ModuleHash(String);

impl ModuleHash {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let hash = sha256(bytes);
        Self(format!("sha256:{}", hex::encode(hash)))
    }

    pub fn verify(&self, bytes: &[u8]) -> bool {
        self == &Self::from_bytes(bytes)
    }
}

// ❌ BAD: Stringly-typed
fn verify_hash(hash: &str, bytes: &[u8]) -> bool {
    let computed = sha256(bytes);
    hash == &format!("sha256:{}", hex::encode(computed))
}
```

### Linting

**Rule**: Zero clippy warnings before merge

```bash
cargo clippy -- -D warnings
```

**Allowed Lints** (use sparingly with justification):
```rust
#![allow(clippy::too_many_arguments)]  // OK if justified
```

---

## PHP Coding Standards

### PSR Compliance

**Rule**: Follow PSR-12 coding style

```php
<?php
// ✅ GOOD: PSR-12 compliant
namespace Plasm\Client;

class JobManifest
{
    private int $cpuCores;
    private int $memoryMb;

    public function __construct(int $cpuCores, int $memoryMb)
    {
        $this->cpuCores = $cpuCores;
        $this->memoryMb = $memoryMb;
    }

    public function toArray(): array
    {
        return [
            'cpu_cores' => $this->cpuCores,
            'memory_mb' => $this->memoryMb,
        ];
    }
}
```

### Type Declarations

**Rule**: Always use strict types and type hints (PHP 8.1+)

```php
<?php
declare(strict_types=1);

// ✅ GOOD: Full type declarations
function submitJob(string $wasmPath, array $options): string
{
    // ...
}

// ❌ BAD: Missing types
function submitJob($wasmPath, $options)
{
    // ...
}
```

### Error Handling

**Rule**: Use exceptions for errors, not return codes

```php
// ✅ GOOD: Exception-based
class JobSubmissionException extends \RuntimeException {}

function submitJob(string $wasmPath): string
{
    if (!file_exists($wasmPath)) {
        throw new JobSubmissionException("WASM file not found: $wasmPath");
    }
    // ...
}

// ❌ BAD: Return codes
function submitJob(string $wasmPath): string|false
{
    if (!file_exists($wasmPath)) {
        return false;  // What went wrong?
    }
    // ...
}
```

---

## Error Handling

### Error Types (Rust)

**Pattern**: Use `thiserror` for error types

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Failed to load module '{path}': {reason}")]
    ModuleLoadFailed { path: String, reason: String },

    #[error("Resource limit exceeded: {resource} ({actual} > {limit})")]
    ResourceLimitExceeded {
        resource: String,
        limit: u64,
        actual: u64,
    },

    #[error("Execution timed out after {timeout_sec}s")]
    TimeoutExceeded { timeout_sec: u64 },

    #[error("Runtime error: {0}")]
    RuntimeError(String),
}
```

### Error Context

**Rule**: Provide actionable context in errors

```rust
// ✅ GOOD: Rich context
Err(ExecutionError::ModuleLoadFailed {
    path: wasm_path.display().to_string(),
    reason: "Permission denied".to_string(),
})

// ❌ BAD: Vague error
Err(ExecutionError::Failed)
```

### Logging

**Rule**: Log at integration boundaries, not internal logic

```rust
// ✅ GOOD: Log at boundaries
pub async fn execute_job(job: Job) -> Result<Receipt, Error> {
    tracing::info!("Executing job: {}", job.id);
    let result = internal_execute(job).await?;
    tracing::info!("Job {} completed in {}ms", job.id, result.wall_time_ms);
    Ok(result)
}

fn internal_execute(job: Job) -> Result<Receipt, Error> {
    // No logging here - internal detail
    let runtime = create_runtime(&job.manifest)?;
    runtime.execute(&job.wasm_bytes)
}

// ❌ BAD: Logging everywhere
fn internal_execute(job: Job) -> Result<Receipt, Error> {
    tracing::debug!("Creating runtime");  // Too noisy
    let runtime = create_runtime(&job.manifest)?;
    tracing::debug!("Executing WASM");    // Too noisy
    runtime.execute(&job.wasm_bytes)
}
```

**Log Levels**:
- `error!`: Unrecoverable errors (job failed, node crashed)
- `warn!`: Recoverable issues (peer unreachable, retrying)
- `info!`: Significant events (job started, peer discovered)
- `debug!`: Detailed diagnostics (only in dev builds)
- `trace!`: Very verbose (network packets, state changes)

---

## Testing Standards

### Test Organization

**Pattern**: Tests live alongside code

```
daemon/src/wasm/
├── runtime.rs
├── runtime_test.rs    # Unit tests
├── manifest.rs
└── manifest_test.rs
```

Or use inline tests:
```rust
// runtime.rs
pub struct Runtime { }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() { }
}
```

### Test Naming

**Rule**: `test_<scenario>_<expected_outcome>`

```rust
#[test]
fn test_execute_hello_wasm_returns_stdout() { }

#[test]
fn test_execute_with_memory_limit_exceeded_returns_error() { }

#[test]
fn test_verify_receipt_with_valid_signature_returns_true() { }
```

### Test Coverage

**Rule**: Minimum 80% coverage for new code

```bash
cargo tarpaulin --out Html
# Open tarpaulin-report.html
```

### Test Fixtures

**Pattern**: Use dedicated fixtures/ directory

```
tests/
├── fixtures/
│   ├── hello.wasm
│   ├── memory_hog.wasm
│   └── invalid.wasm
└── integration_test.rs
```

```rust
#[test]
fn test_load_wasm() {
    let wasm_bytes = include_bytes!("../tests/fixtures/hello.wasm");
    let runtime = Runtime::new().build().unwrap();
    let result = runtime.execute(wasm_bytes, &[]).unwrap();
    assert_eq!(result.exit_code, 0);
}
```

---

## Security Standards

### Input Validation

**Rule**: Validate all external input

```rust
// ✅ GOOD: Validate manifest
impl JobManifest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.memory_mb == 0 || self.memory_mb > MAX_MEMORY_MB {
            return Err(ValidationError::InvalidMemory(self.memory_mb));
        }
        if self.timeout_seconds == 0 || self.timeout_seconds > MAX_TIMEOUT_SEC {
            return Err(ValidationError::InvalidTimeout(self.timeout_seconds));
        }
        Ok(())
    }
}

// Execute
let manifest = JobManifest::from_json(&json)?;
manifest.validate()?;  // Always validate before use
```

### Cryptography

**Rule**: Use well-known libraries, never roll your own crypto

```rust
// ✅ GOOD: Use ed25519-dalek
use ed25519_dalek::{Keypair, Signature, Signer, Verifier};

fn sign_receipt(receipt: &Receipt, keypair: &Keypair) -> Signature {
    let message = receipt.to_bytes();
    keypair.sign(&message)
}

fn verify_receipt(receipt: &Receipt, signature: &Signature, public_key: &PublicKey) -> bool {
    let message = receipt.to_bytes();
    public_key.verify(&message, signature).is_ok()
}

// ❌ BAD: Custom crypto
fn sign_receipt(receipt: &Receipt, key: &[u8]) -> Vec<u8> {
    // Custom signing algorithm - NEVER DO THIS
}
```

### Secrets Management

**Rule**: Never commit secrets; use environment variables

```rust
// ✅ GOOD: Environment variable
let node_key = env::var("PLASM_NODE_KEY")
    .expect("PLASM_NODE_KEY must be set");

// ❌ BAD: Hardcoded
let node_key = "abc123...";  // NEVER
```

---

## Documentation Standards

### Code Comments

**Rule**: Comment *why*, not *what*

```rust
// ✅ GOOD: Explains rationale
// Use wasm3 for MVP due to fast startup (<1ms).
// Will migrate to wasmtime for production performance.
let runtime = Wasm3Runtime::new();

// ❌ BAD: Restates code
// Create a new runtime
let runtime = Wasm3Runtime::new();
```

### Function Documentation

**Rule**: Document public APIs with rustdoc/PHPDoc

```rust
/// Executes a WASM module with the given arguments.
///
/// # Arguments
/// * `wasm_bytes` - The compiled WASM module
/// * `args` - Command-line arguments passed to the module
///
/// # Returns
/// * `Ok(ExecutionResult)` - Execution succeeded
/// * `Err(ExecutionError)` - Module load failed, timeout, or resource limit exceeded
///
/// # Example
/// ```
/// let runtime = Runtime::new().build()?;
/// let result = runtime.execute(&wasm_bytes, &["arg1", "arg2"])?;
/// println!("Exit code: {}", result.exit_code);
/// ```
pub fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult, ExecutionError> {
    // ...
}
```

### README Files

**Rule**: Every module/package has a README with:
- Purpose (one sentence)
- Quick start (code example)
- Configuration (if applicable)
- Links to detailed docs

---

## Git & Version Control

### Commit Messages

**Format**: `<type>: <summary>` (50 chars max)

**Types**:
- `feat`: New feature
- `fix`: Bug fix
- `refactor`: Code restructuring
- `test`: Add/update tests
- `docs`: Documentation
- `chore`: Build/tooling

**Examples**:
```
feat: implement wasm3 runtime with stdout capture
fix: validate manifest memory limit before execution
refactor: extract receipt signing into separate module
test: add integration test for peer discovery
docs: update README with installation instructions
chore: configure cargo-deb for Debian packaging
```

### Branch Naming

**Pattern**: `<type>/<short-description>`

```
feature/wasm-execution
fix/memory-limit-validation
refactor/error-handling
docs/api-documentation
```

### PR Requirements

**Checklist** (before merge):
- [ ] Tests pass (`cargo test`)
- [ ] Linter clean (`cargo clippy`)
- [ ] Code formatted (`cargo fmt`)
- [ ] Documentation updated
- [ ] CHANGELOG updated (if user-facing)
- [ ] Security review (if touching auth/crypto/sandbox)

---

## Code Review Checklist

### Security
- [ ] All external input validated
- [ ] No hardcoded secrets or credentials
- [ ] WASM sandbox enforced (no host access)
- [ ] Cryptographic operations use standard libraries
- [ ] Error messages don't leak sensitive data

### Correctness
- [ ] Error handling covers all edge cases
- [ ] No `unwrap()` or `expect()` in production code
- [ ] Resource limits enforced (memory, CPU, timeout)
- [ ] Tests cover happy path and error cases

### Performance
- [ ] No blocking I/O in async code
- [ ] No unnecessary allocations in hot paths
- [ ] Resource cleanup (files, sockets, memory)

### Maintainability
- [ ] Code is self-explanatory (minimal comments needed)
- [ ] Functions are single-purpose (<50 lines)
- [ ] Module boundaries are clear
- [ ] Dependencies are justified

### Documentation
- [ ] Public APIs documented
- [ ] Complex logic explained (why, not what)
- [ ] Examples provided for non-obvious usage

---

## Anti-Patterns to Avoid

### ❌ Defensive Programming

**Bad**: Adding unnecessary checks for "just in case"
```rust
// ❌ BAD: Defensive check
fn execute(runtime: &Runtime) -> Result<Receipt, Error> {
    if runtime.is_none() {  // Runtime is &, can't be None
        return Err(Error::RuntimeNotInitialized);
    }
    // ...
}
```

**Good**: Fix root cause, use type system
```rust
// ✅ GOOD: Type system prevents None
fn execute(runtime: &Runtime) -> Result<Receipt, Error> {
    // Runtime is always valid reference
    runtime.run()
}
```

### ❌ Error Suppression

**Bad**: Ignoring errors silently
```rust
// ❌ BAD: Silent failure
let _ = fs::remove_file(&temp_file);  // Ignore error
```

**Good**: Handle or log errors
```rust
// ✅ GOOD: Log non-critical error
if let Err(e) = fs::remove_file(&temp_file) {
    tracing::warn!("Failed to cleanup temp file: {}", e);
}
```

### ❌ God Objects

**Bad**: Single struct/module does everything
```rust
// ❌ BAD: God object
struct Plasm {
    fn execute_wasm() { }
    fn discover_peers() { }
    fn send_job() { }
    fn verify_receipt() { }
    // ... 50 more methods
}
```

**Good**: Split by responsibility
```rust
// ✅ GOOD: Focused modules
struct WasmRuntime { fn execute() { } }
struct PeerDiscovery { fn discover() { } }
struct JobTransport { fn send() { } }
struct ReceiptVerifier { fn verify() { } }
```

---

## Pattern Evolution

As patterns emerge from code reviews, add them here with examples and rationale.

**Format**:
```markdown
### Pattern Name
**Context**: When does this apply?
**Example**: Concrete code
**Rationale**: Why this way?
```

---

**These rules are living documents. Update as patterns emerge.**
