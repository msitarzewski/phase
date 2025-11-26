# Task 3 — Artifact Fetcher


**Agent**: Transport Agent
**Estimated**: 7 days

#### 3.1 Fetch binary: phase-fetch
- [ ] Rust binary: `boot/tools/fetch/`
- [ ] CLI: `phase-fetch --url <URL> --hash <sha256> --output <path> [--mirrors <URLs>] [--ipfs-gateway <URL>]`
- [ ] Responsibilities:
  - Download artifact from URL
  - Verify SHA256 hash matches expected
  - Retry on failure with exponential backoff
  - Try mirror URLs if primary fails
  - Fallback to IPFS gateway (CID mode)
  - Output artifact to specified path

**Dependencies**: None (standalone fetcher)
**Output**: Fetcher binary source code

#### 3.2 HTTPS fetch with retry logic
- [ ] HTTP client: Use `reqwest` crate with TLS
- [ ] Retry policy:
  - Max retries: 3
  - Backoff: 2^n seconds (1s, 2s, 4s)
  - Retry on: Connection errors, 5xx server errors
  - No retry on: 4xx client errors (bad URL)
- [ ] Progress reporting: Print download progress to console (MB/s, %)
- [ ] Timeout: 60 seconds per attempt

**Dependencies**: Task 3.1
**Output**: HTTPS fetch implementation

#### 3.3 Mirror fallback
- [ ] Mirror list: Comma-separated URLs in CLI or config file
  - Example: `--mirrors https://mirror1.phase.io/,https://mirror2.phase.io/`
- [ ] Logic:
  - Try primary URL first
  - On failure, iterate through mirrors in order
  - Stop on first success
  - If all mirrors fail, proceed to IPFS fallback

**Dependencies**: Task 3.2
**Output**: Mirror fallback logic

#### 3.4 IPFS gateway fallback
- [ ] IPFS gateway: Public gateway (e.g., `https://ipfs.io/ipfs/<CID>`)
- [ ] CID extraction:
  - Manifest may include `ipfs_cid` field for artifacts
  - If no CID, skip IPFS fallback
- [ ] Logic:
  - If HTTPS mirrors exhausted, try IPFS gateway
  - Fetch via `https://ipfs.io/ipfs/<CID>`
  - Same retry policy as HTTPS
- [ ] Private Mode: Allow IPFS (anonymous, no identity leakage)

**Dependencies**: Task 3.3
**Output**: IPFS fallback implementation

#### 3.5 Hash verification
- [ ] After download completes:
  - Compute SHA256 hash of downloaded file
  - Compare with expected hash (from manifest)
  - If mismatch: Delete file, log error, return failure
  - If match: Move to final output path, return success
- [ ] Use `sha2` crate for hashing

**Dependencies**: Tasks 3.2-3.4
**Output**: Hash verification logic

#### 3.6 Build phase-fetch (static, dual-arch)
- [ ] Build script: `boot/tools/fetch/build.sh`
- [ ] Cross-compile:
  - `cargo build --release --target x86_64-unknown-linux-musl`
  - `cargo build --release --target aarch64-unknown-linux-musl`
- [ ] Statically link OpenSSL: `OPENSSL_STATIC=1`
- [ ] Strip symbols: `strip phase-fetch`
- [ ] Verify size: <5MB per binary
- [ ] Install to initramfs: `boot/initramfs/bin/phase-fetch`

**Dependencies**: Task 3.5
**Output**: Static fetch binaries (x86_64, arm64)

#### 3.7 Test artifact fetch
- [ ] Valid fetch test:
  - Fetch artifact with correct hash
  - Expected: Success, file matches hash
- [ ] Hash mismatch test:
  - Serve corrupted artifact
  - Expected: Failure, file deleted
- [ ] Mirror fallback test:
  - Primary URL returns 500 error
  - Expected: Fetches from mirror successfully
- [ ] IPFS fallback test:
  - All HTTPS mirrors down
  - Expected: Fetches from IPFS gateway

**Dependencies**: Task 3.6
**Output**: Fetch test results

---
