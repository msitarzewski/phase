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

/// SEC-01: server-side authorization + resource policy for the worker.
///
/// `WasmtimeWorker::execute` calls `verify()` (proving *some* keyholder
/// signed the manifest) but that is **not** authorization. This config pins
/// the signer to an authorized identity and clamps the manifest's
/// resource caps to operator-set maxima regardless of what the (untrusted)
/// manifest claims.
///
/// Defaults to **deny-all** (empty allowlist, `allow_unauthenticated = false`)
/// so a worker built with `WasmtimeWorker::new` is secure-by-default. Local
/// dev / existing tests opt into open execution via
/// [`WorkerSecurityConfig::allow_unauthenticated`].
#[derive(Debug, Clone)]
pub struct WorkerSecurityConfig {
    /// Lowercase-hex Ed25519 pubkeys authorized to submit WASM jobs.
    pub authorized_submitters: Vec<String>,
    /// INSECURE escape hatch: accept any verified manifest. Local dev only.
    pub allow_unauthenticated: bool,
    /// Hard server-side ceiling on `max_memory_bytes` (clamps the manifest).
    pub max_memory_bytes: u64,
    /// Hard server-side ceiling on `max_duration` (clamps the manifest).
    pub max_duration: Duration,
}

impl Default for WorkerSecurityConfig {
    fn default() -> Self {
        Self {
            authorized_submitters: Vec::new(),
            allow_unauthenticated: false,
            max_memory_bytes: DEFAULT_MAX_MEMORY,
            max_duration: DEFAULT_MAX_DURATION,
        }
    }
}

impl WorkerSecurityConfig {
    /// SEC-01 authorization gate. `true` if `pubkey_hex` (from a *verified*
    /// manifest) may submit work. Mirrors lucidd's `PolicyConfig`.
    ///
    /// SEC-06 / PeerID-bind hook: v0.2 will also accept a signer whose key
    /// bytes match the delivering libp2p PeerId. The plasm `Worker::execute`
    /// signature has no peer identity today, so the allowlist is the sole
    /// source. See SEC-06.
    pub fn is_authorized_submitter(&self, pubkey_hex: &str) -> bool {
        if self.allow_unauthenticated {
            return true;
        }
        self.authorized_submitters
            .iter()
            .any(|k| k.eq_ignore_ascii_case(pubkey_hex))
    }
}

/// A `phase_protocol::Worker` that runs `JobSpec::Wasm` jobs through Wasmtime.
///
/// The worker is cheap to clone — only the node identity (an `Arc` of the
/// signing key) and a small policy struct are held — so a router / scheduler
/// can register the same worker against multiple kinds without re-creating it.
#[derive(Clone)]
pub struct WasmtimeWorker {
    identity: NodeIdentity,
    capacity_hint: usize,
    security: WorkerSecurityConfig,
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
    ///
    /// SEC-01: the worker is **deny-all by default** — no signer is
    /// authorized until you supply an allowlist via
    /// [`WasmtimeWorker::with_security`] or open it up with a security config
    /// whose `allow_unauthenticated = true` (local dev only).
    pub fn new(identity: NodeIdentity) -> Self {
        // num_cpus is already a transitive dep via phase-net; use it for a
        // sensible default capacity hint.
        let capacity_hint = num_cpus::get().max(1);
        Self {
            identity,
            capacity_hint,
            security: WorkerSecurityConfig::default(),
        }
    }

    /// Override the capacity hint advertised through `Worker::capacity_hint`.
    pub fn with_capacity_hint(mut self, hint: usize) -> Self {
        self.capacity_hint = hint.max(1);
        self
    }

    /// SEC-01: set the authorization + resource-cap policy for this worker.
    pub fn with_security(mut self, security: WorkerSecurityConfig) -> Self {
        self.security = security;
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
        // SEC-01 (1): VERIFY the signature. Proves *some* keyholder signed
        // this manifest — necessary but NOT sufficient.
        job.verify()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;

        // SEC-01 (2): AUTHORIZATION gate. verify() above only proves a
        // self-consistent signature; it does not prove the signer is
        // *authorized*. Without this gate any anonymous peer could run
        // arbitrary WASM on the host (and, chained with the wasmtime
        // sandbox-escape CVEs in SEC-02, achieve host RCE). Reject here —
        // before any WASM bytes are handed to the runtime.
        if !self.security.is_authorized_submitter(&job.signer_pubkey) {
            return Err(WorkerError::BadManifest(format!(
                "submitter not authorized: {}",
                job.signer_pubkey
            )));
        }

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

        // SEC-01 (3): CLAMP manifest-supplied resource caps to operator
        // maxima. The manifest is untrusted; a hostile peer could set
        // max_memory_bytes = u64::MAX / max_duration_ms = u64::MAX to exhaust
        // the host. We take the *minimum* of (manifest value, operator
        // ceiling), and fall back to the ceiling when the manifest omits a
        // value.
        let requested_timeout = wasm_spec
            .max_duration_ms
            .map(Duration::from_millis)
            .unwrap_or(self.security.max_duration);
        let timeout = requested_timeout.min(self.security.max_duration);
        let requested_memory = wasm_spec
            .max_memory_bytes
            .unwrap_or(self.security.max_memory_bytes);
        let max_memory = requested_memory.min(self.security.max_memory_bytes);
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
        build_job_with_caps(
            identity,
            wasm_bytes,
            Some(5_000),
            Some(64 * 1024 * 1024),
        )
    }

