# plasm

The reference WASM Phase node. Plasm implements
`phase-protocol::Worker` using Wasmtime, executing `JobSpec::Wasm` jobs
received from the network. It is one Phase node implementation among many —
equal-citizen to LUCID and to any future implementation.

## Binaries

- `plasmd` — the local execution daemon. Subcommands: `start`,
  `execute-job`, `run`, `serve`, `provider {status,list}`, `init`, `version`.
- `phase-discover`, `phase-verify`, `phase-fetch` — `phase-boot` helper
  binaries used by the network bootstrap flow.

## Library

`plasm::worker::WasmtimeWorker` is the `phase-protocol::Worker`
implementation:

```rust,no_run
use phase_identity::NodeIdentity;
use plasm::worker::WasmtimeWorker;

let identity = NodeIdentity::generate();
let worker = WasmtimeWorker::new(identity);
// `worker` impls `phase_protocol::Worker` and can be plugged into a
// scheduler / router.
```

## Legacy surface

`plasm::provider::*` exposes the original PHP-compat `BootManifest` +
`ManifestGenerator` + Ed25519 signing code. That surface is plasm-specific
and intentionally NOT part of the Phase substrate; the substrate-level
artifact/manifest/receipt envelopes live in `phase-artifact-server`,
`phase-manifest`, and `phase-receipt`.
