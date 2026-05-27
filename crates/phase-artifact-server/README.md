# phase-artifact-server

A content-addressed HTTP server for the Phase substrate. Distributes any signed blob — boot images, WASM modules, inference model weights — keyed by content hash, with range request support and signed-manifest integration via `phase-manifest`.
