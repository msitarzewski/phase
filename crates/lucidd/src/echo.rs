// SPDX-License-Identifier: AGPL-3.0-or-later

//! `EchoWorker` — a degenerate `phase-protocol::Worker` that "infers" by
//! reversing the user's last message and streaming it back one token at a
//! time. Exists so we can prove the streaming pipeline end-to-end before
//! LUCID M2 wires up llama.cpp.
//!
//! This is *not* `phase-protocol`'s test-fixture `EchoWorker` (that one
//! handles `Wasm` and emits a single static `"hello"`). It's an inference-
//! shaped sibling — exactly the shape `LlamaCppWorker` will take in M2,
//! minus the real model.

use std::time::Duration;

use async_stream::stream;
use bytes::Bytes;
use phase_identity::NodeIdentity;
use phase_protocol::{
    ChatMessage, ChatRole, CommitmentAccumulator, Completion, JobEvent, JobHandle, JobHandleProducer,
    JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream, OutputChunk, SignedManifest,
    Worker, WorkerError,
};
use phase_receipt::ReceiptBuilder;

/// Streams the reversed last user message back, one character per
/// `OutputChunk`, with a tiny inter-token delay to make the streaming
/// visible to a real client (CLI / curl / Open WebUI).
#[derive(Debug, Clone)]
pub struct EchoWorker {
    /// Per-token delay. Small enough that `ollama run` doesn't feel sluggish,
    /// large enough that NDJSON framing is observable in a curl trace.
    pub token_delay: Duration,

    /// Worker identity used to sign receipts. Per `phase-core M5`, every
    /// receipt must be a real `SignedReceipt` produced by the worker that
    /// generated the stream.
    pub identity: NodeIdentity,
}

impl Default for EchoWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoWorker {
    pub fn new() -> Self {
        Self {
            token_delay: Duration::from_millis(30),
            identity: NodeIdentity::generate(),
        }
    }
}

impl Worker for EchoWorker {
    fn supported_kinds(&self) -> &[JobSpecKind] {
        &[JobSpecKind::Inference]
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        // Dispatch-time validation: this worker only handles inference jobs.
        let inference = match &job.payload {
            JobSpec::Inference(spec) => spec.clone(),
            other => {
                return Err(WorkerError::Unsupported {
                    kind: other.kind(),
                });
            }
        };

        // Derive the JobId from the manifest's canonical hash. Falls back
        // to all-zeros only if canonicalization itself fails — which would
        // mean the payload's Serialize impl was broken (extremely rare).
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let job_id = JobId(manifest_hash);
        let (handle, producer) = JobHandle::new(job_id);
        let token_delay = self.token_delay;
        let identity = self.identity.clone();

        let stream: JobStream =
            Box::pin(echo_stream(inference, manifest_hash, producer, token_delay, identity));
        Ok((handle, stream))
    }
}

/// Drive the JobStream: reverse the last user message and yield one
/// `OutputChunk` per character, plus a terminal `Final`. Cooperates with
/// cancellation via `producer.is_cancelled()`.
fn echo_stream(
    inference: phase_protocol::InferenceJobSpec,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    token_delay: Duration,
    identity: NodeIdentity,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    stream! {
        // Pick the last user message. If `messages` is empty, fall back to
        // `prompt`. If both are empty, echo a stock placeholder so the
        // stream still produces something observable.
        let source = last_user_text(&inference.messages)
            .or(inference.prompt.clone())
            .unwrap_or_else(|| "(empty input)".to_string());
        let reversed: String = source.chars().rev().collect();

        let mut acc = CommitmentAccumulator::new();
        let mut completion_tokens: u64 = 0;
        let mut cancelled = false;

        // Stream one character per chunk. `chars().collect::<Vec<_>>()` is
        // intentional — we want to release the borrow on `reversed` before
        // the await point so the stream stays `Send` cleanly. `enumerate()`
        // drives the per-chunk `seq` so we don't carry a parallel counter
        // (clippy::explicit_counter_loop).
        let chars: Vec<char> = reversed.chars().collect();
        for (i, ch) in chars.iter().enumerate() {
            if producer.is_cancelled() {
                cancelled = true;
                break;
            }

            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            let chunk = OutputChunk {
                kind: "token".to_string(),
                data: Bytes::copy_from_slice(s.as_bytes()),
                seq: i as u64,
            };
            acc.update(&chunk);
            completion_tokens += 1;
            yield JobEvent::Output(chunk);

            // Inter-token delay. tokio::time::sleep is cancellation-safe.
            tokio::time::sleep(token_delay).await;
        }

        let (commitment, count) = acc.finalize();
        let completion = if cancelled {
            Completion::Cancelled
        } else {
            Completion::Stop
        };

        let result = JobResult {
            job_spec_hash: manifest_hash,
            output_commitment: commitment,
            output_chunk_count: count,
            completion,
            resumption: None,
            metrics: JobMetrics {
                total_duration_ms: 0,
                prompt_tokens: source.chars().count() as u64,
                completion_tokens,
                ..Default::default()
            },
        };

        // Sign the receipt under the worker identity. phase-core M5 wired
        // up the real signing surface; the HTTP layer pulls the commitment
        // off `receipt.result.output_commitment` for the `X-Phase-Receipt`
        // header.
        let receipt = ReceiptBuilder::new(result.clone(), manifest_hash)
            .sign_with(&identity)
            .expect("sign receipt (Serialize impls are infallible)");
        producer.deliver_receipt(receipt);

        yield JobEvent::Final {
            result,
            error: None,
        };
    }
}

fn last_user_text(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, ChatRole::User))
        .map(|m| m.content.clone())
}
