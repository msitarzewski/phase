# Test Audit

Audit of the existing `daemon/` test suite prior to extracting library crates
(`phase-net`, `phase-identity`, `phase-manifest`, `phase-receipt`,
`phase-protocol`, `phase-artifact-server`, `plasm`).

Source of truth: all `#[test]` and `#[tokio::test]` functions discovered under
`/Users/michael/Software/phase/daemon/src/` on 2026-05-27.

Note: `cargo test --list` could not be executed because the workspace at
`/Users/michael/Software/phase/Cargo.toml` references `crates/plasm` and
`crates/lucidd` that do not yet have valid `Cargo.toml` files (workspace
scaffold in progress per Phase-core M1). Test inventory was therefore produced
by static enumeration of `#[test]` / `#[tokio::test]` attributes across all
`src/**/*.rs` files in `daemon/`.

## Summary

- Total tests found: **80** (matches Memory Bank claim)
- unit-internal: **44**
- unit-public: **22**
- integration-internal: **6**
- integration-external: **8**
- Will-survive-refactor: **68**
- Will-break-on-refactor: **12** (listed below)

### Tests that will break on refactor

These reference internal module paths (`crate::provider::...`,
`super::super::protocol::...`) or test helpers/types that are slated to move
into different crates. The assertions themselves remain valid; only the
`use` statements need to be re-pointed and, in some cases, types relocated
behind a re-export from the destination crate.

1. `daemon/src/network/execution.rs::test_execution_handler_creation` — imports `super::super::protocol::JobRequirements`; that type moves to `phase-protocol`.
2. `daemon/src/network/execution.rs::test_module_hash_verification` — same cross-module import; also pins `wasmtime-27` runtime string that lives in plasm post-split.
3. `daemon/src/network/execution.rs::test_invalid_hash_rejected` — same cross-module import.
4. `daemon/src/provider/signing.rs::test_sign_and_verify_manifest` — imports `crate::provider::manifest::{ArtifactInfo, ManifestBuilder}`; signing moves to `phase-manifest`, builder stays adjacent but path changes.
5. `daemon/src/provider/signing.rs::test_verify_tampered_manifest` — same cross-module import.
6. `daemon/src/provider/signing.rs::test_verify_wrong_key` — same cross-module import.
7. `daemon/src/provider/signing.rs::test_manifest_hash_deterministic` — same cross-module import.
8. `daemon/src/provider/generator.rs::test_generate_manifest` — depends on `provider::artifacts::ArtifactStore` (moves to `phase-artifact-server`) and `provider::manifest` (moves to `phase-manifest`).
9. `daemon/src/provider/generator.rs::test_generate_manifest_no_artifacts` — same cross-crate dependency on `ArtifactStore`.
10. `daemon/src/provider/generator.rs::test_generate_signed_manifest` — imports `crate::provider::signing::generate_signing_key` (moves to `phase-identity`).
11. `daemon/src/provider/generator.rs::test_download_urls` — depends on `ArtifactStore`.
12. `daemon/src/provider/generator.rs::test_with_version` — depends on `ArtifactStore`.

## Verdict

**Not fully refactor-safe as-is.** The existing 80 tests are heavily slanted
toward narrow, in-module unit assertions (serialization round-trips, range
header parsing, builder validation, individual helper functions). Those will
all survive once `use` paths are updated, because they exercise pure data
behavior that has no cross-crate coupling.

What is **missing** for the extraction work is end-to-end coverage of the
seams that will become crate boundaries. Today no test:

- Connects two real libp2p swarms and verifies the request/response protocol
  on the wire.
- Starts the HTTP artifact server in-process and downloads a real file
  through it (including range requests against a real socket).
- Round-trips a manifest from generator -> sign -> serialize -> HTTP fetch
  -> verify, across what will become four crates.
- Executes a real WASM module through the runtime and validates the
  resulting receipt against a separately-loaded public key.
- Persists and reloads a signing identity across daemon restarts (the
  identity-persistence story planned for `phase-identity` M3).

