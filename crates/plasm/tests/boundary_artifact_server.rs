//! Boundary integration test #2: HTTP artifact server end-to-end Range request.
//!
//! This exercises the seam that becomes `phase-artifact-server` at M6. It
//! binds the real `ProviderServer` to an ephemeral 127.0.0.1 port, places a
//! known artifact on disk under `<channel>/<arch>/<name>`, performs a real
//! GET with a `Range:` header, and asserts that the server returns
//! 206 Partial Content with the correct `Content-Range` header and the
//! correct byte slice.
//!
//! Today's tests in `provider/server.rs` cover the `parse_range` helper but
//! never bind a socket. This test catches regressions in axum wiring, in
//! header construction, in the streaming read path, and in the route
//! `/:channel/:arch/:artifact` itself -- all of which will move across crate
//! boundaries during M6.

use plasm::provider::{ProviderConfig, ProviderServer};
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_server_e2e_range() {
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        // Lay out a real artifact directory: <temp>/stable/x86_64/kernel
        let temp = TempDir::new().expect("tempdir");
        let artifact_dir = temp.path().join("stable").join("x86_64");
        std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");

        // Build a 200-byte deterministic artifact so we can assert on a known slice.
        let mut payload = Vec::with_capacity(200);
        for i in 0u16..200u16 {
            payload.push((i & 0xFF) as u8);
        }
        let artifact_path = artifact_dir.join("kernel");
        std::fs::write(&artifact_path, &payload).expect("write artifact");

        // Find an ephemeral port by binding then dropping a socket on 127.0.0.1:0.
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
            // run() returns when the listener is dropped or errors.
            let _ = server.run().await;
        });

        // Wait briefly for the server to come up.
        let url = format!("http://127.0.0.1:{}/stable/x86_64/kernel", port);
        let mut ready = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let probe = reqwest::Client::new()
                .get(&url)
                .timeout(Duration::from_millis(250))
                .send()
                .await;
            if probe.is_ok() {
                ready = true;
                break;
            }
        }
        assert!(ready, "artifact server failed to start within 2.5s");

        // Range request: bytes=10-50 (inclusive on both ends, 41 bytes).
        let resp = reqwest::Client::new()
            .get(&url)
            .header("Range", "bytes=10-50")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("range request");

        assert_eq!(
            resp.status().as_u16(),
            206,
            "expected 206 Partial Content, got {}",
            resp.status()
        );

        let content_range = resp
            .headers()
            .get("content-range")
            .expect("content-range header missing")
            .to_str()
            .expect("content-range not utf-8")
            .to_string();
        assert_eq!(
            content_range, "bytes 10-50/200",
            "wrong Content-Range header"
        );

        let accept_ranges = resp
            .headers()
            .get("accept-ranges")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        assert_eq!(accept_ranges, "bytes");

        let body = resp.bytes().await.expect("range body");
        assert_eq!(body.len(), 41, "range body length mismatch");
        assert_eq!(
            &body[..],
            &payload[10..=50],
            "range body bytes do not match source slice"
        );

        server_handle.abort();
    })
    .await;

    assert!(
        result.is_ok(),
        "artifact_server_e2e_range exceeded its 15s budget"
    );
}
