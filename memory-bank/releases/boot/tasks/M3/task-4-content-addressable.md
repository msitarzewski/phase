# Task 4 — Content-Addressable Storage (CAS) Cache


**Agent**: Tooling Agent
**Estimated**: 5 days

#### 4.1 CAS layout design
- [ ] Cache directory structure:
  ```
  /cache/
  ├── manifests/
  │   ├── stable/
  │   │   ├── x86_64/
  │   │   │   ├── latest.json
  │   │   │   └── latest.version
  │   │   └── arm64/
  │   └── testing/
  ├── artifacts/
  │   ├── sha256:abc123.../
  │   │   ├── data (actual file)
  │   │   └── meta.json (size, mtime, refs)
  │   └── sha256:def456.../
  └── cas.db (SQLite or JSON index)
  ```
- [ ] Artifact storage: `/cache/artifacts/<sha256-hash>/data`
- [ ] Metadata: `/cache/artifacts/<sha256-hash>/meta.json`
  - Fields: `size`, `mtime`, `ref_count`, `last_used`

**Dependencies**: M1 Task 1.3 (cache partition created)
**Output**: CAS layout documentation

#### 4.2 Cache initialization script
- [ ] Script: `boot/initramfs/scripts/cache-init.sh` (extend from M2 Task 5.3)
- [ ] Add CAS structure creation:
  - Create `/cache/manifests/`, `/cache/artifacts/`
  - Initialize `cas.db` (simple JSON file for MVP)
- [ ] Skip if Private Mode (PHASE_CACHE=disabled)

**Dependencies**: Task 4.1, M2 Task 5.3
**Output**: Cache initialization script with CAS support

#### 4.3 Cache lookup
- [ ] Function: `cache-lookup <sha256-hash>`
- [ ] Logic:
  - Check if `/cache/artifacts/<hash>/data` exists
  - Verify file integrity (re-hash)
  - Update `last_used` timestamp in metadata
  - Return path if hit, empty if miss
- [ ] Used before fetch to avoid re-downloading

**Dependencies**: Task 4.2
**Output**: Cache lookup script/function

#### 4.4 Cache store
- [ ] Function: `cache-store <sha256-hash> <source-file>`
- [ ] Logic:
  - Create `/cache/artifacts/<hash>/` directory
  - Copy `<source-file>` to `/cache/artifacts/<hash>/data`
  - Create `meta.json` with size, mtime, ref_count=1, last_used=now
  - Update `cas.db` index
- [ ] Skip if Private Mode

**Dependencies**: Task 4.2
**Output**: Cache store script/function

#### 4.5 Garbage collection policy
- [ ] Script: `boot/initramfs/scripts/cache-gc.sh`
- [ ] Policy: LRU (Least Recently Used) eviction
- [ ] Trigger: Cache size exceeds limit (e.g., 80% of partition)
- [ ] Steps:
  - Enumerate all artifacts in `/cache/artifacts/`
  - Sort by `last_used` timestamp (oldest first)
  - Delete artifacts until cache size < threshold
  - Update `cas.db` index
- [ ] Invocation: Optional post-boot background task OR manual via CLI

**Dependencies**: Tasks 4.3, 4.4
**Output**: GC script

#### 4.6 Test cache operations
- [ ] Cache hit test:
  - Store artifact in cache
  - Fetch same artifact (by hash)
  - Expected: Cache hit, no download
- [ ] Cache miss test:
  - Request uncached artifact
  - Expected: Cache miss, downloads from mirror
- [ ] GC test:
  - Fill cache to limit
  - Add new artifact
  - Expected: Oldest artifact evicted

**Dependencies**: All Task 4 items
**Output**: Cache test results

---
