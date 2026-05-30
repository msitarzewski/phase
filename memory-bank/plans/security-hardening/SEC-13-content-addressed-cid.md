# SEC-13 — Content-addressed ModelCid (deferred to v0.2)

**Severity:** ⚪ INFO / known v0.1 limitation (L6) | **Phase:** 5 — deferred | **Effort:** medium, design-heavy | **Status:** deferred to v0.2

## Why
`ModelCid::from_model_id` (`registry.rs:139-148`) computes the CID as `SHA-256("phase/model-id-v1:" || name)` — a hash of the model **name**, not its **content**. Two peers advertising `"qwen3"` collide regardless of what weights they actually serve. So any peer can advertise a backdoored/garbage model under a popular name at near-zero cost, and `find_peers_by_model_id` returns it. The signature + PeerID binding (correctly implemented) authenticates *who advertised*, not *what they serve*.

This was a **deliberate v0.1 shortcut** taken during the M8 demo session (it's what made cross-peer name→CID resolution work without a registry). It's documented as such. Impact under the v0.1 trust model is bounded: discovery returns *candidate* peers; clients must already trust the peer. The real risk is **model substitution / quality degradation**, not key compromise.

## Why deferred
A real fix requires the content-distribution design that's already on the v0.2 roadmap:
- `/api/pull` actually downloading + hashing weights,
- CID = hash of the GGUF content (or a Merkle root over chunks),
- a cross-peer name→CID index (the documented v0.2 "cross-peer name registry"),
- verification at request time that the served model matches the advertised content CID.

Doing it now would mean designing the content layer in isolation; better to do it with the v0.2 pull/registry work as one coherent piece.

## Interim mitigations (cheap, can do before v0.2)
1. **Surface the advertising pubkey to the user** in routing decisions / response headers, so a human can pin trusted server keys (`X-Lucid-Served-By: <pubkey>`).
2. **Allow operator key-pinning** — a client config of trusted server pubkeys per model, so you only route to servers you trust for sensitive models. (This dovetails with SEC-01's allowlist machinery — same shape, other direction.)
3. Document the limitation prominently in user-facing docs (it's in MISSION/audit; put it where a model consumer will see it).

## Acceptance criteria (v0.2, not now)
- CID derived from model content, verifiable at request time.
- Cross-peer name→CID index so name resolution doesn't depend on local load.
- A peer advertising a model whose content doesn't match the CID is detectable/rejectable.

## Dependencies
- v0.2 content-distribution / `/api/pull` design. Tracked here so it isn't lost; **not** part of the immediate hardening push.
