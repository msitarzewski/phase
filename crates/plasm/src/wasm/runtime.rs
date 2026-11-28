use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Bounded number of compiled Wasmtime modules retained in memory to avoid
/// repeating Cranelift JIT compilation on hot WASM workloads while keeping
/// cache memory bounded.
const MAX_CACHED_WASM_MODULES: usize = 64;

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

struct WasmStoreState {
    wasi: wasmtime_wasi::preview1::WasiP1Ctx,
    limits: wasmtime::StoreLimits,
}

struct CompiledModuleCache {
    entries: HashMap<[u8; 32], wasmtime::Module>,
    order: VecDeque<[u8; 32]>,
}

impl CompiledModuleCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, digest: &[u8; 32]) -> Option<wasmtime::Module> {
        self.entries.get(digest).cloned()
    }

    fn insert(&mut self, digest: [u8; 32], module: wasmtime::Module) {
        if self.entries.contains_key(&digest) {
            return;
        }
        while self.entries.len() >= MAX_CACHED_WASM_MODULES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(digest);
        self.entries.insert(digest, module);
    }
}

struct SharedWasmEngine {
    engine: wasmtime::Engine,
    linker: wasmtime::Linker<WasmStoreState>,
    modules: Mutex<CompiledModuleCache>,
}

impl SharedWasmEngine {
    fn global() -> Result<&'static Self, WasmError> {
        static ENGINE: OnceLock<std::result::Result<SharedWasmEngine, String>> = OnceLock::new();
        let init = ENGINE.get_or_init(|| {
            let mut config = wasmtime::Config::new();
            config.consume_fuel(true);
            let engine = wasmtime::Engine::new(&config).map_err(|e| e.to_string())?;
            let mut linker = wasmtime::Linker::new(&engine);
            wasmtime_wasi::preview1::add_to_linker_sync(
                &mut linker,
                |state: &mut WasmStoreState| &mut state.wasi,
            )
            .map_err(|e| e.to_string())?;
            Ok(SharedWasmEngine {
                engine,
                linker,
                modules: Mutex::new(CompiledModuleCache::new()),
            })
        });
        init.as_ref()
            .map_err(|msg| WasmError::RuntimeCreationError(msg.clone()))
    }

    fn get_or_compile(
        &self,
        digest: [u8; 32],
        wasm_bytes: &[u8],
    ) -> Result<wasmtime::Module, WasmError> {
        if let Ok(cache) = self.modules.lock() {
            if let Some(module) = cache.get(&digest) {
                return Ok(module);
            }
        }
        let module = wasmtime::Module::from_binary(&self.engine, wasm_bytes)
            .map_err(|e| WasmError::ModuleLoadError(e.to_string()))?;
        if let Ok(mut cache) = self.modules.lock() {
            cache.insert(digest, module.clone());
        }
        Ok(module)
    }
}

/// Trait for WASM runtime implementations
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must_use on methods returning an already-must-use boxed Future"
)]
#[async_trait::async_trait]
pub trait WasmRuntime {
    /// Execute a WASM module with arguments
    async fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult>;

    /// Execute with explicit timeout
    async fn execute_with_timeout(
        &self,
        wasm_bytes: &[u8],
        args: &[&str],
        timeout: Duration,
    ) -> Result<ExecutionResult>;
}

/// Wasmtime-based WASM runtime implementation (with full WASI support).
///
/// Reuses a process-wide compiled [`wasmtime::Engine`], WASI [`wasmtime::Linker`],
/// and bounded SHA-256 compiled-module cache across executions while isolating
/// every run in its own `Store` with per-job fuel and memory limits.
#[derive(Clone)]
pub struct Wasm3Runtime {
    max_memory_bytes: u64,
    _stack_size_bytes: u64,
}

impl Wasm3Runtime {
    /// Create a new runtime with default limits
    pub fn new() -> Self {
        Self {
            max_memory_bytes: 128 * 1024 * 1024, // 128 MB
            _stack_size_bytes: 64 * 1024,        // 64 KB
        }
    }

    /// Set maximum memory limit
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Set stack size
    pub fn with_stack_size(mut self, bytes: u64) -> Self {
        self._stack_size_bytes = bytes;
        self
    }

    fn compute_module_digest(wasm_bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(wasm_bytes);
        hasher.finalize().into()
    }

