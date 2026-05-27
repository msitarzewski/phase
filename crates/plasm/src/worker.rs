// SPDX-License-Identifier: Apache-2.0

//! `WasmtimeWorker` -- Plasm's `phase_protocol::Worker` implementation.
//!
//! WASM jobs are batch by nature: a single `_start` invocation produces a
//! fixed result. We still model them through the streaming `Worker` trait
//! because that's the universal shape; the resulting stream is the
//! degenerate "one `Output` chunk + one `Final`" case.
//!
//! ## Wire-up
//!
//! 1. `execute()` decodes the [`JobSpec::Wasm`] payload from the signed
//!    manifest.
//! 2. The WASM bytes come from the `WasmJobSpec::input` field. (In the
//!    November 2025 MVP we passed the bytes inline; once
//!    `phase-artifact-server` is wired into the dispatch path the
//!    `module_cid` field is what the worker resolves against. For M7 the
//!    inline path is enough to keep `plasmd execute-job` and the legacy
//!    libp2p job test working.)
//! 3. The `Wasm3Runtime` (wasmtime-backed) executes the module via the
//!    blocking-pool path that all the existing WASM tests exercise.
//! 4. A single `OutputChunk { kind: "stdout", data, seq: 0 }` is folded into
//!    a `CommitmentAccumulator` and emitted before the terminal `Final`.
//! 5. The signed `SignedReceipt<JobResult>` is delivered through
//!    `JobHandleProducer::deliver_receipt`.

use std::time::Duration;

use async_stream::stream;
use bytes::Bytes;

use phase_identity::NodeIdentity;
use phase_protocol::{
    CommitmentAccumulator, Completion, JobEvent, JobHandle, JobId, JobMetrics, JobResult, JobSpec,
    JobSpecKind, JobStream, OutputChunk, SignedManifest, Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;

use crate::wasm::runtime::{Wasm3Runtime, WasmRuntime};

/// The kinds this worker supports — exactly one: `JobSpecKind::Wasm`.
const SUPPORTED: &[JobSpecKind] = &[JobSpecKind::Wasm];

/// Default wall-clock cap when the manifest doesn't supply one. Matches the
/// 5-minute default in `Wasm3Runtime::execute`.
const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(300);

/// Default memory cap. Matches `Wasm3Runtime::new`.
const DEFAULT_MAX_MEMORY: u64 = 128 * 1024 * 1024;

/// A `phase_protocol::Worker` that runs `JobSpec::Wasm` jobs through Wasmtime.
///
/// The worker is cheap to clone — only the node identity (an `Arc` of the
/// signing key) is held — so a router / scheduler can register the same
/// worker against multiple kinds without re-creating it.
#[derive(Clone)]
pub struct WasmtimeWorker {
    identity: NodeIdentity,
    capacity_hint: usize,
}

impl std::fmt::Debug for WasmtimeWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmtimeWorker")
            .field("capacity_hint", &self.capacity_hint)
            .field(
                "peer_id_prefix",
                &hex_prefix(&self.identity.verifying_key().to_bytes(), 4),
            )
            .finish()
    }
}

impl WasmtimeWorker {
    /// Construct a new worker that signs receipts with `identity`.
    pub fn new(identity: NodeIdentity) -> Self {
        // num_cpus is already a transitive dep via phase-net; use it for a
        // sensible default capacity hint.
        let capacity_hint = num_cpus::get().max(1);
        Self {
            identity,
            capacity_hint,
        }
    }

    /// Override the capacity hint advertised through `Worker::capacity_hint`.
    pub fn with_capacity_hint(mut self, hint: usize) -> Self {
        self.capacity_hint = hint.max(1);
        self
    }

    /// The signing identity used for receipts emitted by this worker.
    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }
}

