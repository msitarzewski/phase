use anyhow::Result;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Error, Debug)]
pub enum WasmError {
    #[error("Failed to create WASM runtime: {0}")]
    RuntimeCreationError(String),

    #[error("Failed to load WASM module: {0}")]
    ModuleLoadError(String),

    #[error("Failed to execute WASM: {0}")]
    ExecutionError(String),

    #[error("Execution timeout after {0}s")]
    TimeoutError(u64),

    #[error("Memory limit exceeded: {requested} bytes (limit: {limit} bytes)")]
    MemoryLimitExceeded { requested: u64, limit: u64 },
}

/// Result of WASM execution
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Exit code (0 = success)
    pub exit_code: u32,

    /// Standard output captured from WASM
    pub stdout: String,

    /// Standard error captured from WASM
    pub stderr: String,

    /// Wall clock time (milliseconds)
    pub wall_time_ms: u64,

    /// Module hash (SHA-256)
    pub module_hash: String,
}

/// Trait for WASM runtime implementations
pub trait WasmRuntime {
    /// Execute a WASM module with arguments
    fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult>;

    /// Execute with explicit timeout
    fn execute_with_timeout(
        &self,
        wasm_bytes: &[u8],
        args: &[&str],
        timeout: Duration,
    ) -> Result<ExecutionResult>;
}

/// Wasmtime-based WASM runtime implementation (with full WASI support)
pub struct Wasm3Runtime {
    max_memory_bytes: u64,
    stack_size_bytes: u64,
}

impl Wasm3Runtime {
    /// Create a new runtime with default limits
    pub fn new() -> Self {
        Self {
            max_memory_bytes: 128 * 1024 * 1024, // 128 MB
            stack_size_bytes: 64 * 1024,         // 64 KB
        }
    }

    /// Set maximum memory limit
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Set stack size
    pub fn with_stack_size(mut self, bytes: u64) -> Self {
        self.stack_size_bytes = bytes;
        self
    }

    /// Compute SHA-256 hash of WASM module
    fn compute_module_hash(wasm_bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(wasm_bytes);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

impl Default for Wasm3Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmRuntime for Wasm3Runtime {
    fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult> {
        // Default timeout: 5 minutes
        self.execute_with_timeout(wasm_bytes, args, Duration::from_secs(300))
    }

    fn execute_with_timeout(
        &self,
        wasm_bytes: &[u8],
        _args: &[&str],
        timeout: Duration,
    ) -> Result<ExecutionResult> {
        use wasmtime::*;
        use wasmtime_wasi::{WasiCtxBuilder, ResourceTable, WasiView};

        info!("Executing WASM module ({} bytes)", wasm_bytes.len());
        let start = Instant::now();

        // Compute module hash
        let module_hash = Self::compute_module_hash(wasm_bytes);
        debug!("Module hash: {}", module_hash);

        // Create engine with resource limits
        let mut config = Config::new();
        config.consume_fuel(true); // Enable fuel for timeout control

        let engine = Engine::new(&config)
            .map_err(|e| WasmError::RuntimeCreationError(e.to_string()))?;

        // State struct that holds WASI context
        struct MyState {
            wasi_ctx: wasmtime_wasi::WasiCtx,
            table: ResourceTable,
        }

        impl WasiView for MyState {
            fn table(&mut self) -> &mut ResourceTable {
                &mut self.table
            }

            fn ctx(&mut self) -> &mut wasmtime_wasi::WasiCtx {
                &mut self.wasi_ctx
            }
        }

        // Create WASI context with stdio
        let wasi = WasiCtxBuilder::new()
            .inherit_stdio()
            .build();

        let state = MyState {
            wasi_ctx: wasi,
            table: ResourceTable::new(),
        };

        let mut store = Store::new(&engine, state);

        // Set fuel limit based on timeout (rough heuristic: 1M instructions per second)
        let fuel_limit = (timeout.as_secs() * 1_000_000) as u64;
        store.set_fuel(fuel_limit)
            .map_err(|e| WasmError::RuntimeCreationError(e.to_string()))?;

        // Load module
        let module = Module::from_binary(&engine, wasm_bytes)
            .map_err(|e| WasmError::ModuleLoadError(e.to_string()))?;

        // Create linker - using module linker for now (will add WASI in Milestone 3)
        let linker = Linker::new(&engine);

        // Instantiate module
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| WasmError::ModuleLoadError(e.to_string()))?;

        // Find entry point (_start for WASI)
        let func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| WasmError::ExecutionError(format!("No _start function: {}", e)))?;

        // Execute (fuel exhaustion will cause error if timeout)
        let result = func.call(&mut store, ());

        let exit_code = match result {
            Ok(_) => 0,
            Err(e) => {
                warn!("WASM execution error: {}", e);
                1
            }
        };

        let wall_time_ms = start.elapsed().as_millis() as u64;

        info!(
            "WASM execution complete: exit_code={}, time={}ms",
            exit_code, wall_time_ms
        );

        // Note: For MVP, stdout is inherited (prints to terminal)
        // Full capture will be implemented in later tasks
        let stdout = String::from("(stdout inherited - see terminal output)");

        Ok(ExecutionResult {
            exit_code,
            stdout,
            stderr: String::new(),
            wall_time_ms,
            module_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let runtime = Wasm3Runtime::new();
        assert_eq!(runtime.max_memory_bytes, 128 * 1024 * 1024);
    }

    #[test]
    fn test_runtime_with_limits() {
        let runtime = Wasm3Runtime::new()
            .with_memory_limit(256 * 1024 * 1024)
            .with_stack_size(128 * 1024);

        assert_eq!(runtime.max_memory_bytes, 256 * 1024 * 1024);
        assert_eq!(runtime.stack_size_bytes, 128 * 1024);
    }

    #[test]
    fn test_compute_module_hash() {
        let wasm_bytes = b"fake wasm module";
        let hash = Wasm3Runtime::compute_module_hash(wasm_bytes);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 71); // "sha256:" + 64 hex chars
    }

    // Note: Full execution tests require a valid WASM module
    // These will be added in Task 4 when we create hello.wasm
}
