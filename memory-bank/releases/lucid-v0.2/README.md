# LUCID v0.2 — Substrate, Reach, Trust, and Scale

**Status:** IMPLEMENTATION ACTIVE — 2026-08-09 checkpoint approved; 2026-08-10 external test topology approved; release acceptance incomplete
**Release:** LUCID v0.2
**Tracker role:** This README is the authoritative progress tracker for the build.
**Last tracker update:** 2026-08-10

LUCID v0.2 turns the v0.1 demonstration into a real public-network path: a fresh node can discover the network, resolve a human model name to verified content, pull that content, reach a serving peer through hostile NAT, receive tokens while they are generated, and verify the signed result. Trust, sharding, and Apple-native backends build on that vertical slice without obscuring it.

The release extends the workload-neutral Phase substrate. It must not move model-, token-, or Ollama-specific behavior into `phase-*` crates. That boundary is established at `memory-bank/MISSION.md:106-116`, `memory-bank/systemPatterns.md:28-52`, and `memory-bank/decisions.md:854-875`.

---

## Progress Tracker

### Status legend

| Marker | State | Meaning |
|---|---|---|
| ⬜ | Not started | No implementation has been approved or begun |
| 🟦 | In progress | Approved work is active; evidence is linked below |
| 🟨 | Blocked | A named dependency, decision, or environment gate prevents progress |
| 🟪 | Review | Implementation and QA are complete; awaiting human approval |
| ✅ | Complete | Acceptance criteria passed and completion was approved |
| ⏭️ | Deferred | Explicitly removed from this release by an approved decision |

### Build status

| Order | Milestone | Track | Status | Depends on | Completion evidence |
|---:|---|---|---|---|---|
| 1 | [Content Plane](./content-plane.md) | Intended stream | 🟦 In progress | LUCID v0.1.1, Phase artifact server | Content-derived CIDs, signed alias/provider records, resumable blob streams, verified atomic catalog/install paths, and adversarial tests implemented; real multi-GB/two-node acceptance remains. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 2 | [Live Relay Plane](./live-relay-plane.md) | Intended stream | 🟦 In progress | Existing batch relay and receipt verification | v2 live substreams, ordered frames, cancellation, shared admission/replay gates, receipt binding, and Linux stream-timing regression coverage implemented; physical two-peer timing/load acceptance remains. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 3 | [Reachability Plane](./reachability-plane.md) | Intended stream | 🟦 In progress | Existing bootstrap and `phase-net` swarm | Circuit relay server/client, AutoNAT, rendezvous client/optional-server surfaces, DCUtR integration, path observation, limits, and exact relay-loopback coverage implemented. Public rendezvous stays fail-closed pending hard registration quotas; real consumer-NAT/DCUtR matrix remains. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 4 | [Network Operations](./network-operations.md) | Intended stream | 🟦 In progress | Reachability design | Bounded infrastructure roles, service/config surface, validation automation, protected UMBP qualification, and the first independent DigitalOcean deployment topology are approved; provisioning, geographic fleet, clean-host operator drill, monitoring, and failure drills remain. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 5 | [Intended-Stream Vertical Slice](./intended-stream-vertical-slice.md) | Integration gate | 🟨 Blocked | Content, live relay, reachability, operations | Component and loopback flows pass, but the required physical two-network/NAT run, real model pull/resume, and relay-only remote inference recording have not run. |
| 6 | [Reputation and Redundant Verification](./reputation-and-redundant-verification.md) | Trust | 🟦 In progress | Verified receipts and stable peer identity | Private append-only evidence, validation/compaction, cold-start/decay, operator precedence, truthful taxonomy, bounded deterministic redundancy, and adversarial unit tests implemented; multi-peer load and failure-injection acceptance remains. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 7 | [ShardWorker](./shardworker.md) | Scale / research-gated | 🟨 Blocked | Reputation plus partial-compute verification decision | No production worker exists. The approved partial-tensor verification ADR and reproducible research gate remain unresolved; no sharding claim is made. |
| 8 | [MLX Backend](./mlx-backend.md) | Backend / parallel | 🟦 In progress | Content-plane format metadata; Apple Silicon rig | Pinned-bundle subprocess worker, streaming parser, cancellation/invalidation, receipt integration, resource bounds, mutation checks, and non-Apple fail-closed behavior implemented; real Apple Silicon model acceptance remains. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |
| 9 | [Core AI Feasibility Gate](./core-ai-feasibility-gate.md) | Backend research / non-blocking | ⬜ Not started | MLX findings; public KV-cache evidence | — |
| 10 | [Release Qualification](./release-qualification.md) | Release gate | 🟦 In progress | All required milestones | macOS ARM64 workspace QA and release bundle pass; native Ubuntu x86_64 release/tests/Clippy and isolated HTTP smoke pass for source `1356ef65…d847`. Linux ARM64, full interoperability/NAT, real MLX, rollback, and clean-host matrices remain. See [checkpoint task](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md). |

