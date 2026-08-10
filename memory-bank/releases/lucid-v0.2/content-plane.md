# Milestone: Content Plane

**Status:** 🟦 In progress — implementation checkpoint approved; physical acceptance pending
**Track:** Intended stream
**Blocking:** Yes
**Tracker:** [README.md](./README.md)

## Outcome

A LUCID node can resolve a human model alias through the network, download the exact advertised model content with resumable progress, verify every completed artifact, install it atomically, and advertise only the verified CID it can actually serve.

This milestone closes the deliberate v0.1 shortcut documented at `memory-bank/plans/security-hardening/SEC-13-content-addressed-cid.md:1-30`. Today `ModelCid::from_model_id` hashes the alias rather than the model bytes (`crates/lucidd/src/registry.rs:137-159`), `/api/pull` only registers a pre-existing local file (`crates/lucidd/src/ollama.rs:1095-1157`), and name lookup falls back to that placeholder (`crates/lucidd/src/registry.rs:602-635`).

## User-visible success

From a node with no local model file:

```bash
curl http://127.0.0.1:11434/api/pull \
  -d '{"name":"<published-model-alias>","stream":true}'
```

must produce honest, incremental progress; fetch content from a discovered provider; verify it; atomically install it; register the verified model; and finish with a success response. A subsequent `/api/show`, `/api/tags`, and inference request must refer to the same verified CID.

## Reuse and integration points

| Responsibility | Extend | Required behavior |
|---|---|---|
| Ollama API contract | `crates/lucidd/src/ollama.rs:1081-1160` | Preserve safe name handling and streaming/non-streaming response modes; replace the stub internals |
| CID type and advertisements | `crates/lucidd/src/registry.rs:112-159`, `crates/lucidd/src/registry.rs:230-343` | Derive CID from verified content or a canonical verified bundle root |
| Alias lookup | `crates/lucidd/src/registry.rs:602-635` | Resolve signed network alias records; remove the placeholder from production routing after migration |
| DHT seam | `crates/lucidd/src/registry.rs:345-380`, `crates/lucidd/src/dht_transport.rs:24-58` | Reuse the existing `DhtTransport`; do not create another DHT client |
| Blob storage | `crates/phase-artifact-server/src/artifacts.rs:97-109` | Extend the generic content-addressed store and atomic installation semantics |
| Provider lookup | `crates/phase-artifact-server/src/dht.rs:93-101` | Reuse the generic blob namespace or version it through an approved ADR |
| Transfer | `crates/phase-artifact-server/src/server.rs:356-477`, `crates/phase-artifact-server/src/server.rs:510-545` | Reuse byte-range serving and content-address path validation |
| Worker loading | `crates/lucidd/src/worker_llama.rs:76-144` | Resolve verified installed content instead of constructing an unverified alias path |

## Required decisions before implementation stabilizes

- [ ] **CID construction ADR:** choose whole-file SHA-256 or a canonical chunk/Merkle-root representation. Specify domain separation, canonical byte encoding, chunk size if applicable, and how algorithm upgrades are represented.
- [ ] **Model bundle ADR:** define whether one CID identifies a single GGUF or a manifest covering weights, tokenizer, templates, adapters, and backend-specific files.
- [ ] **Alias-record ADR:** define normalization, signature, publisher identity, sequence/version, TTL, expiry, conflict handling, rollback protection, and how clients present competing publishers.
- [ ] **Provider-record ADR:** define how a verified CID maps to one or more fetch endpoints without trusting an unsigned URL.
- [ ] **Local-store ADR:** define staging paths, atomic commit, permissions, partial-download metadata, quota, garbage collection, and crash recovery.

No alias or key prefix proposed during implementation becomes normative until recorded in the ADR. Existing v0.1 keys remain readable only under an explicit compatibility policy.

## Work packages

### Content identity

- [ ] Add a streaming hasher that never requires loading a multi-gigabyte model into RAM.
- [ ] Add canonical CID parsing/display with strict length and algorithm validation.
- [ ] Make content-derived construction the only path accepted for newly installed production models.
- [ ] Preserve a clearly named legacy parser/lookup only if rolling upgrades require it; legacy results must be marked unverified and must not silently outrank verified records.
- [ ] Bind advertised format, size, and bundle metadata to the signed record.

### Signed name resolution

- [ ] Define the signed alias payload and canonical signed bytes.
- [ ] Publish alias records only after local content verification succeeds.
- [ ] Query and verify all candidate records, rejecting invalid signatures, expired records, impossible sizes/formats, and rollback attempts.
- [ ] Return conflicts as structured alternatives with publisher identity rather than choosing an arbitrary record.
- [ ] Support operator or user pinning of publisher identity and/or exact CID.
- [ ] Cache verified alias answers only through their expiry and invalidate them on a higher accepted sequence.

### Provider discovery

- [ ] Advertise fetch capability separately from “model currently loaded for inference.”
- [ ] Verify that a provider record is attributable to its libp2p identity.
- [ ] Try multiple providers with bounded retries and preserve progress when switching.
- [ ] Prefer local content, then healthy LAN/WAN providers according to an explicit policy; never prefer an unverified source because it is faster.

### Resumable transfer

- [ ] Fetch into a staging area, never the final model path.
- [ ] Use HTTP range requests or the approved equivalent and validate range responses.
- [ ] Persist enough partial state to resume safely after process restart.
- [ ] Re-hash resumed bytes; never trust cached partial hashes without authenticated state.
- [ ] Bound concurrent pulls, total staging bytes, per-model size, idle time, and total duration.
- [ ] Propagate cancellation from the HTTP client and leave either a valid resumable partial or no partial.
- [ ] On provider failure, resume from another provider only if it serves the same CID.

