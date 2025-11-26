# M6 — Phase/Plasma Hello Job Path

**Objective**  
After boot, prove end-to-end by running a WASM “hello job” via Plasm, fetched by CID.

**Deliverables**  
- Boot post-hook that starts Plasm in userland.  
- WASM example artifact (`hello.wasm`) with manifest.  
- Receipt printed to console and stored (except Private mode).

**Acceptance Criteria**  
- Job retrieved from network and executed; receipt includes module hash and timings.  
- Private mode suppresses persistent receipt.

**Tasks**  
- [ ] Post-boot unit/service to start Plasm.  
- [ ] WASM example + manifest publishing.  
- [ ] Receipt formatting and log policy per mode.

**Risks**  
- Networking reorder at handoff → add small retry window in post-boot service.