**Overall:** 0 of 10 milestones complete; 7 in progress; 2 blocked; 1 not started.
**Intended-stream gate:** 0 of 5 complete; four component milestones are in progress and the physical vertical slice is blocked on real-network execution.
**Trust-and-scale gate:** Reputation is in progress; ShardWorker is blocked at its verification research/ADR gate.
**Backend gate:** MLX implementation is in progress pending real Apple Silicon acceptance; Core AI remains a non-blocking, not-started research gate.
**Release qualification:** In progress; macOS ARM64 and Linux x86_64 evidence exist, but the complete release matrix is not satisfied.

### Approved implementation checkpoint — 2026-08-09

- Approved source fingerprint: `1356ef6520ce7ad7dab6369ed40e50cd7507bfff570df87f1881db41fbb7d847`.
- Local macOS ARM64: 460 tests passed, 2 hardware-only tests ignored; strict Clippy, formatting, and diff checks passed.
- UMBP native Ubuntu x86_64: optimized all-target build passed; 450 tests passed, 2 hardware-only tests ignored; strict Clippy passed.
- Isolated UMBP HTTP smoke on `127.0.0.1:11435`: version, tags, non-stream generation, token streaming, embeddings, truthful routing/receipt headers, teardown, and production-relay isolation passed.
- Security: zero known vulnerabilities; four allowed unmaintained transitive warnings; `cargo deny` bans/licenses/sources passed; no high/critical review finding; secret and incomplete-marker scans passed.
- Evidence is stored under `target/phase-validation/linux-x86_64-umbp/1356ef6520ce7ad7dab6369ed40e50cd7507bfff570df87f1881db41fbb7d847/20260809T183940Z-634076/` and summarized in the approved [task record](../../tasks/2026-08/260809_lucid-v0.2-implementation-umbp-qualification.md).
- This checkpoint approves the implementation and documentation update. It is not a release approval and does not waive unchecked milestone acceptance criteria.

### Approved physical test topology — 2026-08-10

```text
External-network requester
        │ Phase/libp2p TCP 4001
        ▼
DigitalOcean foundation relay/bootstrap
        │ circuit relay; DCUtR where possible
        ▼
Pip M1/16-GB Apple Silicon contributor behind consumer NAT
```

- The DigitalOcean node runs the bounded infrastructure `lucidd` locally behind a Reserved IPv4. The recommended starting envelope is 1 vCPU, 2 GB RAM, and 50 GB disk; it is a transport node, not an inference host.
- Native Phase traffic terminates on public TCP `4001`. TCP `80/443` terminates in Caddy and may reverse-proxy existing web origins to UMBP over Tailscale; it does not forward the Phase relay to the Sonic DHCP address.
- Tailscale remains the administrative/recovery plane. The acceptance data path must use Phase discovery/relay so Tailscale cannot accidentally satisfy the reachability criterion.
- UMBP is being snapshotted before an Ubuntu 26.04 LTS upgrade and must pass a post-upgrade service/network audit before reuse.
- This topology removes the missing-public-actor blocker once provisioned. The vertical slice remains blocked until real pull/resume, relay-only inference, first-token timing, cancellation, receipts, DCUtR/fallback, and resource evidence pass.
- Public rendezvous is not part of the first deployment: `lucidd` deliberately configures `rendezvous_server: None` pending hard global/per-peer/per-namespace quotas (`crates/lucidd/src/main.rs:756-765`). Configured relay/bootstrap and DHT paths remain testable.

### What can be worked in parallel

- Content Plane and Live Relay Plane may proceed in parallel after their wire-format decisions are recorded.
- Reachability Plane can proceed in parallel with both, because it extends `phase-net` behavior rather than the LUCID content schema.
- Network Operations starts once relay policy and advertised-address behavior are stable enough to deploy safely.
- MLX may proceed in parallel, but it must consume the Content Plane’s chosen model-format metadata instead of inventing a second registry.
- Reputation begins with evidence collection and storage design, but enforcement waits for stable live-relay receipt outcomes.
- ShardWorker cannot cross its design gate until partial-compute verification has an approved ADR.
- Core AI is research-only until its feasibility gate is passed; it cannot delay the intended stream.