Without those, refactor regressions in glue code (wrong type alias, wrong
re-export, dropped trait bound, lost async cancellation behavior) will
compile clean and pass every existing test while silently breaking the
daemon at runtime.

### Recommended boundary tests to add BEFORE starting M2

1. **Two-node libp2p job round-trip** — spawn two in-process `Swarm`s on
   loopback, have node A send a `JobRequest` containing a tiny valid WASM
   module to node B over the request/response protocol, assert node B
   returns a `JobResult` whose signature verifies against node B's public
   key. Protects the `phase-net` <-> `phase-protocol` <-> plasm execution
   seam.

2. **HTTP artifact server end-to-end** — bind `ProviderServer` to
   `127.0.0.1:0`, populate `ArtifactStore` with a real file, fetch it with
   an `hyper`/`reqwest` client including a `Range: bytes=N-M` header,
   assert returned bytes match. Protects the `phase-artifact-server`
   extraction; the existing `parse_range` tests cover the parser but never
   touch a socket.

3. **Manifest sign-serialize-fetch-verify pipeline** — use
   `ManifestGenerator` to build a manifest from a real `ArtifactStore`,
   sign with a generated key, serve via `ProviderServer`, fetch from a
   client, verify signature with the public key alone (no shared state).
   Protects the `phase-manifest` + `phase-identity` + `phase-artifact-server`
   seam.

4. **Receipt round-trip via stable bytes** — sign a receipt on node A,
   serialize to JSON, hand the JSON + hex public key to node B (simulated
   by a second `Receipt::from_json` call in a fresh scope), verify. The
   existing `test_receipt_signing_and_verification` does this in one
   process with shared types; the new test should treat the receipt as
   opaque bytes crossing a crate boundary.

5. **Persistent identity load/save** — generate a signing key, write to
   a temp file via whatever API `phase-identity` will expose, drop, reload
   from the same path, sign a payload, verify the resulting signature is
   bit-identical (or at least verifies under the same public key). No
   such test exists today; identity is generated fresh per test.

These five tests would establish the contract surface that the extraction
must preserve, and any of them failing after M2-M7 would be an immediate
regression signal.

## Detail

