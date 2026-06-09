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
    ChatMessage, ChatRole, CommitmentAccumulator, Completion, EmbeddingJobSpec, JobEvent,
    JobHandle, JobHandleProducer, JobId, JobMetrics, JobResult, JobSpec, JobSpecKind, JobStream,
    OutputChunk, SignedManifest, Worker, WorkerError,
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
        &[JobSpecKind::Inference, JobSpecKind::Embedding]
    }

    async fn execute(
        &self,
        job: SignedManifest<JobSpec>,
    ) -> Result<(JobHandle, JobStream), WorkerError> {
        // Derive the JobId from the manifest's canonical hash. Falls back
        // to all-zeros only if canonicalization itself fails — which would
        // mean the payload's Serialize impl was broken (extremely rare).
        // Shared across both arms below so the JobId/commitment binding is
        // identical regardless of which workload shape we're serving.
        let manifest_hash = job
            .manifest_hash()
            .map_err(|e| WorkerError::BadManifest(e.to_string()))?;
        let job_id = JobId(manifest_hash);
        let (handle, producer) = JobHandle::new(job_id);
        let token_delay = self.token_delay;
        let identity = self.identity.clone();

        // Dispatch-time validation: this worker serves inference (reversed
        // tokens) and embedding (deterministic fake vectors) jobs only.
        let stream: JobStream = match &job.payload {
            JobSpec::Inference(spec) => Box::pin(echo_stream(
                spec.clone(),
                manifest_hash,
                producer,
                token_delay,
                identity,
            )),
            JobSpec::Embedding(spec) => Box::pin(embedding_stream(
                spec.clone(),
                manifest_hash,
                producer,
                identity,
            )),
            other => {
                return Err(WorkerError::Unsupported { kind: other.kind() });
            }
        };
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

/// Dimension of the deterministic fake embeddings. Sixteen is large enough
/// to exercise the JSON-vector wire shape and cosine-distance plumbing
/// downstream, small enough to keep test vectors readable. A real backend
/// (LUCID M2's MLX / llama.cpp path) emits the model's native dimension here.
const EMBEDDING_DIM: usize = 16;

/// Drive the JobStream for an embedding job: emit one `"embedding"`-kind
/// `OutputChunk` per input string (per the shared embedding wire convention),
/// each carrying `serde_json::to_vec(&Vec<f32>)`, then a terminal signed
/// `Final`. Mirrors `echo_stream`'s commitment/receipt machinery exactly —
/// the only difference is what rides in each chunk.
///
/// The vectors are *deterministic fakes*: this is the GPU-less dev/test
/// backend, so a stable SHA-256-derived vector is the explicitly-sanctioned
/// stand-in for a real model. Same input → same vector, always (the unit
/// tests lean on this), which is what makes the embedding path testable
/// without a GPU.
fn embedding_stream(
    spec: EmbeddingJobSpec,
    manifest_hash: [u8; 32],
    mut producer: JobHandleProducer,
    identity: NodeIdentity,
) -> impl futures::Stream<Item = JobEvent> + Send + 'static {
    stream! {
        let mut acc = CommitmentAccumulator::new();
        let mut completion_tokens: u64 = 0;
        let mut cancelled = false;

        // Empty `input` is valid: emit zero chunks and still produce a Final
        // whose commitment is over zero chunks. `enumerate()` drives `seq` so
        // we don't carry a parallel counter (clippy::explicit_counter_loop).
        let inputs = spec.input.clone();
        let prompt_tokens: u64 = inputs.iter().map(|s| s.chars().count() as u64).sum();

        for (i, input) in inputs.iter().enumerate() {
            if producer.is_cancelled() {
                cancelled = true;
                break;
            }

            let vector = fake_embedding(input);
            let chunk = OutputChunk {
                kind: "embedding".to_string(),
                // Serialize impls for Vec<f32> are infallible; the wire
                // convention is serde_json::to_vec(&vector).
                data: Bytes::from(serde_json::to_vec(&vector).unwrap()),
                seq: i as u64,
            };
            acc.update(&chunk);
            completion_tokens += 1;
            yield JobEvent::Output(chunk);
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
                // prompt_tokens: total input characters across all strings;
                // completion_tokens: number of vectors actually emitted.
                prompt_tokens,
                completion_tokens,
                ..Default::default()
            },
        };

        // Sign under the worker identity — identical to echo_stream so the
        // HTTP layer pulls the commitment off `receipt.result.output_commitment`
        // the same way for both workload shapes.
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

/// Compute a deterministic, unit-length fake embedding for `input`.
///
/// SHA-256 the input bytes, then deterministically expand that digest into
/// `EMBEDDING_DIM` `f32` components (re-hashing with a counter when the
/// dimension outruns the 32-byte digest), map each byte into `[-1.0, 1.0]`,
/// and L2-normalize. Same input always yields the same vector — the property
/// the embedding tests assert on. If the vector would be all-zero (it can't
/// for SHA-256 output, but we guard rather than risk a NaN from dividing by a
/// zero norm), fall back to the raw components.
fn fake_embedding(input: &str) -> Vec<f32> {
    use sha2::{Digest, Sha256};

    let mut components = Vec::with_capacity(EMBEDDING_DIM);
    let mut counter: u32 = 0;
    while components.len() < EMBEDDING_DIM {
        // Domain-separated, counter-extended digest so dim > 32 is stable
        // and architecture-independent (matches registry.rs's hashing style).
        let mut hasher = Sha256::new();
        hasher.update(b"phase/echo-embedding-v1:");
        hasher.update(counter.to_le_bytes());
        hasher.update(input.as_bytes());
        let digest = hasher.finalize();
        for &byte in digest.iter() {
            if components.len() == EMBEDDING_DIM {
                break;
            }
            // Map a byte in [0, 255] to a stable f32 in [-1.0, 1.0].
            components.push((f32::from(byte) / 127.5) - 1.0);
        }
        counter += 1;
    }

    let norm = components.iter().map(|c| c * c).sum::<f32>().sqrt();
    if norm > 0.0 {
        for c in &mut components {
            *c /= norm;
        }
    }
    components
}

fn last_user_text(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, ChatRole::User))
        .map(|m| m.content.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use phase_manifest::ManifestBuilder;

    /// Drive the EchoWorker against an embedding job and assert the shared
    /// embedding wire convention end-to-end: one `"embedding"` Output chunk
    /// per input (ordered by `seq`), each decoding to a `Vec<f32>` of the
    /// fixed dimension, a terminal `Final` whose `output_chunk_count` matches,
    /// and determinism (same input → identical vector).
    #[tokio::test]
    async fn embedding_job_streams_vectors() {
        let client = NodeIdentity::generate();
        let spec = JobSpec::Embedding(EmbeddingJobSpec {
            model_cid: "embed-test".to_string(),
            input: vec!["hello".to_string(), "world".to_string()],
        });
        let manifest = ManifestBuilder::new(spec).sign_with(&client).unwrap();

        let worker = EchoWorker::new();
        let (_handle, mut stream) = worker.execute(manifest).await.unwrap();

        // Drain, collecting Output chunks (keyed by seq) and the Final.
        let mut chunks: Vec<(u64, Vec<f32>)> = Vec::new();
        let mut final_count: Option<u64> = None;
        while let Some(ev) = stream.next().await {
            match ev {
                JobEvent::Output(chunk) => {
                    assert_eq!(chunk.kind, "embedding", "Output chunk kind");
                    let vector: Vec<f32> = serde_json::from_slice(&chunk.data)
                        .expect("embedding chunk decodes to Vec<f32>");
                    chunks.push((chunk.seq, vector));
                }
                JobEvent::Final { result, error } => {
                    assert!(error.is_none(), "embedding Final carried an error");
                    final_count = Some(result.output_chunk_count);
                }
                _ => {}
            }
        }

        // Exactly two embedding chunks, one per input.
        assert_eq!(chunks.len(), 2, "expected one chunk per input string");

        // Order by seq and assert each vector has the fixed dimension.
        chunks.sort_by_key(|(seq, _)| *seq);
        assert_eq!(chunks[0].0, 0, "first chunk seq");
        assert_eq!(chunks[1].0, 1, "second chunk seq");
        for (_, vector) in &chunks {
            assert_eq!(vector.len(), EMBEDDING_DIM, "embedding dimension");
        }

        // Terminal Final arrived with the matching chunk count.
        assert_eq!(
            final_count,
            Some(2),
            "expected a terminal Final with output_chunk_count == 2"
        );

        // Determinism: re-running the same worker against the same "hello"
        // input must reproduce the exact vector emitted above (seq 0).
        let spec2 = JobSpec::Embedding(EmbeddingJobSpec {
            model_cid: "embed-test".to_string(),
            input: vec!["hello".to_string()],
        });
        let manifest2 = ManifestBuilder::new(spec2).sign_with(&client).unwrap();
        let (_handle2, mut stream2) = worker.execute(manifest2).await.unwrap();

        let mut hello_again: Option<Vec<f32>> = None;
        while let Some(ev) = stream2.next().await {
            if let JobEvent::Output(chunk) = ev {
                hello_again = Some(serde_json::from_slice(&chunk.data).expect("decode Vec<f32>"));
            }
        }
        assert_eq!(
            hello_again.as_ref(),
            Some(&chunks[0].1),
            "embedding for the same input must be deterministic"
        );
    }

    /// Empty `input` must still produce a valid stream: zero Output chunks and
    /// a terminal Final whose commitment is over zero chunks — no panic.
    #[tokio::test]
    async fn embedding_job_empty_input_still_finalizes() {
        let client = NodeIdentity::generate();
        let spec = JobSpec::Embedding(EmbeddingJobSpec {
            model_cid: "embed-test".to_string(),
            input: vec![],
        });
        let manifest = ManifestBuilder::new(spec).sign_with(&client).unwrap();

        let worker = EchoWorker::new();
        let (_handle, mut stream) = worker.execute(manifest).await.unwrap();

        let mut output_count = 0usize;
        let mut saw_final = false;
        while let Some(ev) = stream.next().await {
            match ev {
                JobEvent::Output(_) => output_count += 1,
                JobEvent::Final { result, .. } => {
                    saw_final = true;
                    assert_eq!(result.output_chunk_count, 0, "zero chunks committed");
                }
                _ => {}
            }
        }
        assert_eq!(output_count, 0, "empty input emits no Output chunks");
        assert!(saw_final, "empty input still produces a terminal Final");
    }
}