---

## The Intended Stream

The earliest integrated checkpoint is deliberately user-visible:

```text
Fresh node
  → discovers foundation bootstrap records
  → reaches the Phase network from behind NAT
  → resolves "model-name" to a signed, verified content CID
  → pulls/resumes the exact model artifact
  → verifies bytes before registration or execution
  → routes an Ollama-compatible request to a capable peer
  → receives the first remote token before generation completes
  → verifies sequence, commitment, signed receipt, manifest, and serving PeerId
  → reports an attributable success or an actionable failure
```

This path is the release’s center of gravity. A collection of individually completed components does not count if this flow is not demonstrated across real machines and a real NAT boundary.

### Non-negotiable observable outcomes

1. `/api/pull` downloads bytes rather than registering a file that was already present.
2. A CID identifies content, not a human model alias.
3. A node that has never loaded a model can resolve its name through the network.
4. A model whose bytes do not match its advertised CID is rejected before use.
5. Peer-routed output reaches the client incrementally; it is not buffered until completion.
6. Client cancellation propagates to the serving worker and frees resources.
7. A NATed node can be reached through circuit relay and can upgrade to a direct path when DCUtR succeeds.
8. Every accepted remote result remains bound to the dispatched manifest and serving PeerId.
9. A fresh install has a documented, tested bootstrap path that does not require copying a peer multiaddr by hand.
10. Failures distinguish discovery, transfer, verification, policy, capacity, execution, and receipt-validation errors.

---

## Release Boundaries

### Required for the LUCID v0.2 release gate

- Content Plane
- Live Relay Plane
- Reachability Plane
- Network Operations
- Intended-Stream Vertical Slice
- Reputation and Redundant Verification
- ShardWorker’s approved v0.2 outcome as defined in its milestone: verified experimental implementation, or explicit approved deferral if the research gate cannot be satisfied without false trust claims
- MLX Backend
- Release Qualification

### Non-blocking research gate

- Core AI Feasibility Gate. Passing it authorizes a later implementation plan; it does not silently add `CoreAIWorker` to this release.

### Explicitly outside this release

- Cryptographic prompt privacy, still targeted separately from the v0.2 substrate work (`README.md:48-49`).
- A payment rail, marketplace, token, or KYC system (`memory-bank/MISSION.md:43-50`).
- Foundation Models as an Apple backend (`memory-bank/decisions.md:938-950`).
- LUMEN or diffusion functionality inside `lucidd`; LUMEN has its own release package at `../lumen/`.
- TEE- or ZK-dependent trust claims without a separately approved decision and measured implementation.

---

## Architecture and Reuse Contract

| Concern | Existing extension point | v0.2 rule |
|---|---|---|
| Content storage and range serving | `crates/phase-artifact-server/src/artifacts.rs:97-109`, `crates/phase-artifact-server/src/server.rs:188-220`, `crates/phase-artifact-server/src/server.rs:356-477` | Extend the generic blob store and range-serving path; do not create a LUCID-only file server |
| Blob discovery | `crates/phase-artifact-server/src/dht.rs:93-101` and `crates/phase-net/src/discovery.rs:153-167` | Reuse generic DHT transport; keep model alias semantics in LUCID |
| Model registry | `crates/lucidd/src/registry.rs:112-159`, `crates/lucidd/src/registry.rs:602-635` | Replace the name-derived placeholder with content-derived identifiers and signed alias records |
| Pull API | `crates/lucidd/src/ollama.rs:1081-1160` | Refactor the stub handler into the real pull coordinator; preserve Ollama-compatible progress behavior |
| Worker stream | `crates/phase-protocol/src/worker.rs:315-378` | Carry existing `JobEvent` frames over the network; do not invent a token-only substrate protocol |
| Stream commitment | `crates/phase-protocol/src/commitment.rs:29-86` | Replay exactly the output chunks delivered to the client |
| Current batch relay | `crates/phase-net/src/protocol.rs:16-67`, `crates/phase-net/src/discovery.rs:281-306` | Introduce a versioned streaming path; retain an explicit compatibility policy for v0.1 peers |
| NAT and discovery | `crates/phase-net/src/discovery.rs:88-99`, `crates/phase-net/src/discovery.rs:210-351` | Extend `CombinedBehaviour` and the existing driver command/event loop |
| Policy | `crates/lucidd/src/policy.rs:69-164`, `crates/lucidd/src/policy.rs:262-383`, and `crates/lucidd/src/router.rs:590-825` | Attribute remote work, cap resources, and remain default-deny until the recorded trigger is satisfied |
| Apple backend | `crates/lucidd/src/worker_llama.rs:76-144`, `crates/lucidd/src/worker_llama.rs:464-472` | Mirror the existing `Worker` lifecycle and receipt semantics; backend differences stay inside LUCID |