### Atomic install and registration

- [ ] Verify final size and CID before rename/commit.
- [ ] `fsync`/atomic-rename according to platform guarantees documented in the local-store ADR.
- [ ] Refuse to overwrite different verified content under an existing alias without explicit conflict resolution.
- [ ] Register the model and publish provider/alias records only after atomic install.
- [ ] Roll back registry state if model probing or backend load validation fails.
- [ ] Expose CID, format, size, publishers, and verification state through `/api/show` and operational logs without leaking filesystem paths.

### Pull API behavior

- [ ] Preserve Ollama-compatible NDJSON progress for streaming requests.
- [ ] Define stable progress phases: resolving, selecting provider, downloading, verifying, installing, registering, success.
- [ ] Include byte totals only when known and never report 100% before verification and install complete.
- [ ] For `stream:false`, return one terminal JSON object with the verified CID.
- [ ] Map resolution, transfer, quota, verification, conflict, and install errors to distinct actionable responses.
- [ ] Sanitize all attacker-controlled aliases and provider data before logging, extending `pull_name_is_safe` at `crates/lucidd/src/ollama.rs:1081-1093` rather than bypassing it.

## Invariants

1. Same verified bytes and canonical metadata produce the same CID on every supported platform.
2. Different bytes cannot be installed under the same verified CID.
3. Human aliases are mutable references; CIDs are immutable identities.
4. A signed alias record proves who made the mapping, not that the alias is globally canonical.
5. Unverified bytes are never visible to workers through the final content store.
6. Registry advertisement cannot precede verification and atomic install.
7. A failed or cancelled pull cannot corrupt an already-installed model.
8. Provider changes during resume cannot change the target CID.

## Failure and security tests

- [ ] Whole-file and chunk-boundary test vectors are identical on macOS ARM64 and Linux x86_64/ARM64.
- [ ] One-bit payload mutation fails final verification.
- [ ] Tampered signed alias record, provider record, metadata, and signature are rejected.
- [ ] Expired, replayed, lower-sequence, oversized, and conflicting alias records exercise the approved policy.
- [ ] Wrong `Content-Range`, truncated range, overlapping range, ignored range, decompression bomb if compression is allowed, and slowloris provider are bounded.
- [ ] Interrupted pulls resume after HTTP disconnect, provider loss, daemon restart, and disk-pressure cleanup.
- [ ] Concurrent pulls of the same CID converge on one installed artifact without corruption.
- [ ] Disk-full, permission, atomic-rename, checksum, registry-publication, and backend-probe failures leave consistent state.
- [ ] Path traversal, absolute paths, separators, `..`, leading flags, control characters, Unicode-confusable aliases, and log injection are rejected or normalized per ADR.
- [ ] A node with no local name mapping resolves a remote signed alias and finds providers.
- [ ] Legacy placeholder records never outrank verified content records.

## Acceptance criteria

- [ ] `/api/pull` downloads a real multi-gigabyte model from another Phase node.
- [ ] Pull resumes across at least one provider disconnect and one `lucidd` restart.
- [ ] Final CID is independently recomputed on both nodes and matches.
- [ ] Tampered content is rejected before installation, registration, or execution.
- [ ] A third node with no prior model knowledge resolves the alias and exact CID through the DHT.
- [ ] Alias conflicts surface publisher identity and require the approved selection policy.
- [ ] Installed content can be served by the existing artifact-server path with bounded memory and valid range semantics.
- [ ] All new code has unit, integration, and real two-node coverage; workspace QA remains clean.
- [ ] SEC-13 acceptance criteria are satisfied and the deferred record can be closed with evidence.

## Explicit non-goals

- Hosting or curating a central model catalog.
- Declaring one publisher’s alias globally authoritative without a governance decision.
- Downloading arbitrary URLs supplied by an unauthenticated request.
- Conflating “artifact available” with “model loaded and capacity available.”
- Solving licensing policy for every model; the protocol must carry provenance/license metadata without pretending to adjudicate it.

## Completion evidence required in the tracker

- Approved CID, alias-record, and local-store ADR links.
- Test vectors and QA logs.
- A recorded interrupted/resumed multi-gigabyte pull.
- A tamper-rejection recording/log.
- Exact commit/PR and task documentation.

## 2026-08-09 approved implementation checkpoint

- Implemented `ContentPlane` with bounded pull coordination, exact-CID/publisher selection, resumable staging, verification-before-commit, catalog restore, and rollback-safe registry exposure at `crates/lucidd/src/content.rs:508-986`.
- Replaced name-derived production identity with content-derived CIDs plus signed alias/provider records, expiry, sequence, conflict, replay, and durable checkpoint handling in `crates/lucidd/src/registry.rs`.
- Extended the existing Phase artifact store with streaming verification, staged commit, same-CID convergence, symlink/path rejection, and content-addressed lookup in `crates/phase-artifact-server/src/artifacts.rs`.
- Automated unit/integration/adversarial coverage passed on macOS ARM64 and native Ubuntu x86_64, including two discovery nodes resolving signed content, resumable transfer, tamper rejection, catalog rollback, and bounded quota/concurrency behavior.
- Remaining before completion: real multi-gigabyte cross-machine pull, daemon-restart resume, third-node cold alias resolution, physical provider failover, and the full SEC-13 acceptance record.
- Evidence and exact fingerprint: [2026-08-09 checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
