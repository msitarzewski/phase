# Task 5 — Testing & Validation

**Agent**: QA Agent
**Estimated**: 2 days

## 5.1 Unit tests for range parsing

- [ ] Create `daemon/src/provider/range_test.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_parse_range_start_end() {
          let range = ByteRange::parse("bytes=0-1023", 10000).unwrap();
          assert_eq!(range.start, 0);
          assert_eq!(range.end, 1023);
          assert_eq!(range.len(), 1024);
      }

      #[test]
      fn test_parse_range_open_end() {
          let range = ByteRange::parse("bytes=1000-", 10000).unwrap();
          assert_eq!(range.start, 1000);
          assert_eq!(range.end, 9999);
      }

      #[test]
      fn test_parse_range_suffix() {
          let range = ByteRange::parse("bytes=-500", 10000).unwrap();
          assert_eq!(range.start, 9500);
          assert_eq!(range.end, 9999);
          assert_eq!(range.len(), 500);
      }

      #[test]
      fn test_parse_range_clamp_to_file_size() {
          let range = ByteRange::parse("bytes=0-99999", 10000).unwrap();
          assert_eq!(range.end, 9999);
      }

      #[test]
      fn test_parse_range_invalid_unit() {
          let result = ByteRange::parse("kilobytes=0-100", 10000);
          assert!(matches!(result, Err(RangeError::InvalidUnit)));
      }

      #[test]
      fn test_parse_range_not_satisfiable() {
          let result = ByteRange::parse("bytes=20000-", 10000);
          assert!(matches!(result, Err(RangeError::NotSatisfiable)));
      }
  }
  ```

**Dependencies**: M1/Task 3
**Output**: Range parsing tests

---

## 5.2 Integration tests for HTTP server

- [ ] Create `daemon/tests/provider_test.rs`:
  ```rust
  use plasm::provider::{ProviderServer, ProviderState, ProviderConfig};
  use std::sync::Arc;
  use tokio::sync::RwLock;
  use reqwest::Client;

  async fn setup_test_server() -> (String, tempfile::TempDir) {
      let temp_dir = tempfile::tempdir().unwrap();

      // Create test artifacts
      let artifact_dir = temp_dir.path().join("stable/arm64");
      std::fs::create_dir_all(&artifact_dir).unwrap();
      std::fs::write(artifact_dir.join("vmlinuz"), vec![0u8; 1024]).unwrap();

      let config = ProviderConfig {
          port: 0,  // Random available port
          artifacts_dir: temp_dir.path().to_path_buf(),
          channel: "stable".to_string(),
          arch: "arm64".to_string(),
      };

      let state = Arc::new(RwLock::new(ProviderState::new(config)));
      let server = ProviderServer::new(state.clone(), 0);

      // Start server and get actual port
      let addr = server.start().await.unwrap();
      let url = format!("http://{}", addr);

      (url, temp_dir)
  }

  #[tokio::test]
  async fn test_health_endpoint() {
      let (url, _dir) = setup_test_server().await;
      let client = Client::new();

      let resp = client.get(format!("{}/health", url))
          .send().await.unwrap();

      assert_eq!(resp.status(), 200);

      let body: serde_json::Value = resp.json().await.unwrap();
      assert_eq!(body["status"], "healthy");
  }

  #[tokio::test]
  async fn test_artifact_download() {
      let (url, _dir) = setup_test_server().await;
      let client = Client::new();

      let resp = client.get(format!("{}/stable/arm64/kernel", url))
          .send().await.unwrap();

      assert_eq!(resp.status(), 200);
      assert_eq!(resp.headers()["content-length"], "1024");

      let bytes = resp.bytes().await.unwrap();
      assert_eq!(bytes.len(), 1024);
  }

  #[tokio::test]
  async fn test_missing_artifact_404() {
      let (url, _dir) = setup_test_server().await;
      let client = Client::new();

      let resp = client.get(format!("{}/stable/x86_64/kernel", url))
          .send().await.unwrap();

      assert_eq!(resp.status(), 404);
  }

  #[tokio::test]
  async fn test_range_request() {
      let (url, _dir) = setup_test_server().await;
      let client = Client::new();

      let resp = client.get(format!("{}/stable/arm64/kernel", url))
          .header("Range", "bytes=0-99")
          .send().await.unwrap();

      assert_eq!(resp.status(), 206);
      assert_eq!(resp.headers()["content-range"], "bytes 0-99/1024");

      let bytes = resp.bytes().await.unwrap();
      assert_eq!(bytes.len(), 100);
  }
  ```

**Dependencies**: M1/Task 4
**Output**: Integration tests

---

## 5.3 Load testing

- [ ] Install `wrk` or `hey` for load testing:
  ```bash
  brew install wrk  # macOS
  # or
  go install github.com/rakyll/hey@latest
  ```
- [ ] Run load test:
  ```bash
  # 10 concurrent connections, 30 seconds
  wrk -t4 -c10 -d30s http://localhost:8080/health

  # Or with hey
  hey -n 10000 -c 10 http://localhost:8080/health
  ```
- [ ] Test large file download concurrency:
  ```bash
  # 5 concurrent downloads of 100MB file
  hey -n 20 -c 5 http://localhost:8080/stable/arm64/rootfs
  ```
- [ ] Expected results:
  - `/health`: >10000 req/sec
  - Large files: Throughput limited by disk/network, not CPU

**Dependencies**: Task 5.2
**Output**: Load test results

---

## 5.4 End-to-end test with phase-fetch

- [ ] Test with actual phase-fetch client:
  ```bash
  # Start provider
  plasmd serve --artifacts /tmp/artifacts --port 8080

  # Create manifest pointing to provider
  cat > /tmp/manifest.json <<EOF
  {
    "version": "2025.11.26",
    "channel": "stable",
    "arch": "arm64",
    "artifacts": {
      "kernel": {
        "hash": "sha256:$(sha256sum /tmp/artifacts/stable/arm64/vmlinuz | cut -d' ' -f1)",
        "size": $(stat -f%z /tmp/artifacts/stable/arm64/vmlinuz),
        "url": "http://localhost:8080/stable/arm64/kernel"
      }
    }
  }
  EOF

  # Fetch using phase-fetch
  phase-fetch --manifest /tmp/manifest.json --output /tmp/fetched --artifact kernel

  # Verify
  diff /tmp/artifacts/stable/arm64/vmlinuz /tmp/fetched/kernel
  ```

**Dependencies**: Task 5.3
**Output**: phase-fetch integration verified

---

## 5.5 Error handling tests

- [ ] Test scenarios:
  ```bash
  # Invalid artifact name
  curl -I http://localhost:8080/stable/arm64/invalid
  # Expected: 404

  # Invalid channel
  curl -I http://localhost:8080/invalid/arm64/kernel
  # Expected: 404

  # Invalid range
  curl -I -H "Range: bytes=invalid" http://localhost:8080/stable/arm64/kernel
  # Expected: 200 (serves full file)

  # Unsatisfiable range
  curl -I -H "Range: bytes=999999999-" http://localhost:8080/stable/arm64/kernel
  # Expected: 416 Range Not Satisfiable
  ```

**Dependencies**: Task 5.2
**Output**: Error handling verified

---

## Validation Checklist

- [ ] All unit tests pass (`cargo test`)
- [ ] All integration tests pass
- [ ] Load test shows acceptable performance
- [ ] phase-fetch can download from provider
- [ ] Error responses correct (404, 416, etc.)
- [ ] No panics under any test scenario
- [ ] Memory usage stable under load
- [ ] No file descriptor leaks
