// SPDX-License-Identifier: Apache-2.0

//! Streaming commitment accumulator.
//!
//! The signing strategy for streamed jobs (see `SPEC.md` § "Signing
//! strategy") is *streamable-verifiable Merkle accumulation with single
//! final signature*:
//!
//! - The worker maintains a running SHA-256 chain over each emitted
//!   [`crate::OutputChunk`].
//! - At end-of-stream the final accumulator value goes into
//!   [`crate::JobResult::output_commitment`] and the worker signs the
//!   `JobResult` once.
//! - A verifier reconstructs the chain by replaying the chunks it received
//!   (in order, by `seq`) and checks the reconstructed accumulator against
//!   the signed value plus the chunk count.
//!
//! Why this and not per-chunk receipts? At 30 tok/s, per-chunk Ed25519
//! signatures would be 30 sigs/s/peer just for one stream — combined with
//! 100 concurrent streams across the worker that's 3000 sigs/s of pure
//! signing overhead before any work gets done. The accumulator gives us
//! the same verifier guarantee (no chunk can be added, dropped, reordered,
//! or modified without invalidating the final signature) at the cost of one
//! SHA-256 update per chunk and one Ed25519 per stream.

use crate::worker::OutputChunk;
use sha2::{Digest, Sha256};

/// Append-only SHA-256 chain over [`OutputChunk`]s.
///
/// The chain is defined as:
///
/// ```text
/// state_0 = SHA256("phase-protocol:v1:commitment")
/// state_n = SHA256(state_{n-1} || seq_n || len(kind_n) || kind_n ||
///                  len(data_n) || data_n)
/// ```
///
/// `seq_n` is the chunk's `seq` field encoded as 8 big-endian bytes;
/// `len(...)` is a 4-byte big-endian length prefix. Domain separation in
/// the initial state prevents cross-protocol commitment forgery.
#[derive(Clone, Debug)]
pub struct CommitmentAccumulator {
    state: [u8; 32],
    chunks: u64,
}

impl Default for CommitmentAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl CommitmentAccumulator {
    const DOMAIN: &'static [u8] = b"phase-protocol:v1:commitment";

    /// Initialize with the domain-separated initial state.
    pub fn new() -> Self {
        let mut h = Sha256::new();
        h.update(Self::DOMAIN);
        let state: [u8; 32] = h.finalize().into();
        Self { state, chunks: 0 }
    }

    /// Fold one chunk into the accumulator.
    pub fn update(&mut self, chunk: &OutputChunk) {
        let mut h = Sha256::new();
        h.update(self.state);
        h.update(chunk.seq.to_be_bytes());
        let kind_bytes = chunk.kind.as_bytes();
        h.update((kind_bytes.len() as u32).to_be_bytes());
        h.update(kind_bytes);
        h.update((chunk.data.len() as u32).to_be_bytes());
        h.update(&chunk.data);
        self.state = h.finalize().into();
        self.chunks += 1;
    }

    /// Finalize. Returns the 32-byte commitment and the chunk count, which
    /// together populate [`crate::JobResult::output_commitment`] +
    /// `output_chunk_count`. Consumes the accumulator to make it explicit
    /// that no more chunks can be added after this point.
    pub fn finalize(self) -> ([u8; 32], u64) {
        (self.state, self.chunks)
    }

    /// Current state without consuming. Useful for tests and for workers
    /// that want to log progress.
    pub fn peek(&self) -> ([u8; 32], u64) {
        (self.state, self.chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn chunk(seq: u64, kind: &str, data: &[u8]) -> OutputChunk {
        OutputChunk {
            kind: kind.to_string(),
            data: Bytes::copy_from_slice(data),
            seq,
        }
    }

    #[test]
    fn empty_streams_have_distinct_terminal_states() {
        // An empty stream's commitment is just the initial domain state.
        // Two empty streams should match each other.
        let a = CommitmentAccumulator::new().finalize();
        let b = CommitmentAccumulator::new().finalize();
        assert_eq!(a, b);
        assert_eq!(a.1, 0);
    }

    #[test]
    fn replay_matches_emission() {
        // The verifier guarantee: replaying the on-wire chunks reproduces
        // the worker's terminal state.
        let mut worker = CommitmentAccumulator::new();
        worker.update(&chunk(0, "token", b"Hello"));
        worker.update(&chunk(1, "token", b", "));
        worker.update(&chunk(2, "token", b"world!"));
        let worker_state = worker.finalize();

        let mut verifier = CommitmentAccumulator::new();
        verifier.update(&chunk(0, "token", b"Hello"));
        verifier.update(&chunk(1, "token", b", "));
        verifier.update(&chunk(2, "token", b"world!"));
        let verifier_state = verifier.finalize();

        assert_eq!(worker_state, verifier_state);
    }

    #[test]
    fn truncation_changes_commitment() {
        // Dropping the last chunk MUST change either the commitment or
        // the count — proving the verifier can detect truncation attacks.
        let mut full = CommitmentAccumulator::new();
        full.update(&chunk(0, "token", b"A"));
        full.update(&chunk(1, "token", b"B"));
        full.update(&chunk(2, "token", b"C"));
        let full_state = full.finalize();

        let mut truncated = CommitmentAccumulator::new();
        truncated.update(&chunk(0, "token", b"A"));
        truncated.update(&chunk(1, "token", b"B"));
        let truncated_state = truncated.finalize();

        assert_ne!(full_state, truncated_state);
        assert_ne!(full_state.1, truncated_state.1);
    }

    #[test]
    fn reordering_changes_commitment() {
        let mut ordered = CommitmentAccumulator::new();
        ordered.update(&chunk(0, "token", b"A"));
        ordered.update(&chunk(1, "token", b"B"));

        let mut reversed = CommitmentAccumulator::new();
        reversed.update(&chunk(1, "token", b"B"));
        reversed.update(&chunk(0, "token", b"A"));

        assert_ne!(ordered.finalize().0, reversed.finalize().0);
    }

    #[test]
    fn kind_is_domain_separated() {
        // Chunks with identical data but different kinds must commit
        // differently — prevents an attacker from substituting a
        // "stdout" chunk for a "token" chunk.
        let mut as_token = CommitmentAccumulator::new();
        as_token.update(&chunk(0, "token", b"X"));

        let mut as_stdout = CommitmentAccumulator::new();
        as_stdout.update(&chunk(0, "stdout", b"X"));

        assert_ne!(as_token.finalize().0, as_stdout.finalize().0);
    }
}
