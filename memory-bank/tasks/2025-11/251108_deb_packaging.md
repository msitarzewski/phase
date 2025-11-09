## Task: Create Debian package using cargo-deb

### Context
- **Repository**: root
- **Related Work**: `release_plan.yaml` → matching milestone
- **Constraints**: Open-core only (no commercial code); no timelines; public repo
- **Affected Systems**: daemon (Rust), php-sdk (PHP), examples, docs

### Expected Outcomes
- **Acceptance Criteria**:
  1. Produce plasm_<version>_amd64.deb; postinst enables systemd service.
  2. Artifacts committed under appropriate directories
- **Success Metrics**: Manual demo flows; deterministic outputs; clean lint/build

### Definition of Done
- Code compiles; minimal tests pass; example runs; documented in README sections

### Architectural Constraints
- **Must Follow**: WASM-only execution; default-deny policy; encrypted transport
- **Must Extend**: Repo structure established in this MVP
- **Must Not**: Introduce commercial billing code; rely on centralized discovery

### Instructions
Create outline for approval. After approval, do work. Do not document Memory Bank until code is approved.
