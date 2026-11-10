# LUMEN — Independent Diffusion Node Release

**Status:** PLANNED / PRE-VERSION
**Version:** Unassigned — owner decision required
**Tracker role:** This README is the authoritative progress tracker for the LUMEN build.
**Last tracker update:** 2026-08-08

LUMEN is Phase’s diffusion and image-synthesis flagship. It is a separate node implementation, daemon, API surface, worker, policy domain, and release—not a mode inside `lucidd`. It reuses the workload-neutral Phase substrate and validates the content-addressed artifact half of that architecture.

That boundary is already accepted at `memory-bank/decisions.md:854-884`: LUCID streams autoregressive tokens and speaks Ollama; LUMEN produces large image/video artifacts, uses diffusion-native client contracts, and stresses content distribution. The generic extension points already exist in `crates/phase-protocol/src/job_spec.rs:35-54` and `crates/phase-protocol/src/worker.rs:315-378`.

No LUMEN implementation work belongs in `crates/lucidd/`. Shared improvements belong only in `phase-*` crates when they remain genuinely workload-neutral.

---

## Progress Tracker

### Status legend

| Marker | State | Meaning |
|---|---|---|
| ⬜ | Not started | No implementation has been approved or begun |
| 🟦 | In progress | Approved work is active; evidence is linked below |
| 🟨 | Blocked | A named dependency or decision prevents progress |
| 🟪 | Review | Implementation and QA are complete; human approval pending |
| ✅ | Complete | Acceptance criteria passed and completion was approved |
| ⏭️ | Deferred | Explicitly removed from the release by an approved decision |

### Build status

| Order | Milestone | Status | Depends on | Completion evidence |
|---:|---|---|---|---|
| 1 | [Product Contract and Compatibility Wedge](./product-contract-and-compatibility-wedge.md) | ⬜ Not started | Accepted separate-node ADR | — |
| 2 | [Protocol Contract](./protocol-contract.md) | ⬜ Not started | Product contract | — |
| 3 | [Artifact Plane](./artifact-plane.md) | ⬜ Not started | Product and protocol contracts; LUCID Content Plane decisions where generic | — |
| 4 | [Local Diffusion Worker](./local-diffusion-worker.md) | ⬜ Not started | Protocol contract and artifact plane | — |
| 5 | [Client API and Preview Stream](./client-api-and-preview-stream.md) | ⬜ Not started | Product contract and local worker | — |
| 6 | [Discovery, Routing, and Policy](./discovery-routing-and-policy.md) | ⬜ Not started | Protocol, artifact, and local worker | — |
| 7 | [Intended Image Flow](./intended-image-flow.md) | ⬜ Not started | Milestones 1–6 | — |
| 8 | [Distributed Diffusion Research Gate](./distributed-diffusion-research-gate.md) | ⬜ Not started | Local flow and LUCID trust/sharding research | — |
| 9 | [Release Qualification](./release-qualification.md) | ⬜ Not started | All required milestones | — |

**Overall:** 0 of 9 milestones complete.
**Local flagship gate:** 0 of 7 complete.
**Distributed research gate:** Not started and non-blocking unless later promoted by an approved scope decision.
**Release qualification:** Not started.

---

## Product Promise

The first LUMEN release should prove this exact path:

```text
Diffusion-native client
  → submits a graph/prompt through the approved compatibility API
  → LUMEN validates policy, resources, model bundle, and inputs
  → resolves every referenced model/adapter/input by verified content identity
  → executes locally through the selected diffusion backend
  → streams bounded progress and optional previews
  → commits the final image/video artifact to the Phase blob store
  → returns a signed receipt plus verified artifact reference and metadata
  → a client fetches the exact output through range-capable content serving
```

Remote routing follows only after the local path is correct. Distributed denoise-step, conditioning, or model sharding remains research-gated because partial floating-point computation is not cheaply verifiable (`memory-bank/decisions.md:890-914`).

## Success boundaries

### Required for the first LUMEN release

- A chosen native compatibility wedge backed by real client testing.
- A workload-neutral Phase protocol extension for image-generation jobs.
- Verified model/input/output artifact identity and provenance.
- At least one real local diffusion backend.
- Bounded progress and preview streaming.
- Policy/resource enforcement appropriate to large GPU and disk workloads.
- Signed receipt bound to job and final artifact commitment.
- Local and remote single-worker flows through Phase discovery/routing.
- Clean-machine installation, operation, recovery, and documentation.

### Research-only unless separately approved

- Multi-peer distributed diffusion.
- Cross-peer denoise-step splitting, conditioning parallelism, or model sharding.
- Probabilistic verification claims based on perceptual similarity.
- Video-generation support beyond artifact/protocol forward compatibility.

### Explicit non-goals

- Adding diffusion endpoints or workers to `lucidd`.
- Reusing Ollama as the diffusion API simply because LUCID already has it.
- Building a generic “media daemon” that combines unrelated workloads.
- Central model hosting, payments, KYC, tokens, blockchain, or a marketplace.
- Training/fine-tuning in the first release unless explicitly promoted through a new plan.
- Declaring LUMEN production-trustworthy for distributed partial computation without an accepted verification decision.

