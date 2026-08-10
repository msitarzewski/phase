# Milestone: Intended Image Flow

**Status:** ⬜ Not started
**Blocking:** Yes
**Depends on:** Product, Protocol, Artifact, Worker, API, and Routing milestones
**Tracker:** [README.md](./README.md)

## Outcome

Demonstrate LUMEN’s complete local and remote single-worker experience through the chosen native client, with verified artifacts, live progress/previews, cancellation, policy, and signed receipts. This is the product-proof milestone analogous to LUCID’s intended-stream vertical slice.

## Canonical actors

- **Client machine:** chosen unmodified diffusion-native client pointed at LUMEN.
- **Local LUMEN:** API edge and optionally a local worker.
- **Remote contributor:** separate LUMEN node with real backend/GPU behind a different network.
- **Content provider:** serves verified model/bundle/input artifacts.
- **Phase relay/bootstrap services:** provide discovery/reachability where direct access is unavailable.

## Canonical local flow

1. Install LUMEN from a clean supported machine.
2. Start with a persistent Phase identity and safe default configuration.
3. Configure the chosen client endpoint.
4. Resolve/install the approved model bundle by verified content identity.
5. Submit the canonical generation request.
6. Validate request, policy, resources, and every artifact before allocation.
7. Load the backend/model and report bounded progress.
8. Stream optional previews before completion.
9. Commit final output atomically into the Phase artifact store.
10. Emit receipt-bound artifact reference and client-compatible result.
11. Fetch and independently verify the final output CID.

## Canonical remote flow

1. Begin from a client/API node without a local diffusion worker capable of the request.
2. Discover the remote contributor and its signed capability.
3. Resolve all model/input artifact CIDs; ensure the contributor can acquire them under policy.
4. Route the signed job through Phase to the contributor.
5. Contributor validates submitter, policy, caps, artifacts, and capacity.
6. Progress/previews return live over the peer stream.
7. Final output is committed by an approved provider and referenced by CID.
8. Requester verifies event sequence, commitment, receipt, manifest, serving PeerId, and final artifact CID.
9. Chosen client displays/saves the result through its native workflow.

## Required happy-path runs

- [ ] Local canonical text-to-image flow through the unmodified client.
- [ ] Remote single-worker flow across different physical networks.
- [ ] Relay-only remote flow if the contributor is behind NAT.
- [ ] Preview-enabled and preview-disabled flow.
- [ ] Fixed-seed repeat on the same backend to characterize reproducibility.
- [ ] Multiple outputs if included in the approved feature envelope.
- [ ] Content pull/resume for at least one multi-gigabyte model bundle.
- [ ] Independent final artifact fetch and CID recomputation.

## Required failure-path runs

- [ ] Tampered/corrupt/missing model bundle or member.
- [ ] Invalid/oversized input media.
- [ ] Unsupported graph/node/option.
- [ ] Excessive resolution, pixel count, steps, batch, outputs, or duration.
- [ ] Policy/manual/thermal/battery refusal.
- [ ] Client cancellation during queue, load, denoise, preview, final encode, and artifact commit.
- [ ] Backend hang, crash, crash loop, and GPU memory exhaustion.
- [ ] Remote peer loss before output, during previews, after final reference, and before receipt.
- [ ] Wrong final artifact CID, wrong receipt signer/PeerId, wrong manifest, and commitment mismatch.
- [ ] Disk full during input staging and final output commit.
- [ ] Relay loss and artifact provider loss.

## Evidence contract

Capture synchronized sanitized evidence containing:

- Software commit/version, OS, architecture, backend/version, device class, model/bundle CIDs, client/version, and PeerIds.
- Request/job ID and operation class without raw prompt/input/output content.
- Policy/resource decision and caps.
- Artifact resolution, transfer, verification, and install state.
- Queue/load/first-preview/final-output/receipt timings.
- Route and relay/direct path.
- Output artifact CID, size, dimensions, MIME type, commitment/chunk count, receipt verification.
- Cancellation/failure cleanup state.

## Performance measurements

- Clean start and warm start.
- Model/bundle pull and verification throughput.
- Model load time and resident memory/VRAM.
- Queue time, first preview, final image, and total receipt time.
- Preview bandwidth and memory.
- Local versus remote/relay overhead.
- Artifact store/write/fetch throughput.
- Cancellation cleanup latency.
- Sustained repeated-job thermal/power behavior on designated hardware.

Thresholds are approved from measured baselines; they are not invented in advance.

## Acceptance criteria

- [ ] Chosen unmodified client completes both local and remote canonical flows.
- [ ] Remote contributor is a physical node on a different network.
- [ ] Every model/input/final artifact used by the flow is verified by content identity.
- [ ] Preview arrives before final completion and remains bounded.
- [ ] Final output bytes independently match the receipt-bound CID.
- [ ] Receipt binds exact job and serving PeerId.
- [ ] Cancellation and every listed failure produce safe, reproducible cleanup.
- [ ] User sees whether execution is local/remote and the remote privacy implication.
- [ ] Runbook and evidence contain no secrets or raw private content.
- [ ] Tracker links exact commits, configuration, evidence, and approved task record.

## Stop conditions

The milestone cannot pass if:

- A custom test UI substitutes for the selected compatibility client.
- Final output is returned from a staging/local path rather than the verified artifact store.
- Model or auxiliary files are assumed/pre-copied without content verification in the only demonstration.
- Preview output is replayed after completion but presented as live.
- Remote success skips receipt/PeerId/artifact verification.
- Cancellation leaves GPU work, process, queue entry, temp data, or artifact pin alive.
- Diffusion code resides in or depends on `lucidd`.

## Completion evidence required in the tracker

- Local/remote topology and sanitized evidence bundle.
- Exact client/backend/model/hardware matrix.
- Performance/resource report.
- Failure/cancellation checklist.
- Exact commit/PR and task documentation.
