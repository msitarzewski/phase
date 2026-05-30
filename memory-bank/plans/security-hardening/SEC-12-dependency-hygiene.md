# SEC-12 — Dependency hygiene (unmaintained crates)

**Severity:** 🔵 LOW (L3, L4) | **Phase:** 4 | **Effort:** small–medium (bincode migration is the real work) | **Status:** planned

## Why
`cargo audit` flagged 4 unmaintained crates (warnings, not vulnerabilities):
- **`bincode 1.3.3`** — RUSTSEC-2025-0141 unmaintained. **We use it directly** for DHT advertisement encoding (`crates/lucidd/src/registry.rs`). This is the one that's our own choice, not transitive.
- **`fxhash 0.2.1`** — RUSTSEC-2025-0057, transitive.
- **`mach 0.3.2`** — RUSTSEC-2020-0168, transitive (macOS).
- **`paste 1.0.15`** — RUSTSEC-2024-0436, transitive (via wasmtime + netlink).

## Scope
- `crates/lucidd/src/registry.rs` + `crates/lucidd/Cargo.toml` — the bincode usage (`SignedModelAdvertisement::encode/decode`, the `SigningPayload` canonical bincode).
- Transitive ones: track only; they clear when upstreams update.

## Approach
1. **bincode (our code):** migrate `registry.rs` off bincode 1.x. Options:
   - `bincode 2.x` (maintained successor, different API — `encode_to_vec`/`decode_from_slice` with a `Config`), or
   - `postcard` (no_std-friendly, stable wire format, well-maintained), or
   - reuse the **canonical-JSON** approach already used elsewhere in the codebase (consistency — but JSON is larger on the DHT).
   **Important:** the advertisement signing payload is bincode-encoded and **signed**, so changing the encoding changes the signed bytes → bump `ADVERTISEMENT_SCHEMA_VERSION` and ensure old/new nodes don't silently mis-verify. Since the network is tiny (v0.1), a clean break is acceptable; document it.
   Recommendation: **postcard** — stable, maintained, compact, deterministic (good for signed payloads).
2. **Transitive (fxhash/mach/paste):** no action beyond SEC-02's wasmtime bump (which may drop `paste`/`mach` to newer lines). Add them to `deny.toml`'s allow-list with the RUSTSEC IDs + "transitive, tracked" notes so SEC-00's gate stays green. Re-check after each dependency bump.

## Acceptance criteria
- `registry.rs` no longer depends on bincode 1.x; advertisements encode/decode/sign/verify with the replacement; `ADVERTISEMENT_SCHEMA_VERSION` bumped.
- All registry tests (13) pass with the new encoding (round-trip, tamper detection, schema mismatch).
- Transitive unmaintained crates documented in `deny.toml`; `cargo deny check` passes.
- `cargo audit` shows 0 vulnerabilities and only the explicitly-accepted unmaintained warnings.

## Test plan
- Existing `registry` test suite (13 tests) green on the new encoding.
- A fresh advertise→DHT→find_peers round-trip works end-to-end (the live two-node path, if convenient).
- Tamper test still detects mutation under the new encoding.

## Dependencies
- Coordinate with SEC-02 (which may already shift the transitive ones). Do the bincode migration after SEC-02 so the dep tree is settled.