impl Worker for WasmtimeWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        SUPPORTED
    }

    fn capacity_hint(&self) -> usize {
        self.capacity_hint
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        // Decode + validate the signed manifest.
        job.verify()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let job_id = JobId(manifest_hash);

        // Extract the WASM payload. The signing envelope already typed the
        // payload as JobSpec; we just match on the variant.
        let wasm_spec = match job.payload {
            JobSpec::Wasm(spec) => spec,
            other => {
                return Err(WorkerError::Unsupported { kind: other.kind() });
            }
        };

        let timeout = wasm_spec
            .max_duration_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_MAX_DURATION);
        let max_memory = wasm_spec.max_memory_bytes.unwrap_or(DEFAULT_MAX_MEMORY);
        let wasm_bytes: Vec<u8> = wasm_spec.input.clone();

        let (handle, mut producer) = JobHandle::new(job_id);
        let identity = self.identity.clone();

        // Build the stream. WASM execution is one-shot, so the stream emits
        // a single Output chunk (the stdout buffer) and a single Final.
        let stream: JobStream = Box::pin(stream! {
            // Execute via the existing wasmtime path. Errors here become
            // Completion::Error; the receipt is signed regardless so the
            // client can replay/observe the failure.
            let exec_result = Wasm3Runtime::new()
                .with_memory_limit(max_memory)
                .execute_with_timeout(&wasm_bytes, &[], timeout)
                .await;

            let mut acc = CommitmentAccumulator::new();

            let (completion, error_msg, stdout_bytes, wall_time_ms, exit_code, module_hash) =
                match exec_result {
                    Ok(r) => {
                        let completion = if r.exit_code == 0 {
                            Completion::Stop
                        } else {
                            Completion::Error
                        };
                        let err = (r.exit_code != 0)
                            .then(|| format!("wasm exit_code={}", r.exit_code));
                        (
                            completion,
                            err,
                            r.stdout.into_bytes(),
                            r.wall_time_ms,
                            r.exit_code,
                            r.module_hash,
                        )
                    }
                    Err(e) => (
                        Completion::Error,
                        Some(format!("wasm execution failed: {}", e)),
                        Vec::new(),
                        0,
                        1,
                        compute_module_hash(&wasm_bytes),
                    ),
                };

            // Emit the stdout chunk (even if empty, so verifiers see the
            // commitment account for it). Verifier-side replay reconstructs
            // the same commitment from the same chunk.
            let chunk = OutputChunk {
                kind: "stdout".to_string(),
                data: Bytes::from(stdout_bytes),
                seq: 0,
            };
            acc.update(&chunk);
            yield JobEvent::Output(chunk);

            let (output_commitment, output_chunk_count) = acc.finalize();

            // Worker-attested metrics — observability only.
            let mut metrics = JobMetrics {
                total_duration_ms: wall_time_ms,
                ..JobMetrics::default()
            };
            metrics.extra.insert("module_hash".to_string(), module_hash);
            metrics.extra.insert("exit_code".to_string(), exit_code.to_string());

            let result = JobResult {
                job_spec_hash: manifest_hash,
                output_commitment,
                output_chunk_count,
                completion: completion.clone(),
                resumption: None,
                metrics,
            };

            // Sign the receipt. Failure to sign is fatal — but with a valid
            // identity it shouldn't fail; degrade to dropping the receipt
            // delivery (the handle will resolve `WorkerError::Dropped`).
            if let Ok(receipt) = ReceiptBuilder::new(result.clone(), manifest_hash)
                .sign_with(&identity)
            {
                producer.deliver_receipt(receipt);
            }

            yield JobEvent::Final { result, error: error_msg };
        });

        Ok((handle, stream))
    }
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let take = n.min(bytes.len());
    let mut s = String::with_capacity(take * 2);
    for b in &bytes[..take] {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn compute_module_hash(wasm_bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(wasm_bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use phase_manifest::ManifestBuilder;
    use phase_protocol::{JobSpec, WasmJobSpec};

    fn tiny_wasm() -> Vec<u8> {
        // Minimal valid wasm module: magic + version. wasmtime will reject
        // it for missing `_start` -- exactly the path we want to exercise
        // for the error-completion case.
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    fn build_job(identity: &NodeIdentity, wasm_bytes: Vec<u8>) -> SignedManifest<JobSpec> {
        let payload = JobSpec::Wasm(WasmJobSpec {
            module_cid: "inline".to_string(),
            input: wasm_bytes,
            max_duration_ms: Some(5_000),
            max_memory_bytes: Some(64 * 1024 * 1024),
        });
        ManifestBuilder::new(payload)
            .sign_with(identity)
            .expect("sign manifest")
    }

    #[tokio::test]
    async fn supported_kinds_is_wasm_only() {
        let id = NodeIdentity::generate();
        let worker = WasmtimeWorker::new(id);
        assert_eq!(worker.supported_kinds(), &[JobSpecKind::Wasm]);
    }

    #[tokio::test]
    async fn capacity_hint_is_at_least_one() {
        let id = NodeIdentity::generate();
        let worker = WasmtimeWorker::new(id);
        assert!(worker.capacity_hint() >= 1);
    }

    #[tokio::test]
    async fn invalid_wasm_yields_error_completion_with_signed_receipt() {
        let id = NodeIdentity::generate();
        let worker = WasmtimeWorker::new(id.clone());
        let job = build_job(&id, tiny_wasm());

        let (handle, mut stream) = worker.execute(job).await.expect("dispatch");

        // First event: Output (empty stdout for a failed execution).
        let first = stream.next().await.expect("output event");
        assert!(matches!(first, JobEvent::Output(_)));

        // Second event: Final with Completion::Error.
        let last = stream.next().await.expect("final event");
        match last {
            JobEvent::Final { result, error } => {
                assert_eq!(result.completion, Completion::Error);
                assert!(error.is_some());
                assert_eq!(result.output_chunk_count, 1);
            }
            _ => panic!("expected Final"),
        }
        assert!(stream.next().await.is_none(), "stream must terminate");

        // Receipt should be signed and verifiable.
        let receipt = handle.finish().await.expect("receipt delivered");
        receipt.verify().expect("receipt verifies");
    }
}
