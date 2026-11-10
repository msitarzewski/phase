# Milestone: Product Contract and Compatibility Wedge

**Status:** ⬜ Not started
**Blocking:** Yes
**Tracker:** [README.md](./README.md)

## Outcome

Choose and validate the first LUMEN user workflow, client compatibility surface, backend, model families, product/license identity, and honest release boundary before protocol or daemon code solidifies around assumptions.

The accepted architecture requires LUMEN to speak its ecosystem’s native protocol instead of copying LUCID’s Ollama surface (`memory-bank/decisions.md:861-879`). Candidate ecosystems named in that decision include ComfyUI graphs, `diffusers`, and A1111-style APIs, but none has yet been selected.

## Required decisions

- [ ] Assign the first release version and milestone IDs.
- [ ] Complete product-name/trademark clearance, including the recorded Unreal Engine “Lumen” caveat (`memory-bank/decisions.md:881-884`).
- [ ] Choose and record the LUMEN application license.
- [ ] Select one primary compatibility wedge and a concrete unmodified client/version.
- [ ] Select the first local backend and supported backend version.
- [ ] Select a deliberately small model/format matrix for the first release.
- [ ] Define whether the first release supports text-to-image only or also image-to-image/inpainting.
- [ ] Define remote-routing disclosure, safety/policy boundary, and operator control expectations.

## Compatibility evaluation

Evaluate each serious candidate against the same matrix:

| Axis | Required evidence |
|---|---|
| Client adoption | Real clients/workflows the API unlocks |
| Request complexity | Prompt-only versus graph/workflow representation |
| Streaming | Progress/preview mechanism and cancellation |
| Artifact handling | Upload/reference of models, LoRAs, VAEs, ControlNet, source images, masks; output retrieval |
| Compatibility stability | Versioning and backward-compatibility expectations |
| Remote semantics | Whether local filesystem paths or plugin assumptions make peer execution unsafe/impossible |
| Security surface | Arbitrary nodes/plugins/scripts, path references, URL fetching, deserialization risks |
| Implementation size | Minimal complete subset versus accidental platform clone |
| Licensing | API/client/backend redistribution and integration constraints |
| Testability | Headless, deterministic-enough fixtures and real-client automation |

## Canonical use-case specification

Before exit, write one testable narrative containing:

- Named client and version.
- How the user configures the LUMEN endpoint.
- Exact supported operation and intentionally unsupported controls.
- Model and auxiliary artifacts required.
- Request fields, defaults, bounds, and validation errors.
- Progress and preview behavior.
- Cancellation behavior.
- Final artifact metadata and retrieval.
- Local-versus-remote disclosure.
- Receipt/provenance information exposed to the user.

## Backend evaluation

- [ ] Run at least two viable backend candidates or justify why only one qualifies.
- [ ] Measure startup, model load, first preview, total image, VRAM/RAM, disk, cancellation, crash behavior, and headless serviceability.
- [ ] Verify subprocess/API stability and licensing from primary upstream materials.
- [ ] Identify which input graph/features can be supported safely without arbitrary plugin execution.
- [ ] Confirm output bytes and metadata can be captured without scraping an interactive UI.
- [ ] Record supported GPU/runtime platforms honestly.

## Initial feature envelope

The first release should prefer a narrow complete flow. The decision must explicitly mark each item supported, deferred, or rejected:

- Text-to-image.
- Seeded generation.
- Width/height, steps, guidance, sampler/scheduler.
- Negative prompt.
- Batch size/count.
- Image-to-image and denoise strength.
- Inpainting and mask.
- LoRA, VAE, embeddings, ControlNet or other auxiliaries.
- Preview images.
- Multiple final images.
- Metadata embedding/sidecar.
- Model installation/pull.
- Remote routing.
- Video or animation.
- Arbitrary custom workflow nodes/plugins.

## Product safety and operator sovereignty

- [ ] Define which inputs are structural validation versus operator-configurable content policy.
- [ ] Keep relays content-neutral; execution policy belongs to the worker operator.
- [ ] Require explicit resource caps for resolution, pixels, steps, batch, concurrent jobs, upload bytes, output bytes, and duration.
- [ ] Define remote-job visibility honestly: serving peers can see prompts and input images unless a future privacy mechanism says otherwise.
- [ ] Define provenance fields without pretending they solve misuse or authenticity universally.

## Acceptance criteria

- [ ] Version/IDs, trademark, license, compatibility, backend, model, and feature-envelope decisions are approved.
- [ ] A real chosen client drives a proof-of-concept backend workflow.
- [ ] Unsupported client features fail clearly rather than being ignored silently.
- [ ] The canonical use case is executable and bounded.
- [ ] Remote filesystem/plugin assumptions are removed or explicitly excluded.
- [ ] Resource and privacy disclosures are written before public implementation claims.
- [ ] The decision creates no dependency on `lucidd` internals.

## Completion evidence required in the tracker

- Compatibility/backend decision matrix and measurements.
- Trademark and license decision records.
- Canonical use-case script/recording.
- Approved feature-envelope and API ADR.
- Exact task documentation.
