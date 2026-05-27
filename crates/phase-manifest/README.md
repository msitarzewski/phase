# phase-manifest

Generic signed manifest types for Phase. Exposes `SignedManifest<T>` so any payload — WASM job spec, inference job spec, artifact descriptor — can be signed and verified through a single Ed25519-backed code path.