---

## Architecture Contract

```text
Native diffusion client/API
          │
          ▼
      LUMEN edge
  request translation · validation · output retrieval
          │
          ▼
   LUMEN scheduler/policy
 local worker or eligible Phase peer
          │
          ▼
    Diffusion Worker
 backend adapter · cancellation · progress · previews
          │
          ├──── Phase protocol: SignedManifest<JobSpec> / JobEvent / receipt
          ├──── Phase net: identity / discovery / encrypted transport
          └──── Phase artifact server: models / inputs / final outputs
```

### Shared substrate reused unchanged where possible

| Capability | Existing home | LUMEN use |
|---|---|---|
| Identity | `crates/phase-identity/src/keypair.rs:27-100`, `crates/phase-identity/src/storage.rs:25-99` | Stable PeerId and receipt signer |
| Signed request | `crates/phase-manifest/src/manifest.rs:42-120` | Signed image-generation job manifest |
| Workload contract | `crates/phase-protocol/src/job_spec.rs:35-54` | New approved job variant and kind |
| Worker stream | `crates/phase-protocol/src/worker.rs:55-88`, `crates/phase-protocol/src/worker.rs:315-378` | Progress, previews/artifact references, terminal result |
| Output commitment | `crates/phase-protocol/src/commitment.rs:29-86` | Bind committed output chunks/references |
| Signed receipt | `crates/phase-receipt/src/receipt.rs:40-120` | Attribute completion to worker identity |
| Discovery/transport | `crates/phase-net/src/discovery.rs:88-129`, `crates/phase-net/src/discovery.rs:210-351` | Capability discovery and encrypted peer path |
| Artifact store | `crates/phase-artifact-server/src/artifacts.rs:87-109`, `crates/phase-artifact-server/src/artifacts.rs:278-323`, `crates/phase-artifact-server/src/server.rs:188-220` | Model, input, preview where retained, and final-output blobs |

### Product-specific components that belong in LUMEN

- Diffusion request/job schema fields and validation.
- Backend adapters and model lifecycle.
- Native client compatibility API.
- Diffusion capability advertisements and scheduling policy.
- GPU/VRAM/step/resolution/batch safety limits.
- Preview/output encoders and artifact metadata.
- LUMEN operator/consumer documentation.

## Decisions that must precede implementation

- Product version and milestone IDs.
- Public product-name/trademark clearance; the accepted ADR notes Unreal Engine’s “Lumen” name at `memory-bank/decisions.md:881-884`.
- LUMEN application license.
- Compatibility wedge and exact first supported client workflow.
- First backend and supported model families/formats.
- `ImageGenJobSpec`/result contract, chunk kinds, and protocol-version impact.
- Model/input/output artifact bundle schemas and provenance.
- API authentication, remote-routing UX, content policy boundary, and operator controls.
- Preview format/rate/size and whether previews are commitment-covered or informational.
- Remote capability/registry schema.
- Distributed diffusion go/defer criteria.

---

## Definition of Done

- Every required milestone is ✅ and has reproducible evidence.
- LUMEN exists as an independent daemon/product boundary; `lucidd` contains no diffusion code.
- A real native client completes a local image-generation job unmodified or with the precisely documented endpoint configuration.
- Every model, adapter, input, and final output used in the canonical flow has verified content identity and provenance metadata.
- Progress and preview streaming are bounded; client cancellation releases backend resources.
- Final output is stored atomically, fetchable by CID/range, and bound into the signed receipt.
- A remote single-worker job completes over Phase with signer/PeerId/manifest/output verification.
- Invalid content, oversized jobs, unsafe paths, malformed graphs/parameters, backend crashes, disk exhaustion, and peer loss fail safely.
- Clean-machine consumer and operator runbooks pass.
- Workspace tests, clippy, format, audit, release builds, license review, and claims review pass.
- Distributed diffusion is either explicitly research-only or promoted through a separately approved trust decision.

---

## Tracker Update Protocol

Use the same evidence discipline as the [LUCID v0.2 tracker](../lucid-v0.2/README.md#tracker-update-protocol): 🟦 when approved work starts, 🟨 with an exact blocker, 🟪 after QA when awaiting human review, and ✅ only after acceptance evidence and approval. Update totals and evidence links in this README during the build.

## Files

- [index.yaml](./index.yaml) — machine-readable release charter and gates
- [product-contract-and-compatibility-wedge.md](./product-contract-and-compatibility-wedge.md)
- [protocol-contract.md](./protocol-contract.md)
- [artifact-plane.md](./artifact-plane.md)
- [local-diffusion-worker.md](./local-diffusion-worker.md)
- [client-api-and-preview-stream.md](./client-api-and-preview-stream.md)
- [discovery-routing-and-policy.md](./discovery-routing-and-policy.md)
- [intended-image-flow.md](./intended-image-flow.md)
- [distributed-diffusion-research-gate.md](./distributed-diffusion-research-gate.md)
- [release-qualification.md](./release-qualification.md)
