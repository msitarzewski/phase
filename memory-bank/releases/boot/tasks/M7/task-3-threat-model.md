# Task 3 — Threat Model


**Agent**: Security Agent
**Estimated**: 7 days

#### 3.1 Threat modeling framework
- [ ] Document: `boot/docs/threat-model.md`
- [ ] Framework: STRIDE (Spoofing, Tampering, Repudiation, Information Disclosure, DoS, Elevation of Privilege)
- [ ] Scope:
  - Assets: Boot image, manifests, WASM jobs, receipts, network communications
  - Adversaries: Network attacker, local attacker, compromised mirror, malicious peer
  - Goals: Confidentiality, Integrity, Availability, Non-repudiation

**Dependencies**: None
**Output**: Threat model framework

#### 3.2 Attack scenarios and mitigations
- [ ] **Tampering: Manifest modification**
  - Scenario: Attacker modifies manifest to point to malicious kernel
  - Impact: High (malicious code execution)
  - Mitigation: Ed25519 signatures, offline root keys (M3)
  - Residual risk: Key compromise (requires key rotation)

- [ ] **Tampering: Artifact corruption**
  - Scenario: Mirror serves corrupted or malicious kernel
  - Impact: High (malicious code execution)
  - Mitigation: SHA256 hash verification (M3)
  - Residual risk: Hash collision (negligible with SHA256)

- [ ] **Rollback: Downgrade attack**
  - Scenario: Attacker serves old manifest with known vulnerabilities
  - Impact: Medium (exploitation of old vulnerabilities)
  - Mitigation: Manifest version counter, rollback protection (M3)
  - Residual risk: First boot (no cached version to compare)

- [ ] **Spoofing: MITM on network**
  - Scenario: Attacker intercepts network traffic, serves malicious manifest/artifacts
  - Impact: High (malicious code execution)
  - Mitigation: QUIC + Noise encryption (M2), HTTPS for mirrors (M3), signature verification (M3)
  - Residual risk: Compromised CA (for HTTPS), user disables verification (not supported)

- [ ] **Information Disclosure: Private Mode leakage**
  - Scenario: Private Mode writes persistent data, leaking user activity
  - Impact: Medium (privacy violation)
  - Mitigation: tmpfs overlay, no cache writes (M4), ephemeral identity (M2)
  - Residual risk: Network traffic analysis (IP exposure), firmware-level logging

- [ ] **DoS: Mirror unavailability**
  - Scenario: All mirrors down, user cannot fetch artifacts
  - Impact: Low (boot fails, but no security compromise)
  - Mitigation: Multiple mirrors, IPFS fallback (M3)
  - Residual risk: Global network outage (accept risk)

- [ ] **DoS: Resource exhaustion (WASM)**
  - Scenario: Malicious WASM job consumes excessive CPU/memory
  - Impact: Low (Plasm crash, but sandboxed)
  - Mitigation: Resource limits (memory, CPU, timeout) (M6)
  - Residual risk: Algorithmic complexity attacks (accept risk for MVP)

- [ ] **Elevation of Privilege: Sandbox escape**
  - Scenario: WASM job escapes sandbox, gains host access
  - Impact: Critical (arbitrary code execution on host)
  - Mitigation: Wasmtime sandboxing, no syscall access (M6)
  - Residual risk: Wasmtime vulnerability (monitor CVEs, update regularly)

- [ ] **Repudiation: Receipt forgery**
  - Scenario: Node claims job executed but didn't (fake receipt)
  - Impact: Medium (false claims of work)
  - Mitigation: Ed25519 receipt signatures (M6)
  - Residual risk: Compromised node key (future: reputation system)

- [ ] **Physical: Evil Maid attack**
  - Scenario: Attacker with physical access modifies USB stick
  - Impact: High (malicious boot)
  - Mitigation: Secure Boot with owner keys (M7), TPM sealing (future)
  - Residual risk: User disables Secure Boot, no physical security

**Dependencies**: M1-M6 (attack surfaces)
**Output**: Attack scenarios with STRIDE classification

#### 3.3 Trust boundaries
- [ ] Identify trust boundaries:
  - UEFI firmware ↔ Bootloader: Secure Boot boundary
  - Bootloader ↔ Seed kernel: Verified boot (future)
  - Seed initramfs ↔ Network: Discovery + verification boundary
  - Network ↔ Mirrors: TLS + signature verification boundary
  - Target OS ↔ WASM jobs: Sandbox boundary
- [ ] Diagram: Trust zones and boundaries

**Dependencies**: Task 3.2
**Output**: Trust boundaries diagram

#### 3.4 Residual risks and acceptance
- [ ] Document accepted risks (out of scope for MVP):
  - **Firmware-level attacks**: UEFI rootkits, bootkits (requires Secure Boot + measured boot)
  - **Supply chain attacks**: Compromised build infrastructure (requires reproducible builds + attestation)
  - **Side-channel attacks**: Timing, cache, speculative execution (accept for MVP)
  - **Physical attacks**: DMA attacks, hardware implants (requires physical security policies)
  - **Network traffic analysis**: Metadata leakage in Private Mode (future: Tor integration)
- [ ] Rationale: MVP focuses on integrity and basic confidentiality, not adversarial environments

**Dependencies**: Task 3.2
**Output**: Residual risks documentation

---
