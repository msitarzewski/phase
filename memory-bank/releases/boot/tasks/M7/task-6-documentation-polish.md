# Task 6 — Documentation Polish & Publishing


**Agent**: Docs Agent
**Estimated**: 4 days

#### 6.1 Review and polish
- [ ] Grammar and spelling: Proofread all documentation
- [ ] Consistency: Terminology, formatting, code blocks
- [ ] Clarity: Ensure step-by-step instructions clear and unambiguous
- [ ] Completeness: Check all cross-references, links valid
- [ ] Peer review: Internal review (if team), external beta tester review

**Dependencies**: All Task 1-5 documents
**Output**: Polished documentation

#### 6.2 Generate table of contents
- [ ] Script: `boot/scripts/generate-toc.sh`
- [ ] Auto-generate TOCs for long documents (threat model, troubleshooting)
- [ ] Update `boot/docs/README.md` with complete doc tree

**Dependencies**: Task 6.1
**Output**: Auto-generated TOCs

#### 6.3 Publish to GitHub
- [ ] Commit all docs to `boot/docs/`
- [ ] Update main `README.md` with link to Phase Boot docs
- [ ] Tag release: `git tag -s v0.1.0-docs`
- [ ] Push to GitHub: Docs appear in repository

**Dependencies**: Task 6.2
**Output**: Documentation published on GitHub

#### 6.4 Website integration (optional)
- [ ] If Phase website exists:
  - Export docs to website format (markdown → HTML)
  - Publish to `https://phase.io/docs/boot/`
  - Add download page with links to images, checksums, signatures
  - SEO: Quickstart landing page, architecture overview
- [ ] If no website: GitHub docs sufficient for MVP

**Dependencies**: Task 6.3
**Output**: Website documentation (optional)

---