No milestone may create a duplicate identity store, DHT client, artifact server, commitment mechanism, receipt type, or policy engine.

---

## Decision Gates

The following decisions must be written to `memory-bank/decisions.md` and approved before their corresponding code is considered stable:

- Content CID shape: whole-file hash versus chunk/Merkle root, including canonical encoding and algorithm agility.
- Signed name→CID record schema, normalization, conflict handling, TTL, and rollback behavior.
- Model bundle/format representation for GGUF, MLX, and future Apple-native formats.
- Streaming relay protocol identifier, framing, terminal receipt delivery, cancellation, and v0.1 compatibility.
- Foundation relay service policy: resource caps, reservation policy, abuse response, retention, and operator disclosure.
- Reputation evidence model, storage scope, decay, Sybil assumptions, and routing influence.
- Partial-compute verification strategy and tolerance model before untrusted sharding.
- MLX process boundary and supported deployment contract.
- Core AI go/no-go evidence on public stateful decode/KV-cache support.

---

## Definition of Done

The release is complete only when all of the following are true:

- Every required milestone is ✅ and links reproducible evidence.
- The intended stream succeeds on at least two physical nodes on different networks, including one node behind consumer NAT.
- The same scenario succeeds through a relay-only path; a separate run demonstrates DCUtR direct-path upgrade where the environment supports it.
- Pull interruption and resumption are tested on a multi-gigabyte model without corrupting the final store.
- Tampered chunks, wrong manifests, alias substitution, replayed/expired advertisements, out-of-order stream frames, missing receipts, and wrong-peer receipts are rejected.
- Cancellation, slow-client backpressure, serving-peer loss, relay loss, and local capacity exhaustion have deterministic outcomes and no leaked worker process or reservation.
- Reputation affects routing only through the approved policy, and its raw evidence remains inspectable.
- ShardWorker makes no claim stronger than the accepted partial-verification design permits.
- MLX passes real Apple Silicon inference and the same commitment/receipt checks as llama.cpp.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, format checks, dependency audit, and release builds pass.
- Operator and consumer runbooks are executable from a clean machine.
- Public README claims match the demonstrated behavior exactly.

---

## Tracker Update Protocol

This file is updated during the build, not reconstructed afterward.

When a milestone begins:

1. Change its marker to 🟦.
2. Add the approved branch/PR and task record under Completion evidence.
3. Record any decision gate still open in the milestone file.

When blocked:

1. Change its marker to 🟨.
2. Name the exact blocker and owner in the milestone file.
3. Do not substitute a weaker implementation and call it complete.

When ready for review:

1. Change its marker to 🟪.
2. Link test output, demo evidence, security review, and the proposed diff.
3. Wait for explicit approval before marking complete.

When approved:

1. Change its marker to ✅.
2. Add the merge commit/tag, task documentation, test totals, and artifact locations.
3. Recalculate the progress totals above.

The tracker must never mark a milestone complete based only on code existence.

---

## Files

- [index.yaml](./index.yaml) — machine-readable release scope, sequencing, and gates
- [content-plane.md](./content-plane.md)
- [live-relay-plane.md](./live-relay-plane.md)
- [reachability-plane.md](./reachability-plane.md)
- [network-operations.md](./network-operations.md)
- [intended-stream-vertical-slice.md](./intended-stream-vertical-slice.md)
- [reputation-and-redundant-verification.md](./reputation-and-redundant-verification.md)
- [shardworker.md](./shardworker.md)
- [mlx-backend.md](./mlx-backend.md)
- [core-ai-feasibility-gate.md](./core-ai-feasibility-gate.md)
- [release-qualification.md](./release-qualification.md)
- [LUMEN release tracker](../lumen/README.md) — separate diffusion flagship; not part of LUCID
