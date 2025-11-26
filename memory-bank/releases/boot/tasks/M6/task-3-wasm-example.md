# Task 3 — WASM Example Publication


**Agent**: Runtime Agent, Networking Agent
**Estimated**: 3 days

#### 3.1 Prepare hello.wasm artifact
- [ ] Source: `wasm-examples/hello/` from Phase MVP
- [ ] Build: `cargo build --release --target wasm32-wasi`
- [ ] Binary: `wasm-examples/hello/target/wasm32-wasi/release/hello.wasm`
- [ ] Verify size: ~84KB (from MVP metrics)
- [ ] Test locally: `plasmd execute-job --wasm hello.wasm --args "Hello, World!"` → "dlroW ,olleH"

**Dependencies**: None (leverages existing wasm-examples/)
**Output**: hello.wasm binary

#### 3.2 Create WASM job manifest
- [ ] File: `hello-job-manifest.json`
- [ ] Schema (similar to M3 artifact manifest):
  ```json
  {
    "version": "0.1",
    "job_id": "hello-job-v0.1",
    "module": {
      "hash": "sha256:abc123...",
      "size": 84000,
      "url": "https://mirror.phase.io/wasm/hello.wasm",
      "ipfs_cid": "QmABC123..."
    },
    "resources": {
      "memory_mb": 128,
      "cpu_seconds": 5,
      "timeout_seconds": 10
    },
    "arguments": ["Hello, World!"],
    "metadata": {
      "name": "Hello Job",
      "description": "Simple string reversal WASM example"
    }
  }
  ```
- [ ] Sign manifest: Use M3 signing tool (targets key)

**Dependencies**: Task 3.1, M3 Task 2.2 (signing)
**Output**: Signed WASM job manifest

#### 3.3 Publish to DHT and IPFS
- [ ] IPFS:
  - Add hello.wasm: `ipfs add hello.wasm` → Get CID
  - Add manifest: `ipfs add hello-job-manifest.json` → Get CID
  - Pin both: Pinata, Web3.Storage, or self-hosted
- [ ] DHT (libp2p):
  - Provider: Run `phase-manifest-advertise` (from M2) to advertise manifest on DHT
  - Key: `/phase/wasm/hello-job/manifest`
  - Value: Manifest CID or HTTPS URL
- [ ] Update manifest with CIDs in `ipfs_cid` field

**Dependencies**: Task 3.2, M2 Task 3.6 (DHT client)
**Output**: Published WASM job on DHT/IPFS

---