| #  | Test name                                | File:line                                            | Category               | Protects against                                                                  | Survives refactor? |
|----|------------------------------------------|------------------------------------------------------|------------------------|-----------------------------------------------------------------------------------|--------------------|
| 1  | test_default_config                      | daemon/src/config.rs:181                             | unit-internal          | Default daemon config values drift                                                | YES                |
| 2  | test_save_load_config                    | daemon/src/config.rs:188                             | integration-external   | Config TOML round-trip via filesystem                                             | YES                |
| 3  | test_discovery_creation                  | daemon/src/network/discovery.rs:392                  | integration-external   | mDNS/Discovery startup (permission-tolerant smoke)                                | YES                |
| 4  | test_default_capabilities                | daemon/src/network/discovery.rs:415                  | unit-internal          | PeerCapabilities auto-detect produces non-empty arch + nonzero cores              | YES                |
| 5  | test_execution_handler_creation          | daemon/src/network/execution.rs:112                  | integration-internal   | ExecutionHandler exposes ed25519 pubkey of correct length                         | NO                 |
| 6  | test_module_hash_verification            | daemon/src/network/execution.rs:121                  | integration-internal   | Hash check passes when module bytes match declared hash                           | NO                 |
| 7  | test_invalid_hash_rejected               | daemon/src/network/execution.rs:152                  | integration-internal   | "hash mismatch" surfaces when wasm bytes != declared hash                         | NO                 |
| 8  | test_job_offer_serialization             | daemon/src/network/protocol.rs:179                   | unit-public            | JobOffer JSON round-trip wire format                                              | YES                |
| 9  | test_job_response_accepted               | daemon/src/network/protocol.rs:201                   | unit-public            | JobResponse::Accepted variant tag in JSON                                         | YES                |
| 10 | test_job_response_rejected               | daemon/src/network/protocol.rs:213                   | unit-public            | JobResponse::Rejected variant tag in JSON                                         | YES                |
| 11 | test_job_request_serialization           | daemon/src/network/protocol.rs:226                   | unit-public            | JobRequest JSON round-trip including wasm bytes + args                            | YES                |
| 12 | test_job_request_validation              | daemon/src/network/protocol.rs:252                   | unit-public            | JobRequest::validate rejects empty job_id                                         | YES                |
| 13 | test_job_result_serialization            | daemon/src/network/protocol.rs:285                   | unit-public            | JobResult JSON round-trip                                                         | YES                |
| 14 | test_runtime_creation                    | daemon/src/wasm/runtime.rs:216                       | unit-internal          | Default Wasm3Runtime memory limit                                                 | YES                |
| 15 | test_runtime_with_limits                 | daemon/src/wasm/runtime.rs:222                       | unit-internal          | Builder-style limit setters                                                       | YES                |
| 16 | test_compute_module_hash                 | daemon/src/wasm/runtime.rs:232                       | unit-internal          | sha256: prefix + 64-hex-char format of module hash                                | YES                |
| 17 | test_manifest_creation                   | daemon/src/wasm/manifest.rs:79                       | unit-internal          | JobManifest defaults                                                              | YES                |
| 18 | test_manifest_validation                 | daemon/src/wasm/manifest.rs:87                       | unit-internal          | JobManifest rejects zero cpu_cores                                                | YES                |
| 19 | test_manifest_json_serialization         | daemon/src/wasm/manifest.rs:97                       | unit-public            | JobManifest JSON round-trip                                                       | YES                |
| 20 | test_receipt_creation                    | daemon/src/wasm/receipt.rs:153                       | unit-internal          | Receipt default fields                                                            | YES                |
| 21 | test_receipt_json_serialization          | daemon/src/wasm/receipt.rs:162                       | unit-public            | Receipt JSON round-trip post-signing                                              | YES                |
| 22 | test_receipt_signing_and_verification    | daemon/src/wasm/receipt.rs:189                       | integration-external   | ed25519 sign+verify cycle on receipt (correct key passes, wrong key fails)        | YES                |
| 23 | test_mdns_config_creation                | daemon/src/provider/mdns.rs:178                      | unit-internal          | MdnsConfig field assignment                                                       | YES                |
| 24 | test_txt_records                         | daemon/src/provider/mdns.rs:189                      | unit-public            | mDNS TXT record keys for channel/arch/http_port/version                           | YES                |
| 25 | test_hostname                            | daemon/src/provider/mdns.rs:200                      | integration-external   | OS hostname lookup returns non-empty <256 chars                                   | YES                |
| 26 | test_advertiser_creation                 | daemon/src/provider/mdns.rs:208                      | unit-internal          | MdnsAdvertiser placeholder lifecycle                                              | YES                |
| 27 | test_default_config (provider)           | daemon/src/provider/config.rs:151                    | unit-internal          | ProviderConfig defaults (enabled=false, port=8080)                                | YES                |
| 28 | test_bind_address                        | daemon/src/provider/config.rs:160                    | unit-internal          | bind_address() formats host:port                                                  | YES                |
| 29 | test_arch_detection                      | daemon/src/provider/config.rs:170                    | unit-internal          | Auto-detected arch is non-empty                                                   | YES                |
| 30 | test_compute_file_hash                   | daemon/src/provider/signing.rs:126                   | integration-external   | SHA256 of "hello world" matches known constant                                    | YES                |
| 31 | test_key_id                              | daemon/src/provider/signing.rs:136                   | unit-internal          | key_id returns 64 hex chars                                                       | YES                |
| 32 | test_sign_and_verify_manifest            | daemon/src/provider/signing.rs:143                   | integration-internal   | sign_manifest writes signature, verify_manifest_signature returns true            | NO                 |
| 33 | test_verify_tampered_manifest            | daemon/src/provider/signing.rs:171                   | integration-internal   | Tampered version field fails verification                                         | NO                 |
| 34 | test_verify_wrong_key                    | daemon/src/provider/signing.rs:199                   | integration-internal   | Wrong verifying key errors                                                        | NO                 |
| 35 | test_manifest_hash_deterministic         | daemon/src/provider/signing.rs:224                   | unit-public            | compute_manifest_hash returns same hash on repeat                                 | NO                 |
| 36 | test_artifact_validation_valid           | daemon/src/provider/manifest.rs:420                  | unit-internal          | ArtifactInfo.validate accepts well-formed input                                   | YES                |
| 37 | test_artifact_validation_empty_filename  | daemon/src/provider/manifest.rs:426                  | unit-internal          | Empty filename rejected                                                           | YES                |
| 38 | test_artifact_validation_zero_size       | daemon/src/provider/manifest.rs:433                  | unit-internal          | Zero size_bytes rejected                                                          | YES                |
| 39 | test_artifact_validation_invalid_hash_no_colon | daemon/src/provider/manifest.rs:440            | unit-internal          | Hash missing "algo:" prefix rejected                                              | YES                |
| 40 | test_artifact_validation_invalid_hash_non_hex  | daemon/src/provider/manifest.rs:447            | unit-internal          | Non-hex hash body rejected                                                        | YES                |
| 41 | test_manifest_validation_valid           | daemon/src/provider/manifest.rs:454                  | unit-internal          | BootManifest with kernel artifact validates                                       | YES                |
| 42 | test_manifest_validation_missing_kernel  | daemon/src/provider/manifest.rs:460                  | unit-internal          | Missing kernel artifact yields MissingArtifact error                              | YES                |
| 43 | test_manifest_validation_invalid_version | daemon/src/provider/manifest.rs:470                  | unit-internal          | manifest_version != 1 rejected                                                    | YES                |
| 44 | test_manifest_serialization_roundtrip    | daemon/src/provider/manifest.rs:477                  | unit-public            | BootManifest JSON round-trip equality                                             | YES                |
| 45 | test_builder_minimal                     | daemon/src/provider/manifest.rs:491                  | unit-internal          | ManifestBuilder produces valid minimal manifest                                   | YES                |
| 46 | test_builder_missing_version             | daemon/src/provider/manifest.rs:506                  | unit-internal          | Builder fails without version                                                     | YES                |
| 47 | test_builder_missing_kernel              | daemon/src/provider/manifest.rs:515                  | unit-internal          | Builder fails without kernel artifact                                             | YES                |
| 48 | test_builder_with_provider               | daemon/src/provider/manifest.rs:524                  | unit-internal          | Builder attaches ProviderInfo                                                     | YES                |
| 49 | test_iso8601_validation                  | daemon/src/provider/manifest.rs:541                  | unit-internal          | is_valid_iso8601 helper accepts/rejects samples                                   | YES                |
| 50 | test_metrics_increment                   | daemon/src/provider/metrics.rs:97                    | unit-internal          | ProviderMetrics counter increment + snapshot                                      | YES                |
| 51 | test_health_check                        | daemon/src/provider/metrics.rs:107                   | integration-external   | perform_health_check against a real tempdir path                                  | YES                |
| 52 | test_artifact_store_new                  | daemon/src/provider/artifacts.rs:223                 | unit-internal          | ArtifactStore::new succeeds on tempdir                                            | YES                |
| 53 | test_get_artifact_not_found              | daemon/src/provider/artifacts.rs:230                 | unit-internal          | get_artifact returns None for missing file                                        | YES                |
| 54 | test_get_artifact_found                  | daemon/src/provider/artifacts.rs:237                 | integration-external   | get_artifact returns metadata + sha256 hash from real file                        | YES                |
| 55 | test_path_traversal_prevention           | daemon/src/provider/artifacts.rs:255                 | unit-public            | "../" segments rejected by get_artifact_path                                      | YES                |
| 56 | test_list_artifacts                      | daemon/src/provider/artifacts.rs:262                 | integration-external   | list_artifacts walks real directory                                               | YES                |
| 57 | test_compute_hash                        | daemon/src/provider/artifacts.rs:275                 | integration-external   | SHA256 of "hello world" matches known constant from a real file                   | YES                |
| 58 | test_manifest_record_new                 | daemon/src/provider/dht.rs:90                        | unit-internal          | ManifestRecord builds expected manifest_url + TTL                                 | YES                |
| 59 | test_dht_key_format                      | daemon/src/provider/dht.rs:105                       | unit-public            | DHT key contains "/phase/{channel}/{arch}/manifest"                               | YES                |
| 60 | test_serialization_roundtrip (dht)       | daemon/src/provider/dht.rs:114                       | unit-public            | ManifestRecord bytes round-trip                                                   | YES                |
| 61 | test_not_expired_initially               | daemon/src/provider/dht.rs:131                       | unit-internal          | Fresh ManifestRecord reports not expired                                          | YES                |
| 62 | test_server_creation                     | daemon/src/provider/server.rs:398                    | unit-internal          | ProviderServer captures config.port                                               | YES                |
| 63 | test_parse_range_full_range              | daemon/src/provider/server.rs:405                    | unit-internal          | "bytes=0-1023" parses to (0,1023)                                                 | YES                |
| 64 | test_parse_range_from_offset             | daemon/src/provider/server.rs:412                    | unit-internal          | "bytes=N-" clamps to file end                                                     | YES                |
| 65 | test_parse_range_single_byte             | daemon/src/provider/server.rs:419                    | unit-internal          | Single-byte range                                                                 | YES                |
| 66 | test_parse_range_last_byte               | daemon/src/provider/server.rs:426                    | unit-internal          | Last-byte range                                                                   | YES                |
| 67 | test_parse_range_invalid_beyond_size     | daemon/src/provider/server.rs:433                    | unit-internal          | Beyond-EOF range rejected                                                         | YES                |
| 68 | test_parse_range_invalid_start_after_end | daemon/src/provider/server.rs:440                    | unit-internal          | start>end rejected                                                                | YES                |
| 69 | test_parse_range_invalid_format          | daemon/src/provider/server.rs:447                    | unit-internal          | Missing "bytes=" prefix rejected                                                  | YES                |
| 70 | test_parse_range_invalid_format_multiple_dashes | daemon/src/provider/server.rs:454              | unit-internal          | Multiple dashes rejected                                                          | YES                |
| 71 | test_parse_range_start_equals_size       | daemon/src/provider/server.rs:461                    | unit-internal          | start==size rejected                                                              | YES                |
| 72 | test_parse_range_empty_values            | daemon/src/provider/server.rs:468                    | unit-internal          | "bytes=-" rejected                                                                | YES                |
| 73 | test_parse_range_non_numeric             | daemon/src/provider/server.rs:475                    | unit-internal          | Non-numeric values rejected                                                       | YES                |
| 74 | test_parse_range_zero_length_file        | daemon/src/provider/server.rs:482                    | unit-internal          | Zero-length file yields None                                                      | YES                |
| 75 | test_generate_manifest                   | daemon/src/provider/generator.rs:146                 | integration-internal   | ManifestGenerator builds BootManifest from ArtifactStore                          | NO                 |
| 76 | test_generate_manifest_no_artifacts      | daemon/src/provider/generator.rs:160                 | integration-internal   | Empty store yields error                                                          | NO                 |
| 77 | test_generate_signed_manifest            | daemon/src/provider/generator.rs:170                 | integration-internal   | Generator produces ed25519 signature when key supplied                            | NO                 |
| 78 | test_normalize_artifact_name             | daemon/src/provider/generator.rs:184                 | unit-public            | vmlinuz/bzImage/initrd alias normalization                                        | YES                |
| 79 | test_download_urls                       | daemon/src/provider/generator.rs:198                 | integration-internal   | Manifest download_url uses /{channel}/{arch}/{name} layout                        | NO                 |
| 80 | test_with_version                        | daemon/src/provider/generator.rs:212                 | integration-internal   | Generator version override propagates                                             | NO                 |
