# Task 3 — Range Request Support

**Agent**: Runtime Agent
**Estimated**: 2 days

## 3.1 Understand HTTP Range requests

- [ ] Review RFC 7233 (HTTP Range Requests)
- [ ] Key headers:
  - Request: `Range: bytes=0-1023` (first 1KB)
  - Response: `Content-Range: bytes 0-1023/10485760`
  - Response status: 206 Partial Content
- [ ] Use cases:
  - Resume interrupted downloads
  - Parallel chunk downloads
  - Video/audio streaming (scrubbing)

**Dependencies**: M1/Task 2
**Output**: Understanding documented

---

## 3.2 Create range parsing module

- [ ] Create `daemon/src/provider/range.rs`:
  ```rust
  use std::ops::Range;

  #[derive(Debug, Clone)]
  pub struct ByteRange {
      pub start: u64,
      pub end: u64,  // Inclusive
  }

  impl ByteRange {
      /// Parse Range header value like "bytes=0-1023" or "bytes=1024-"
      pub fn parse(header: &str, file_size: u64) -> Result<Self, RangeError> {
          if !header.starts_with("bytes=") {
              return Err(RangeError::InvalidUnit);
          }

          let range_spec = &header[6..];

          // Handle single range (multi-range not supported)
          if range_spec.contains(',') {
              return Err(RangeError::MultiRangeNotSupported);
          }

          let parts: Vec<&str> = range_spec.split('-').collect();
          if parts.len() != 2 {
              return Err(RangeError::InvalidFormat);
          }

          let start = if parts[0].is_empty() {
              // Suffix range: "-500" means last 500 bytes
              let suffix: u64 = parts[1].parse()
                  .map_err(|_| RangeError::InvalidNumber)?;
              file_size.saturating_sub(suffix)
          } else {
              parts[0].parse().map_err(|_| RangeError::InvalidNumber)?
          };

          let end = if parts[1].is_empty() {
              // Open range: "1024-" means from 1024 to end
              file_size - 1
          } else {
              parts[1].parse().map_err(|_| RangeError::InvalidNumber)?
          };

          // Validate range
          if start > end || start >= file_size {
              return Err(RangeError::NotSatisfiable);
          }

          Ok(Self {
              start,
              end: end.min(file_size - 1),
          })
      }

      pub fn len(&self) -> u64 {
          self.end - self.start + 1
      }
  }

  #[derive(Debug)]
  pub enum RangeError {
      InvalidUnit,
      InvalidFormat,
      InvalidNumber,
      MultiRangeNotSupported,
      NotSatisfiable,
  }
  ```

**Dependencies**: Task 3.1
**Output**: Range parsing module

---

## 3.3 Update artifact handler for ranges

- [ ] Modify `serve_artifact` to handle Range header:
  ```rust
  use axum::http::header::RANGE;

  pub async fn serve_artifact(
      State(state): State<SharedState>,
      headers: HeaderMap,
      Path((channel, arch, artifact)): Path<(String, String, String)>,
  ) -> Result<Response, StatusCode> {
      // ... existing file opening code ...

      let file_size = metadata.len();

      // Check for Range header
      if let Some(range_header) = headers.get(RANGE) {
          let range_str = range_header.to_str()
              .map_err(|_| StatusCode::BAD_REQUEST)?;

          match ByteRange::parse(range_str, file_size) {
              Ok(range) => {
                  return serve_partial(file, range, file_size, content_type).await;
              }
              Err(RangeError::NotSatisfiable) => {
                  return Ok(Response::builder()
                      .status(StatusCode::RANGE_NOT_SATISFIABLE)
                      .header(header::CONTENT_RANGE, format!("bytes */{}", file_size))
                      .body(Body::empty())
                      .unwrap());
              }
              Err(_) => {
                  // Invalid range, serve full file
                  tracing::warn!("Invalid range header, serving full file");
              }
          }
      }

      // Serve full file (existing code)
      // ...
  }
  ```

**Dependencies**: Task 3.2
**Output**: Handler checks for Range header

---

## 3.4 Implement partial content response

- [ ] Add `serve_partial` function:
  ```rust
  async fn serve_partial(
      mut file: File,
      range: ByteRange,
      file_size: u64,
      content_type: &str,
  ) -> Result<Response, StatusCode> {
      use tokio::io::{AsyncReadExt, AsyncSeekExt};

      // Seek to start position
      file.seek(std::io::SeekFrom::Start(range.start)).await
          .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

      // Create bounded reader
      let reader = file.take(range.len());
      let stream = ReaderStream::new(reader);
      let body = Body::from_stream(stream);

      Ok(Response::builder()
          .status(StatusCode::PARTIAL_CONTENT)
          .header(header::CONTENT_TYPE, content_type)
          .header(header::CONTENT_LENGTH, range.len())
          .header(header::CONTENT_RANGE,
              format!("bytes {}-{}/{}", range.start, range.end, file_size))
          .header(header::ACCEPT_RANGES, "bytes")
          .body(body)
          .unwrap())
  }
  ```
- [ ] Add `Accept-Ranges: bytes` to full file responses too

**Dependencies**: Task 3.3
**Output**: Partial content serving

---

## 3.5 Test range requests

- [ ] Create test file:
  ```bash
  dd if=/dev/urandom of=/tmp/artifacts/stable/arm64/vmlinuz bs=1M count=100
  sha256sum /tmp/artifacts/stable/arm64/vmlinuz  # Note hash
  ```
- [ ] Test various range requests:
  ```bash
  # First 1MB
  curl -H "Range: bytes=0-1048575" -o /tmp/chunk1 http://localhost:8080/kernel

  # Second 1MB
  curl -H "Range: bytes=1048576-2097151" -o /tmp/chunk2 http://localhost:8080/kernel

  # Last 1MB
  curl -H "Range: bytes=-1048576" -o /tmp/last http://localhost:8080/kernel

  # From offset to end
  curl -H "Range: bytes=99000000-" -o /tmp/tail http://localhost:8080/kernel

  # Verify status 206
  curl -I -H "Range: bytes=0-1023" http://localhost:8080/kernel

  # Verify invalid range returns 416
  curl -I -H "Range: bytes=999999999-" http://localhost:8080/kernel
  ```
- [ ] Verify chunks can be reassembled:
  ```bash
  cat /tmp/chunk1 /tmp/chunk2 ... > /tmp/reassembled
  sha256sum /tmp/reassembled  # Should match original
  ```

**Dependencies**: Task 3.4
**Output**: Range requests working

---

## 3.6 Add resume download test

- [ ] Simulate interrupted download with curl:
  ```bash
  # Start download, interrupt with Ctrl+C
  curl -o /tmp/partial http://localhost:8080/rootfs
  # Ctrl+C after ~50%

  # Resume
  curl -C - -o /tmp/partial http://localhost:8080/rootfs

  # Verify complete
  sha256sum /tmp/partial
  ```

**Dependencies**: Task 3.5
**Output**: Resume functionality verified

---

## Validation Checklist

- [ ] Range header parsing correct for all formats
- [ ] 206 Partial Content for valid ranges
- [ ] 416 Range Not Satisfiable for invalid ranges
- [ ] Content-Range header correct
- [ ] Accept-Ranges: bytes in all responses
- [ ] Partial downloads can be reassembled
- [ ] curl -C (resume) works correctly
- [ ] Metrics track partial vs full downloads