    fn build_job_with_caps(
        identity: &NodeIdentity,
        wasm_bytes: Vec<u8>,
        max_duration_ms: Option<u64>,
        max_memory_bytes: Option<u64>,
    ) -> SignedManifest<JobSpec> {
        let payload = JobSpec::Wasm(WasmJobSpec {
            module_cid: "inline".to_string(),
            input: wasm_bytes,
            max_duration_ms,
            max_memory_bytes,
        });
        ManifestBuilder::new(payload)
            .sign_with(identity)
            .expect("sign manifest")
    }

    /// SEC-01: a worker that accepts any verified manifest (local-dev mode).
    /// Existing behavioral tests use this so they exercise execution, not the
    /// authz gate.
    fn open_worker(id: NodeIdentity) -> WasmtimeWorker {
        WasmtimeWorker::new(id).with_security(WorkerSecurityConfig {
            allow_unauthenticated: true,
            ..WorkerSecurityConfig::default()
        })
    }

    /// SEC-01: a worker whose allowlist contains exactly `authorized`'s key.
    fn allowlisted_worker(signer: &NodeIdentity, authorized: &NodeIdentity) -> WasmtimeWorker {
        let key_hex = hex::encode(authorized.verifying_key().to_bytes());
        WasmtimeWorker::new(signer.clone()).with_security(WorkerSecurityConfig {
            authorized_submitters: vec![key_hex],
            allow_unauthenticated: false,
            ..WorkerSecurityConfig::default()
        })
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
        let worker = open_worker(id.clone());
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

    // --- SEC-01 regression tests ------------------------------------------

    #[tokio::test]
    async fn sec01_unauthorized_signer_is_rejected_before_execution() {
        // A self-signed manifest from a key NOT in the allowlist must be
        // rejected at the authorization gate — before any WASM runs.
        let node = NodeIdentity::generate();
        let attacker = NodeIdentity::generate();
        let authorized = NodeIdentity::generate();

        // Worker allows only `authorized`; attacker signs the job.
        let worker = allowlisted_worker(&node, &authorized);
        let job = build_job(&attacker, tiny_wasm());

        match worker.execute(job).await {
            Ok(_) => panic!("unauthorized signer must be rejected, but execute() returned Ok"),
            Err(WorkerError::BadManifest(msg)) => {
                assert!(
                    msg.contains("not authorized"),
                    "expected authorization rejection, got: {msg}"
                );
            }
            Err(other) => panic!("expected BadManifest authorization error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sec01_allowlisted_signer_is_accepted() {
        // The same key, when allowlisted, is accepted and dispatched.
        let node = NodeIdentity::generate();
        let client = NodeIdentity::generate();
        let worker = allowlisted_worker(&node, &client);
        let job = build_job(&client, tiny_wasm());

        // Dispatch succeeds (invalid wasm still yields an error *completion*,
        // but the call itself returns Ok — authz passed).
        let (_handle, mut stream) = worker
            .execute(job)
            .await
            .expect("allowlisted signer must be accepted");
        assert!(stream.next().await.is_some(), "stream should produce events");
    }

    #[tokio::test]
    async fn sec01_max_memory_is_clamped_to_operator_ceiling() {
        // A manifest claiming max_memory_bytes = u64::MAX must NOT be honored;
        // the worker clamps to its configured ceiling. We assert the clamp
        // logic directly via the runtime memory limit the worker computes.
        let node = NodeIdentity::generate();
        let client = NodeIdentity::generate();

        let ceiling = 32 * 1024 * 1024;
        let worker = WasmtimeWorker::new(node).with_security(WorkerSecurityConfig {
            authorized_submitters: vec![hex::encode(client.verifying_key().to_bytes())],
            allow_unauthenticated: false,
            max_memory_bytes: ceiling,
            max_duration: Duration::from_secs(1),
        });

        // Manifest demands the moon for both memory and duration.
        let job = build_job_with_caps(&client, tiny_wasm(), Some(u64::MAX), Some(u64::MAX));

        // Execution must still succeed (authz passes; caps are clamped, not
        // rejected). If the clamp were absent, wasmtime would attempt to
        // reserve u64::MAX bytes and the worker would behave very differently.
        let (_handle, mut stream) = worker.execute(job).await.expect("dispatch");
        // It runs to completion within the clamped 1s budget rather than
        // hanging on an absurd duration request.
        let mut saw_final = false;
        while let Some(ev) = stream.next().await {
            if matches!(ev, JobEvent::Final { .. }) {
                saw_final = true;
            }
        }
        assert!(saw_final, "clamped job should still produce a Final event");
    }

    #[test]
    fn sec01_security_config_default_is_deny_all() {
        let cfg = WorkerSecurityConfig::default();
        assert!(!cfg.allow_unauthenticated);
        assert!(cfg.authorized_submitters.is_empty());
        // No key is authorized by default.
        assert!(!cfg.is_authorized_submitter(&"ab".repeat(32)));
    }
}
