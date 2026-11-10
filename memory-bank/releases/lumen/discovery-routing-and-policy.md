# Milestone: Discovery, Routing, and Policy

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Protocol Contract, Artifact Plane, and Local Diffusion Worker
**Tracker:** [README.md](./README.md)

## Outcome

LUMEN advertises verifiable diffusion capability, discovers eligible single-worker peers, chooses local or remote execution under explicit policy, relays bounded progress/output references, and verifies the remote receipt/artifact. Worker operators retain control over GPU, content, resource, and availability policy.

LUMEN must reuse `phase-net` discovery/transport and Phase identity rather than importing `lucidd`’s inference-specific registry/router. Reusable DHT and routing seams may be extracted only if they are truly workload-neutral and preserve crate dependency direction.

## Capability schema decisions

Signed advertisements need the minimum scheduling facts:

- Supported LUMEN protocol/API/backend versions.
- Supported model/bundle CIDs or model-family/format capability.
- Maximum width/height/pixels, steps, batch/count, concurrent jobs, and output bytes.
- Supported operation subset (text-to-image, image-to-image, etc.).
- Backend/device class at a privacy-preserving level.
- Current capacity/queue hints, validity, and refresh.
- Reachable peer identity/address through Phase networking.

Advertisements are self-claims signed by the peer, not proof of GPU capacity or output correctness.

## Work packages

### Registry

- [ ] Define signed, versioned LUMEN capability advertisement canonical bytes.
- [ ] Publish one or more DHT records through existing `phase-net` record APIs.
- [ ] Refresh with bounded jitter and withdraw/expire when backend/model/capacity disappears.
- [ ] Decode, verify signature, expiry, PeerId binding, schema, and limits before routing.
- [ ] Keep human alias→bundle CID in the generic content/alias plane; capability records reference verified CIDs.
- [ ] Avoid placing high-churn exact telemetry into the DHT.

### Router

- [ ] Order decisions: request validation → operator policy → verified artifacts → local capacity → eligible remote peers → refusal.
- [ ] Prefer local execution according to explicit configuration, not silently.
- [ ] Filter peers by exact protocol, operation, bundle/content, limits, reachability, policy, and capacity.
- [ ] Apply reputation evidence only when the shared trust milestone and LUMEN-specific comparison semantics exist.
- [ ] Fail over only before committed preview/final output reaches the client unless a resume protocol is designed.
- [ ] Expose selected route, PeerId, backend class, and trust state to the API.

### Remote execution

- [ ] Reuse the versioned live peer stream from LUCID v0.2 where it remains generic over `JobEvent`.
- [ ] Bound request, progress, preview, artifact-reference, and receipt frames.
- [ ] Propagate client cancellation to the serving peer/worker.
- [ ] Verify manifest/job binding, signer/PeerId, event sequence, output commitment, chunk count, completion, and artifact CID.
- [ ] Fetch final output from an authenticated content provider and verify its CID; the relay need not carry full image bytes.
- [ ] Treat missing/invalid receipt or artifact as failure, not partial success.

### Operator policy

- [ ] Separate self-traffic from donated remote traffic.
- [ ] Default-deny remote work until operator opt-in or an approved future open policy.
- [ ] Cap resolution/pixels, steps, batch/count, duration, GPU memory, queue, concurrent jobs, preview/output bytes, input bytes, and artifact retention.
- [ ] Support manual pause and platform battery/thermal/interactive-use auto-pause where available.
- [ ] Define optional model/publisher/operation allow/deny policy without putting content moderation in relays.
- [ ] Verify submitter identity/authorization and clamp signed-manifest requests.
- [ ] Return attributable refusal categories without exposing sensitive configuration.

### Observability

- [ ] Record route, PeerId, content CIDs, operation class, durations, byte counts, completion, verification, and resource state.
- [ ] Do not log prompt text, input/output image bytes, masks, private artifact URLs/tokens, or keys.
- [ ] Surface queue/capacity, policy pause, model load, artifact fetch, preview, finalization, and verification states.

## Failure and adversarial tests

- Forged/expired/oversized/malformed capability record.
- Peer advertises content it does not possess or operation it refuses.
- Wrong model/bundle CID, wrong final artifact, wrong signer/PeerId/manifest/commitment.
- Peer loss before accept, during load, during previews, after final reference, and before receipt.
- Slow preview consumer, oversized preview, output-reference flood, artifact provider loss.
- Policy bypass attempts through backend-specific options, signed manifest limits, or self/remote traffic confusion.
- Queue/resource amplification and repeated expensive artifact pulls.
- Cancellation and cleanup on both nodes.

## Acceptance criteria

- [ ] Two physical LUMEN nodes discover signed eligible capability through Phase.
- [ ] Router chooses local or remote according to documented policy.
- [ ] Remote single-worker generation streams progress/previews and returns a verified final artifact/receipt.
- [ ] Serving operator caps and pause/refusal states are enforced.
- [ ] Invalid capability, result, receipt, or artifact is rejected.
- [ ] Client cancellation reaches the remote backend and frees resources.
- [ ] No LUMEN job bytes or policy semantics are interpreted by generic relays.
- [ ] Full multi-peer/workspace QA passes.

## Explicit non-goals

- Distributed multi-peer denoise/sharding.
- A universal/global reputation score.
- Relays inspecting or moderating image prompts/content.
- Importing `lucidd` as a library dependency.
- Treating capability advertisements as verified capacity.

## Completion evidence required in the tracker

- Approved advertisement/routing/policy ADRs.
- Two-node discovery and remote-generation recording.
- Receipt/artifact tamper and policy-bypass QA.
- Cancellation/resource cleanup evidence.
- Exact commit/PR and task documentation.
