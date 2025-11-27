# Task 5 — CLI Testing

**Agent**: QA Agent
**Estimated**: 1 day

## 5.1 Test serve command

- [ ] Basic serve:
  ```bash
  # Create test artifacts
  mkdir -p /tmp/test-artifacts/stable/arm64
  dd if=/dev/urandom of=/tmp/test-artifacts/stable/arm64/vmlinuz bs=1M count=1

  # Start provider
  plasmd serve --artifacts /tmp/test-artifacts --port 9090

  # Verify running
  curl http://localhost:9090/health
  curl http://localhost:9090/status | jq

  # Stop with Ctrl+C
  ```

**Dependencies**: M4/Tasks 1-4
**Output**: Serve tested

---

## 5.2 Test config file

- [ ] Test config loading:
  ```bash
  # Create config
  cat > /tmp/provider.toml <<EOF
  [provider]
  port = 9091
  artifacts_dir = "/tmp/test-artifacts"
  channels = ["stable"]
  architectures = ["arm64"]

  [dht]
  enabled = false

  [mdns]
  enabled = true
  EOF

  # Start with config
  plasmd serve --config /tmp/provider.toml

  # Verify port from config
  curl http://localhost:9091/health
  ```

**Dependencies**: Task 5.1
**Output**: Config tested

---

## 5.3 Test CLI arg override

- [ ] Args override config:
  ```bash
  # Config says port 9091, CLI says 9092
  plasmd serve --config /tmp/provider.toml --port 9092

  # Should use 9092
  curl http://localhost:9092/health
  ```

**Dependencies**: Task 5.2
**Output**: Override tested

---

## 5.4 Test status commands

- [ ] While provider running:
  ```bash
  # In background
  plasmd serve --artifacts /tmp/test-artifacts &

  # Test status
  plasmd provider status
  plasmd provider status --json | jq

  # Test list
  plasmd provider list

  # Test keyid
  plasmd provider keyid

  # Cleanup
  kill %1
  ```

**Dependencies**: Task 5.3
**Output**: Status commands tested

---

## 5.5 Test error handling

- [ ] Missing artifacts:
  ```bash
  plasmd serve --artifacts /nonexistent
  # Should error: "Artifacts directory does not exist"
  ```
- [ ] Port in use:
  ```bash
  # Start first instance
  plasmd serve --port 9090 &

  # Try second instance
  plasmd serve --port 9090
  # Should error: "Port 9090 is already in use"
  ```
- [ ] Invalid config:
  ```bash
  echo "invalid toml {{{{" > /tmp/bad.toml
  plasmd serve --config /tmp/bad.toml
  # Should error or use defaults with warning
  ```

**Dependencies**: Task 5.4
**Output**: Errors tested

---

## Validation Checklist

- [ ] `plasmd serve` starts correctly
- [ ] Config file loads
- [ ] CLI args override config
- [ ] Status commands work
- [ ] Clear error messages
- [ ] Graceful shutdown works
