// SPDX-License-Identifier: Apache-2.0
//
// Boundary test: prove a `SignedReceipt<JobResult>` produced by the M7
// WasmtimeWorker round-trips through the PHP SDK's canonical-JSON
// verification path. The companion PHP-side check lives in
// `examples/php_compat_receipt_check.php`; this test produces the JSON
// envelope on disk that the PHP script then verifies, so a CI / local run
// can drive the full Rust → PHP path end-to-end.

use std::path::PathBuf;

use futures_util::StreamExt;
use phase_identity::NodeIdentity;
use phase_manifest::ManifestBuilder;
use phase_protocol::{JobEvent, JobSpec, WasmJobSpec, Worker};
use plasm::worker::{WasmtimeWorker, WorkerSecurityConfig};

#[tokio::test]
async fn wasm_worker_emits_php_verifiable_receipt() {
    let id = NodeIdentity::generate();
    // SEC-01: worker is deny-all by default. This boundary test signs with
    // `id` and submits to itself; allowlist that key so the job dispatches.
    let worker = WasmtimeWorker::new(id.clone()).with_security(WorkerSecurityConfig {
        authorized_submitters: vec![hex::encode(id.verifying_key().to_bytes())],
        ..WorkerSecurityConfig::default()
    });

    let payload = JobSpec::Wasm(WasmJobSpec {
        module_cid: "inline".to_string(),
        // Minimal-but-invalid wasm — execution will Error, but we still
        // emit a signed receipt over a non-empty stdout chunk and the PHP
        // SDK should verify it.
        input: vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
        max_duration_ms: Some(5_000),
        max_memory_bytes: Some(64 * 1024 * 1024),
    });
    let manifest = ManifestBuilder::new(payload)
        .sign_with(&id)
        .expect("sign manifest");

    let (handle, mut stream) = worker.execute(manifest).await.expect("dispatch");
    while let Some(event) = stream.next().await {
        if matches!(event, JobEvent::Final { .. }) {
            break;
        }
    }
    let receipt = handle.finish().await.expect("receipt");
    receipt.verify().expect("rust-side verify");

    // Write the JSON envelope to a path the PHP script can read. The path
    // is intentionally outside `target/` so a manual run is easy.
    let json = serde_json::to_string_pretty(&receipt).expect("serialize");
    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("phase-m7-php-compat");
    std::fs::create_dir_all(&out_dir).expect("mkdir");
    let out_path = out_dir.join("receipt.json");
    std::fs::write(&out_path, &json).expect("write");

    // The signing public key, hex-encoded — what the PHP verifier pins to.
    let pk_path = out_dir.join("worker_pubkey.hex");
    let pk_hex = receipt.worker_pubkey.clone();
    std::fs::write(&pk_path, &pk_hex).expect("write pk");

    // Smoke check: every field the PHP SDK reads is present.
    assert!(!receipt.signature.is_empty());
    assert!(!receipt.worker_pubkey.is_empty());
    assert!(!receipt.job_id.is_empty());
    assert_eq!(receipt.schema_version, 1);
}
