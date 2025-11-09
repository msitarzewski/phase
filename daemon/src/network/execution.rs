use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::wasm::receipt::Receipt;
use crate::wasm::runtime::{WasmRuntime, Wasm3Runtime};
use super::protocol::{JobRequest, JobResult};

/// Job execution handler
pub struct ExecutionHandler {
    /// Node's signing key for receipts
    signing_key: SigningKey,
}

impl ExecutionHandler {
    /// Create a new execution handler
    pub fn new(signing_key: SigningKey) -> Self {
        Self { signing_key }
    }

    /// Execute a job request and return the result
    pub async fn execute_job(&self, request: JobRequest) -> Result<JobResult> {
        info!("Executing job: {} (hash: {})", request.job_id, request.module_hash);

        // Validate request
        request.validate()
            .map_err(|e| anyhow::anyhow!("Job request validation failed: {}", e))?;

        // Verify module hash
        let computed_hash = self.compute_module_hash(&request.wasm_bytes);
        if computed_hash != request.module_hash {
            warn!(
                "Module hash mismatch: expected {}, got {}",
                request.module_hash, computed_hash
            );
            return Err(anyhow::anyhow!(
                "Module hash mismatch: expected {}, got {}",
                request.module_hash,
                computed_hash
            ));
        }

        debug!("Module hash verified: {}", computed_hash);

        // Execute WASM in sandbox
        let runtime = Wasm3Runtime::new()
            .with_memory_limit(request.requirements.memory_mb * 1024 * 1024);

        let args_refs: Vec<&str> = request.args.iter().map(|s| s.as_str()).collect();

        let timeout = Duration::from_secs(request.requirements.timeout_seconds);
        let exec_result = runtime.execute_with_timeout(&request.wasm_bytes, &args_refs, timeout).await
            .context("WASM execution failed")?;

        info!(
            "Job {} complete: exit_code={}, time={}ms",
            request.job_id, exec_result.exit_code, exec_result.wall_time_ms
        );

        // Create and sign receipt
        let mut receipt = Receipt::new(
            exec_result.module_hash.clone(),
            exec_result.exit_code,
            exec_result.wall_time_ms,
        );

        receipt.sign(&self.signing_key)
            .map_err(|e| anyhow::anyhow!("Failed to sign receipt: {}", e))?;

        let receipt_json = receipt.to_json()
            .map_err(|e| anyhow::anyhow!("Failed to serialize receipt: {}", e))?;

        debug!("Receipt signed with pubkey: {}", receipt.node_pubkey);

        // Return result
        Ok(JobResult {
            job_id: request.job_id,
            stdout: exec_result.stdout,
            stderr: exec_result.stderr,
            exit_code: exec_result.exit_code,
            receipt_json,
        })
    }

    /// Compute SHA-256 hash of WASM module
    fn compute_module_hash(&self, wasm_bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(wasm_bytes);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }

    /// Get the node's public key (hex-encoded)
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing_key.verifying_key().to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::JobRequirements;
    use rand::RngCore;

    fn generate_signing_key() -> SigningKey {
        let mut secret_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        SigningKey::from_bytes(&secret_bytes)
    }

    #[tokio::test]
    async fn test_execution_handler_creation() {
        let signing_key = generate_signing_key();
        let handler = ExecutionHandler::new(signing_key);

        assert!(!handler.public_key_hex().is_empty());
        assert_eq!(handler.public_key_hex().len(), 64); // 32 bytes hex
    }

    #[tokio::test]
    async fn test_module_hash_verification() {
        let signing_key = generate_signing_key();
        let handler = ExecutionHandler::new(signing_key);

        let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d]; // WASM magic
        let correct_hash = handler.compute_module_hash(&wasm_bytes);

        let request = JobRequest::new(
            "test-job".to_string(),
            correct_hash.clone(),
            wasm_bytes,
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );

        // Hash verification happens inside execute_job
        // This will fail at WASM execution but hash check passes
        let result = handler.execute_job(request).await;
        // Should fail at execution (invalid WASM), not hash check
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(!err_msg.contains("hash mismatch"));
    }

    #[tokio::test]
    async fn test_invalid_hash_rejected() {
        let signing_key = generate_signing_key();
        let handler = ExecutionHandler::new(signing_key);

        let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d];
        let wrong_hash = "sha256:deadbeef".to_string();

        let request = JobRequest::new(
            "test-job".to_string(),
            wrong_hash,
            wasm_bytes,
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );

        let result = handler.execute_job(request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("hash mismatch"));
    }
}
