# Task 5 — Multi-Provider Test

**Agent**: QA Agent
**Estimated**: 1 day

## 5.1 Setup multiple providers

- [ ] Provider 1 (Mac):
  ```bash
  plasmd serve --artifacts ~/boot-artifacts --port 8080
  ```
- [ ] Provider 2 (Linux VM or another Mac):
  ```bash
  plasmd serve --artifacts /path/to/artifacts --port 8081
  ```
- [ ] Both advertising same channel/arch

**Dependencies**: M5/Tasks 1-4
**Output**: Multiple providers running

---

## 5.2 Test mDNS with multiple providers

- [ ] Discover all providers:
  ```bash
  dns-sd -B _phase-image._tcp local.
  # Should show multiple services:
  # phase-stable-arm64._phase-image._tcp. (Provider 1)
  # phase-stable-arm64-2._phase-image._tcp. (Provider 2)
  ```

**Dependencies**: Task 5.1
**Output**: Multiple mDNS responses

---

## 5.3 Test DHT with multiple providers

- [ ] Both providers publish to DHT:
  ```
  Key: /phase/stable/arm64/manifest
  Values from multiple providers (replicated)
  ```
- [ ] Discovery returns multiple options:
  ```bash
  phase-discover --channel stable --arch arm64 --all
  # Should return URLs from both providers
  ```

**Dependencies**: Task 5.2
**Output**: DHT replication verified

---

## 5.4 Test client provider selection

- [ ] Client should select nearest/fastest:
  ```bash
  # From Phase Boot VM
  phase-discover --channel stable --arch arm64

  # Should prefer LAN provider (mDNS) over WAN (DHT)
  ```
- [ ] Fallback if first provider fails:
  ```bash
  # Stop Provider 1
  # Client should find Provider 2
  ```

**Dependencies**: Task 5.3
**Output**: Provider selection works

---

## 5.5 Test load distribution

- [ ] Multiple clients fetching:
  ```bash
  # Client 1
  phase-fetch --manifest http://provider1:8080/manifest.json --output /tmp/a

  # Client 2 (simultaneously)
  phase-fetch --manifest http://provider2:8081/manifest.json --output /tmp/b
  ```
- [ ] Both succeed, load distributed

**Dependencies**: Task 5.4
**Output**: Load distribution verified

---

## Validation Checklist

- [ ] Multiple providers can run simultaneously
- [ ] mDNS shows all providers
- [ ] DHT has records from all providers
- [ ] Client selects best provider
- [ ] Fallback works when provider fails
- [ ] Load can be distributed
