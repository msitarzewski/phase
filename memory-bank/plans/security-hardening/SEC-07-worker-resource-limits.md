# SEC-07 — Worker resource limits (model LRU + port exhaustion)

**Severity:** 🟡 MEDIUM (M2) | **Phase:** 3 | **Effort:** small (1 agent-wave) | **Status:** planned

## Why
- **Unbounded model loading** (`worker_llama.rs:219-290`): `ensure_loaded` has no cap on concurrently-loaded models; eviction was explicitly deferred to "M6" (`worker_llama.rs:46`). Each model is ~GB RAM. The local API has no limit at all; relay peers can pin every on-disk model into RAM concurrently → memory-exhaustion DoS.
- **Port exhaustion** (`worker_llama.rs:292-305`): `allocate_port` does `fetch_add % span` over a 120-port range (`~130`) and never checks occupancy — past `span` loads it **wraps and reuses live ports**, the child's bind fails, surfaced as a generic error with no backpressure → spawn/fail churn.

## Scope
- `crates/lucidd/src/worker_llama.rs` — `ensure_loaded` (`~219`), `allocate_port` (`~292`), the `LoadedModel` struct (the `last_used` field already exists, `~158`).
- `crates/lucidd/src/policy.rs` and/or `LlamaCppConfig` — a `max_loaded_models` setting.

## Approach
1. **LRU eviction:** add `max_loaded_models` (config, sane default e.g. 2–3 depending on typical model size vs RAM). In `ensure_loaded`, before spawning a new model when at cap, evict the least-recently-used (`last_used` is already tracked) — cleanly shut down its `llama-server` child (reuse the existing `Drop`/kill path) and free its port.
2. **Port tracking:** maintain a set of in-use ports; `allocate_port` picks a free one; when the range is full, return `WorkerError::Capacity` instead of wrapping onto a live port. Release the port on model unload/evict.
3. Ensure eviction + the supervisor's restart logic don't race (evicting a model mid-restart).

## Acceptance criteria
- At most `max_loaded_models` `llama-server` processes run at once; loading an N+1th evicts the LRU.
- Port allocation never reuses a live port; range exhaustion returns `Capacity`, not a spawn-fail churn.
- Evicted models' subprocesses are killed and ports freed (no leak across evictions — verify no zombie/FD leak).
- Existing single-model flows unchanged.

## Test plan
- Test (with the fake-llama-server fixture): load `max+1` distinct models → exactly `max` children alive, LRU evicted, its port freed.
- Test: exhaust the port range → `Capacity` error, no live-port reuse.
- Test: evict-then-reload the same model works (port reacquired cleanly).

## Dependencies
None. Parallel-safe with SEC-04 (same file, different functions — review for collisions on `LoadedModel`/`ensure_loaded` edits).
