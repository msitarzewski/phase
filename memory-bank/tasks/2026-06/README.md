# June 2026 — Task Summary

## Tasks Completed

### 2026-06-09: LUCID v0.1.1 gap-closure
- Embeddings end-to-end (`/api/embed` + legacy `/api/embeddings`), multi-peer relay failover,
  self-traffic policy path (`should_serve_self`), `/api/pull` stub, and a stale-docstring fix.
- `JobSpec::Embedding` rides the existing `OutputChunk` commitment/receipt machinery — no new
  result shape; SEC-05 receipt verify+bind covers embedding relays unchanged.
- 263 tests pass, clippy `-D warnings` clean, `cargo audit` 0 vulns. Merged via PR #10 (`14ae8d6`).
- Validated live on `scratch` (ARM64, cargo 1.96.0): build + 87 tests + the three new endpoints
  over HTTP against the echo worker.
- Files: `phase-protocol/job_spec.rs`, `lucidd/{router,policy,echo,worker_llama,ollama,main,registry}.rs`.
- See: [260609_lucid-v0.1.1-gaps.md](./260609_lucid-v0.1.1-gaps.md).

### 2026-06-09: Apple Core AI / Foundation Models research
- Recorded that Apple Foundation Models is a hard no for a LUCID backend (ToS + unenforceable AUP
  for a neutral relay); Core AI is the license-clean ANE path for our own models, a future
  efficiency backend after MLX (gated on KV-cache verification).
- See: [../../research/2026-06-09-apple-coreai-foundation-models.md](../../research/2026-06-09-apple-coreai-foundation-models.md)
  and `decisions.md` 2026-06-09.
