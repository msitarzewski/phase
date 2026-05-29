# SEC-08 — Atomic, mode-0600 private-key write

**Severity:** 🟡 MEDIUM (M3) | **Phase:** 3 | **Effort:** small (1 agent-wave) | **Status:** planned

## Why
`write_secret` (`storage.rs:67-86`) does `fs::write(path, secret)` **then** `set_permissions(0o600)`. `fs::write` creates the file at umask default (typically 0o644), so between those two lines the **Ed25519 private key is world-readable on disk**. There's also no temp-file + atomic rename (a crash mid-write leaves a truncated key), and `load_or_create` (`keypair.rs:50-60`) has a TOCTOU race between the `load` check and the `save`. The existing mode-0600 test (`storage.rs:141`) only asserts the *final* state, missing the window.

## Scope
- `crates/phase-identity/src/storage.rs` — `write_secret`.
- `crates/phase-identity/src/keypair.rs` — `load_or_create` race.

## Approach
1. Create the file with the right mode **atomically** from the start:
   ```rust
   use std::os::unix::fs::OpenOptionsExt;
   let mut f = OpenOptions::new()
       .write(true).create_new(true).mode(0o600)
       .open(&tmp_path)?;      // tmp in the same dir
   f.write_all(secret)?;
   f.sync_all()?;
   fs::rename(&tmp_path, &final_path)?;   // atomic on same filesystem
   ```
   `create_new(true)` fails if the file exists — which also closes the `load_or_create` race: the creator that wins `create_new` writes; a loser gets `AlreadyExists` and falls back to `load`.
2. Set the parent dir to `0o700` when creating it (currently `create_dir_all` uses 0o755 — `~/.config/phase` is world-traversable; minor but cheap, see L-note).
3. Windows: `create_new` works; the Unix `.mode()` is `cfg(unix)`-gated (ACL handling on Windows is out of scope for v0.1, document it).
4. Strengthen the test to assert the file is **never** observable at a non-0600 mode — hard to test the race directly, but at minimum assert `create_new` semantics and that no 0644 window exists by construction (the mode is set at open, not after).

## Acceptance criteria
- The private key file is created mode 0600 with no world-readable window (mode set at `open`, not post-write).
- Write is atomic (temp + rename); a crash can't leave a partial key at the real path.
- Concurrent `load_or_create` on the same path can't double-generate or corrupt (one creator wins).
- Parent dir is 0o700.

## Test plan
- Test: after `save`, file mode == 0600 (existing) **plus** assert the implementation uses `create_new`+`mode` (code-level / no post-hoc chmod).
- Test: `load_or_create` called twice concurrently (two threads/tasks) on a fresh path → both return the same key, exactly one generation occurred.
- Test: simulated partial write (write to temp, don't rename) leaves the real path absent, not truncated.

## Dependencies
None. Fully parallel — `phase-identity` only.
