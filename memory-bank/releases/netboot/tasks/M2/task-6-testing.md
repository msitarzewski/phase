# Task 6 — Testing & Validation

**Agent**: QA Agent
**Estimated**: 2 days

## 6.1 Verify with phase-verify

- [ ] Test that generated manifests pass phase-verify:
  ```bash
  # Generate manifest
  curl http://localhost:8080/stable/arm64/manifest.json > /tmp/manifest.json

  # Extract public key from plasmd
  plasmd provider keyid > /tmp/pubkey.txt

  # Verify with phase-verify
  phase-verify \
    --manifest /tmp/manifest.json \
    --pubkey /tmp/pubkey.txt

  # Expected: "Manifest signature valid"
  ```

**Dependencies**: M2/Task 5
**Output**: phase-verify compatibility confirmed

---

## 6.2 Test manifest schema compliance

- [ ] Validate against JSON schema:
  ```bash
  # Get manifest
  curl http://localhost:8080/manifest.json > /tmp/manifest.json

  # Validate with jq (basic checks)
  jq '.manifest_version == 1' /tmp/manifest.json
  jq '.artifacts.kernel.hash | startswith("sha256:")' /tmp/manifest.json
  jq '.signatures | length > 0' /tmp/manifest.json
  jq '.expires_at' /tmp/manifest.json  # Should be future date
  ```
- [ ] Create JSON schema file for automated validation (optional)

**Dependencies**: Task 6.1
**Output**: Schema compliance verified

---

## 6.3 Test signature tampering detection

- [ ] Tamper with manifest and verify rejection:
  ```bash
  # Get manifest
  curl http://localhost:8080/manifest.json > /tmp/manifest.json

  # Tamper with version
  jq '.version = "tampered"' /tmp/manifest.json > /tmp/tampered.json

  # Verify should fail
  phase-verify --manifest /tmp/tampered.json --pubkey /tmp/pubkey.txt
  # Expected: "Signature verification failed"
  ```

**Dependencies**: Task 6.2
**Output**: Tamper detection verified

---

## 6.4 Test hash verification

- [ ] Verify artifact hashes match actual files:
  ```bash
  # Get manifest
  curl http://localhost:8080/manifest.json > /tmp/manifest.json

  # Extract kernel hash from manifest
  EXPECTED=$(jq -r '.artifacts.kernel.hash' /tmp/manifest.json)

  # Download kernel and compute hash
  curl -o /tmp/kernel http://localhost:8080/kernel
  ACTUAL="sha256:$(sha256sum /tmp/kernel | cut -d' ' -f1)"

  # Compare
  [ "$EXPECTED" = "$ACTUAL" ] && echo "Hash matches" || echo "Hash mismatch!"
  ```

**Dependencies**: Task 6.3
**Output**: Hash verification working

---

## 6.5 Integration test with phase-fetch

- [ ] Full fetch flow using generated manifest:
  ```bash
  # Start provider
  plasmd serve --artifacts /tmp/artifacts --port 8080 &

  # Create output directory
  mkdir -p /tmp/fetched

  # Fetch all artifacts using manifest
  phase-fetch \
    --manifest http://localhost:8080/manifest.json \
    --output /tmp/fetched \
    --artifact all

  # Verify files exist
  ls -la /tmp/fetched/

  # Verify hashes
  sha256sum /tmp/fetched/*
  ```

**Dependencies**: Task 6.4
**Output**: phase-fetch integration verified

---

## 6.6 Test manifest expiration

- [ ] Create expired manifest and verify rejection:
  ```rust
  #[tokio::test]
  async fn test_expired_manifest_rejected() {
      let mut manifest = sample_manifest();
      manifest.expires_at = "2020-01-01T00:00:00Z".to_string();

      let result = manifest.validate();
      assert!(matches!(result, Err(ManifestError::Expired)));
  }
  ```
- [ ] Verify phase-verify rejects expired manifests

**Dependencies**: Task 6.5
**Output**: Expiration handling verified

---

## 6.7 Performance tests

- [ ] Measure manifest generation time:
  ```bash
  # Time manifest generation with various artifact sizes
  time curl http://localhost:8080/stable/arm64/manifest.json

  # First request (cold): <500ms acceptable
  # Cached request: <10ms acceptable
  ```
- [ ] Measure hash computation for large rootfs:
  ```bash
  # Create 500MB test rootfs
  dd if=/dev/urandom of=/tmp/artifacts/stable/arm64/rootfs.sqfs bs=1M count=500

  # Time hash computation
  time curl http://localhost:8080/stable/arm64/manifest.json
  ```

**Dependencies**: Task 6.6
**Output**: Performance benchmarks

---

## Validation Checklist

- [ ] phase-verify accepts generated manifests
- [ ] Manifest JSON matches expected schema
- [ ] Signature verification works correctly
- [ ] Tampered manifests are rejected
- [ ] Artifact hashes match actual files
- [ ] phase-fetch can use generated manifests
- [ ] Expired manifests are rejected
- [ ] Performance is acceptable (<500ms cold, <10ms cached)
- [ ] All unit tests pass
- [ ] All integration tests pass
