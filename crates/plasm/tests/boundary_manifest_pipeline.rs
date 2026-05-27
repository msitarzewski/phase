//! Boundary integration test #3: manifest sign -> serialize -> fetch -> verify.
//!
//! This exercises the four-way seam between what will become
//! `phase-manifest`, `phase-identity`, `phase-artifact-server`, and the
//! plasm provider. After the M5+M6 split the verifier will only have access
//! to the published public key and the manifest bytes -- nothing else
//! crosses the boundary. So that's exactly what this test simulates:
//! sign with a generated Ed25519 key, serialise to JSON, write to the
//! artifact directory, fetch over HTTP via `ProviderServer`, deserialise,
//! and verify the signature using only the public key.

use ed25519_dalek::{SigningKey, VerifyingKey};
use plasm::provider::{
    manifest::{ArtifactInfo, BootManifest, ManifestBuilder},
    signing::{sign_manifest, verify_manifest_signature},
    ProviderConfig, ProviderServer,
};
use rand::RngCore;
use std::time::Duration;
use tempfile::TempDir;

fn fresh_signing_key() -> SigningKey {
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    SigningKey::from_bytes(&secret)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_sign_serialize_fetch_verify() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        // 1. Generate a fresh ed25519 keypair. Only the verifying key will be
        //    available to the consumer side of the boundary.
        let signing_key = fresh_signing_key();
        let verifying_key: VerifyingKey = signing_key.verifying_key();

        // 2. Build a minimal valid BootManifest and sign it.
        let artifact = ArtifactInfo {
            filename: "kernel".to_string(),
            size_bytes: 64,
            hash: "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
            download_url: Some("/stable/x86_64/kernel".to_string()),
        };
        let mut manifest = ManifestBuilder::new("stable".to_string(), "x86_64".to_string())
            .version("0.1.0-boundary".to_string())
            .artifact("kernel".to_string(), artifact)
            .build()
            .expect("build manifest");

        sign_manifest(&mut manifest, &signing_key).expect("sign manifest");
        assert_eq!(manifest.signatures.len(), 1, "expected one signature");

        // 3. Serialise to JSON bytes -- this is the wire format crossing
        //    crate boundaries.
        let json = serde_json::to_vec(&manifest).expect("serialise manifest");

        // 4. Stand up an artifact directory containing the manifest JSON
        //    plus the kernel artifact, and serve it via ProviderServer.
        let temp = TempDir::new().expect("tempdir");
        let arch_dir = temp.path().join("stable").join("x86_64");
        std::fs::create_dir_all(&arch_dir).expect("arch dir");
        std::fs::write(arch_dir.join("kernel"), vec![0x42u8; 64]).expect("write kernel");
        // ProviderServer dynamically generates manifest.json from its own
        // ManifestGenerator (unsigned). To fetch *our* signed manifest we
        // place it at a known artifact path that the server will serve raw
        // through the artifact handler.
        std::fs::write(arch_dir.join("signed-manifest.json"), &json).expect("write manifest");

        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        let port = probe.local_addr().expect("probe addr").port();
        drop(probe);

        let config = ProviderConfig {
            enabled: true,
            bind_addr: "127.0.0.1".to_string(),
            port,
            artifacts_dir: temp.path().to_path_buf(),
            channel: "stable".to_string(),
            arch: "x86_64".to_string(),
        };
        let server = ProviderServer::new(config);
        let server_handle = tokio::spawn(async move {
            let _ = server.run().await;
        });

        let url = format!(
            "http://127.0.0.1:{}/stable/x86_64/signed-manifest.json",
            port
        );

        // Poll until the server accepts a connection.
        let mut got = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            match reqwest::Client::new()
                .get(&url)
                .timeout(Duration::from_millis(500))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    got = Some(r.bytes().await.expect("body bytes"));
                    break;
                }
                _ => continue,
            }
        }
        let fetched = got.expect("server did not serve manifest within 2.5s");

        // 5. Deserialise on the consumer side -- only bytes crossed the wire.
        let recovered: BootManifest =
            serde_json::from_slice(&fetched).expect("deserialise fetched manifest");

        // 6. Verify the signature using only the public key.
        let verified = verify_manifest_signature(&recovered, &verifying_key)
            .expect("verify signature");
        assert!(verified, "manifest signature did not verify after HTTP round-trip");

        // Sanity: tampering with the recovered manifest must cause verification
        // to fail, so we know the signature check is real.
        let mut tampered = recovered.clone();
        tampered.version = "0.0.0-tampered".to_string();
        let still_ok = verify_manifest_signature(&tampered, &verifying_key)
            .expect("verify tampered");
        assert!(!still_ok, "tampered manifest must not verify");

        server_handle.abort();
    })
    .await;

    assert!(
        result.is_ok(),
        "manifest_sign_serialize_fetch_verify exceeded its 15s budget"
    );
}