    /// Compute SHA-256 hash of WASM module
    #[cfg(test)]
    fn compute_module_hash(wasm_bytes: &[u8]) -> String {
        format!(
            "sha256:{}",
            hex::encode(Self::compute_module_digest(wasm_bytes))
        )
    }
}

impl Default for Wasm3Runtime {
    fn default() -> Self {
        Self::new()
    }
}

// Inherent methods for Wasm3Runtime
impl Wasm3Runtime {
    fn execute_sync(
        wasm_bytes: &[u8],
        timeout: Duration,
        max_memory_bytes: u64,
    ) -> Result<ExecutionResult> {
        use wasmtime::*;

        info!("Executing WASM module ({} bytes)", wasm_bytes.len());
        let start = Instant::now();

        let digest = Self::compute_module_digest(wasm_bytes);
        let module_hash = format!("sha256:{}", hex::encode(digest));
        debug!("Module hash: {}", module_hash);

        let shared = SharedWasmEngine::global()?;
        let module = shared.get_or_compile(digest, wasm_bytes)?;

        // Create isolated WASI preview1 context with per-job memory ceiling.
        let wasi = wasmtime_wasi::WasiCtxBuilder::new()
            .inherit_stdio()
            .build_p1();
        let limits = StoreLimitsBuilder::new()
            .memory_size(usize::try_from(max_memory_bytes).unwrap_or(usize::MAX))
            .build();

        let mut store = Store::new(&shared.engine, WasmStoreState { wasi, limits });
        store.limiter(|state| &mut state.limits);

        // Set fuel limit based on timeout (rough heuristic: 1M instructions per second)
        let fuel_limit = timeout.as_secs().max(1).saturating_mul(1_000_000);
        store
            .set_fuel(fuel_limit)
            .map_err(|e| WasmError::RuntimeCreationError(e.to_string()))?;

        // Instantiate module against the shared pre-populated WASI linker
        let instance = shared
            .linker
            .instantiate(&mut store, &module)
            .map_err(|e| WasmError::ModuleLoadError(e.to_string()))?;

        // Find entry point (_start for WASI)
        let func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| WasmError::ExecutionError(format!("No _start function: {}", e)))?;

        // Execute (fuel exhaustion or memory cap causes error)
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

#[async_trait::async_trait]
impl WasmRuntime for Wasm3Runtime {
    async fn execute(&self, wasm_bytes: &[u8], args: &[&str]) -> Result<ExecutionResult> {
        // Default timeout: 5 minutes
        self.execute_with_timeout(wasm_bytes, args, Duration::from_secs(300))
            .await
    }

    async fn execute_with_timeout(
        &self,
        wasm_bytes: &[u8],
        _args: &[&str],
        timeout: Duration,
    ) -> Result<ExecutionResult> {
        // Clone data for move into spawn_blocking
        let wasm_bytes = wasm_bytes.to_vec();
        let max_memory = self.max_memory_bytes;

        // Run blocking WASM execution in blocking thread pool
        let result = tokio::task::spawn_blocking(move || {
            Self::execute_sync(&wasm_bytes, timeout, max_memory)
        })
        .await?;

        result
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
        assert_eq!(runtime._stack_size_bytes, 128 * 1024);
    }

    #[test]
    fn test_compute_module_hash() {
        let wasm_bytes = b"fake wasm module";
        let hash = Wasm3Runtime::compute_module_hash(wasm_bytes);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 71); // "sha256:" + 64 hex chars
    }

    #[test]
    fn test_shared_engine_caches_compiled_module() {
        // Minimal valid WASI-compatible module exporting empty `_start`.
        let minimal_wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic + version
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type section: () -> ()
            0x03, 0x02, 0x01, 0x00, // func section
            0x07, 0x0a, 0x01, 0x06, b'_', b's', b't', b'a', b'r', b't', 0x00,
            0x00, // export _start
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // code: nop/end
        ];
        let digest = Wasm3Runtime::compute_module_digest(minimal_wasm);
        let shared = SharedWasmEngine::global().expect("shared engine");
        let _ = shared
            .get_or_compile(digest, minimal_wasm)
            .expect("first compile");
        let cached = shared
            .modules
            .lock()
            .expect("cache lock")
            .get(&digest)
            .is_some();
        assert!(cached, "compiled module should be cached by SHA-256 digest");
    }
}
