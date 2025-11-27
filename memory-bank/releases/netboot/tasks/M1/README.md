# Milestone M1 — HTTP Artifact Server

**Status**: PLANNED
**Owner**: Runtime Agent (primary), Networking Agent (integration)
**Dependencies**: Existing plasmd codebase with tokio runtime
**Estimated Effort**: 2-3 weeks

## Intent Summary

Add an embedded HTTP server to plasmd for serving boot artifacts (kernel, initramfs, rootfs). This is the foundation for the provider functionality - before we can advertise in DHT, we need to actually serve files.

---

## Acceptance Criteria

1. **HTTP server starts**: plasmd can start HTTP server on configurable port
2. **Artifact serving**: GET requests return correct files with proper headers
3. **Large file support**: Range requests work for files >100MB
4. **Health check**: `/health` endpoint returns 200 OK for monitoring
5. **Concurrent requests**: Multiple simultaneous downloads work correctly
6. **Graceful shutdown**: Server stops cleanly when plasmd exits

## Technical Decisions

### HTTP Framework Choice
**Recommendation**: `axum` (built on hyper, tower ecosystem)
- Already async/tokio-native (matches plasmd)
- Good middleware support
- Lightweight, fast
- Active maintenance

**Alternatives considered**:
- `hyper` directly: Lower-level, more boilerplate
- `warp`: Similar to axum, less momentum
- `actix-web`: Different async runtime (actix vs tokio)

### Integration Approach
- HTTP server runs alongside libp2p swarm in same tokio runtime
- Shared state via `Arc<RwLock<ProviderState>>`
- Server lifecycle tied to plasmd daemon lifecycle

---

## Tasks

1. [HTTP Server Integration](task-1-http-server.md) — Add axum to plasmd
2. [Artifact Endpoints](task-2-artifact-endpoints.md) — Serve kernel/initramfs/rootfs
3. [Range Request Support](task-3-range-requests.md) — Partial content for large files
4. [Health & Status Endpoints](task-4-health-status.md) — Monitoring endpoints
5. [Testing & Validation](task-5-testing.md) — Unit and integration tests

---

## File Changes

### New Files
```
daemon/src/provider/
├── mod.rs              # Provider module
├── server.rs           # HTTP server setup
├── handlers.rs         # Request handlers
├── state.rs            # Shared provider state
└── range.rs            # Range request handling
```

### Modified Files
```
daemon/src/lib.rs       # Export provider module
daemon/src/main.rs      # Start HTTP server in daemon mode
daemon/Cargo.toml       # Add axum, tower dependencies
```

---

## Dependencies (Cargo.toml additions)

```toml
[dependencies]
axum = "0.7"
tower = "0.4"
tower-http = { version = "0.5", features = ["fs", "cors", "trace"] }
```

---

## API Preview

```
GET /                   → Provider info (JSON)
GET /health             → Health check (200 OK)
GET /manifest.json      → Boot manifest (M2)
GET /kernel             → Kernel file
GET /initramfs          → Initramfs file
GET /rootfs             → Root filesystem file
```

---

## Success Metrics

- HTTP server starts in <100ms
- File serving throughput >100MB/s on localhost
- Memory overhead <10MB for server
- Zero panics under load testing
