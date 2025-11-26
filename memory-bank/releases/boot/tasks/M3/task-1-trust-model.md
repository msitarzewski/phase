# Task 1 — Trust Model & Key Management


**Agent**: Security Agent
**Estimated**: 5 days

#### 1.1 Define TUF-inspired trust model
- [ ] Document: `boot/docs/trust-model.md`
- [ ] Key hierarchy:
  - **Root key** (offline, cold storage):
    - Purpose: Sign targets key
    - Rotation: Annual or on compromise
    - Storage: Hardware token, encrypted backup
  - **Targets key** (online, automated signing):
    - Purpose: Sign manifest files
    - Rotation: Quarterly or on compromise
    - Storage: CI/CD system, encrypted at rest
- [ ] Trust chain:
  - Boot media embeds root public key (hardcoded)
  - Root key signs targets public key
  - Targets key signs manifest files
  - Verification: manifest → targets key → root key

**Dependencies**: None
**Output**: Trust model documentation

#### 1.2 Generate test keys
- [ ] Script: `boot/scripts/keygen.sh`
- [ ] Generate Ed25519 keypairs:
  - Root keypair: `root.priv`, `root.pub`
  - Targets keypair: `targets.priv`, `targets.pub`
- [ ] Sign targets key with root key:
  - Message: `targets.pub` content
  - Signature: `targets.pub.sig`
- [ ] Store root.pub in: `boot/configs/keys/root.pub` (embedded in binaries)
- [ ] Store targets.pub + sig in: Boot media or fetch separately (TBD)

**Dependencies**: Task 1.1
**Output**: Test keypairs for development

#### 1.3 Key distribution strategy
- [ ] **Root public key**: Embed in `phase-verify` binary (compile-time)
- [ ] **Targets public key**: Fetch from well-known URL OR embed (for simplicity)
  - Option A: Embed targets.pub + sig in boot media (simpler, static trust)
  - Option B: Fetch targets.pub on first boot, verify with root key (flexible, updates)
  - **Decision**: Option A for MVP, Option B for production (M7)
- [ ] Document key update procedure for future rotations

**Dependencies**: Task 1.2
**Output**: Key distribution strategy document

---
