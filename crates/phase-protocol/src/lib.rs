// SPDX-License-Identifier: Apache-2.0

//! # phase-protocol
//!
//! The `Worker` trait and `JobSpec` enum — the abstraction every Phase node
//! implementation impls to participate in the network.
//!
//! See [`SPEC.md`](https://github.com/msitarzewski/phase/blob/main/crates/phase-protocol/SPEC.md)
//! for the narrative spec, rationale, and rejected alternatives.
//!
//! ## At a glance
//!
//! - [`Worker`] — the trait. One method: `execute(SignedManifest<JobSpec>) ->
//!   (JobHandle, JobStream)`. Streaming is the universal shape; batch is the
//!   degenerate one-event stream.
//! - [`JobSpec`] — discriminated union of workload payloads
//!   ([`WasmJobSpec`], [`InferenceJobSpec`], future variants).
//! - [`JobEvent`] — items on the stream: `Output(OutputChunk)`,
//!   `Progress(ProgressUpdate)`, or terminal `Final`.
//! - [`CommitmentAccumulator`] — SHA-256 chain over output chunks.
//!   Workers fold each chunk in; the final state goes into the signed
//!   `JobResult`; verifiers reconstruct it from the chunks they saw.
//! - [`JobHandle`] — cancellation + signed-receipt retrieval.
//! - [`ConversationToken`] — opaque resumption handle for KV-cache reuse.

#![deny(missing_debug_implementations)]
#![deny(unsafe_code)]

mod commitment;
mod job_spec;
mod worker;

pub use commitment::CommitmentAccumulator;
pub use job_spec::{
    ChatMessage, ChatRole, Completion, ConversationToken, InferenceJobSpec, JobMetrics, JobResult,
    JobSpec, JobSpecKind, PeerId, SamplingParams, WasmJobSpec,
};
pub use worker::{
    should_resume_on_same_peer, DynWorker, JobEvent, JobHandle, JobHandleProducer, JobId,
    JobStream, OutputChunk, ProgressUpdate, Worker, WorkerError, DEFAULT_RESUMPTION_GRACE,
};

// ---------------------------------------------------------------------------
// Signed envelope types (real)
// ---------------------------------------------------------------------------
//
// As of phase-core M5, the envelope types live in their own crates and are
// re-exported here so trait + smoke-test sites keep referring to
// `phase_protocol::SignedManifest<T>` / `SignedReceipt<T>` unchanged.

pub use phase_manifest::SignedManifest;
pub use phase_receipt::SignedReceipt;

// ---------------------------------------------------------------------------
// Smoke tests at the crate root — exercise the public API surface.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod smoke {
    use super::*;
    use bytes::Bytes;
    use futures_util::StreamExt;
    use phase_identity::NodeIdentity;
    use phase_manifest::ManifestBuilder;
    use phase_receipt::ReceiptBuilder;
    use std::sync::Arc;

    /// A degenerate worker that emits a single `Output` chunk and a `Final`.
    /// Exists to prove the trait can be implemented and called.
    struct EchoWorker {
        identity: NodeIdentity,
    }

    impl Worker for EchoWorker {
        fn supported_kinds(&self) -> &[JobSpecKind] {
            &[JobSpecKind::Wasm]
        }

        async fn execute(
            &self,
            job: SignedManifest<JobSpec>,
        ) -> Result<(JobHandle, JobStream), WorkerError> {
            let manifest_hash = job
                .manifest_hash()
                .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
            let job_id = JobId(manifest_hash);
            let (handle, mut producer) = JobHandle::new(job_id);
            let identity = self.identity.clone();

            let stream = async_stream::stream! {
                let mut acc = CommitmentAccumulator::new();
                let chunk = OutputChunk {
                    kind: "stdout".into(),
                    data: Bytes::from_static(b"hello"),
                    seq: 0,
                };
                acc.update(&chunk);
                yield JobEvent::Output(chunk);

                let (commitment, count) = acc.finalize();
                let result = JobResult {
                    job_spec_hash: manifest_hash,
                    output_commitment: commitment,
                    output_chunk_count: count,
                    completion: Completion::Stop,
                    resumption: None,
                    metrics: JobMetrics::default(),
                };
                let receipt = ReceiptBuilder::new(result.clone(), manifest_hash)
                    .sign_with(&identity)
                    .expect("sign receipt");
                producer.deliver_receipt(receipt);
                yield JobEvent::Final { result, error: None };
            };

            Ok((handle, Box::pin(stream)))
        }
    }

    #[tokio::test]
    async fn echo_worker_round_trips() {
        let identity = NodeIdentity::generate();
        let worker: Arc<dyn DynWorker> = Arc::new(EchoWorker {
            identity: identity.clone(),
        });
        assert_eq!(worker.supported_kinds(), &[JobSpecKind::Wasm]);

        let payload = JobSpec::Wasm(WasmJobSpec {
            module_cid: "bafy…".into(),
            input: vec![],
            max_duration_ms: None,
            max_memory_bytes: None,
        });
        let manifest = ManifestBuilder::new(payload)
            .sign_with(&identity)
            .expect("sign manifest");

        let (handle, mut stream) = worker.execute_boxed(manifest).await.unwrap();

        // Should see one Output then Final.
        let first = stream.next().await.unwrap();
        assert!(matches!(first, JobEvent::Output(_)));
        let last = stream.next().await.unwrap();
        assert!(matches!(last, JobEvent::Final { .. }));
        assert!(stream.next().await.is_none());

        // And the receipt should be retrievable.
        let receipt = handle.finish().await.unwrap();
        assert_eq!(receipt.result.output_chunk_count, 1);
        receipt.verify().expect("verify receipt signature");
    }

    #[test]
    fn job_spec_kind_round_trips_through_serde() {
        let kinds = [JobSpecKind::Wasm, JobSpecKind::Inference];
        for k in kinds {
            let json = serde_json::to_string(&k).unwrap();
            let back: JobSpecKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }
}
