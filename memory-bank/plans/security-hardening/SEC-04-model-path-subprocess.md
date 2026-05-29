# SEC-04 — Model-path traversal + subprocess hardening

**Severity:** 🟠 HIGH (H1) + 🔵 L8 | **Phase:** 2 | **Effort:** small (1 agent-wave) | **Status:** planned

## Why
`resolve_model_path` (`worker_llama.rs:363-374`) does no validation:
- absolute `model_id` (`"/etc/shadow"`) is returned verbatim;
- relative (`"../../etc/passwd"`) escapes `model_dir` because `Path::join` doesn't normalize `..`.

Reachable from the local Ollama HTTP API (`ollama.rs:312,533`) with zero validation. The relay path is gated by the model-loaded check (`router.rs:410`) so remote peers can't reach it directly — but a local process, or anyone if `LUCIDD_HOST=0.0.0.0`, gets: a filesystem existence + partial-content **oracle** (distinct errors at `worker_llama.rs:235-238` vs `:701-708`), and the ability to feed arbitrary files into llama.cpp's C++ GGUF parser (mmap/parse surface with CVE history).

Plus L8: the subprocess inherits lucidd's **entire environment** (no `env_clear()`) and resolves `llama-server` via inherited `$PATH` (`worker_llama.rs:386,126`) — env-leak + binary-hijack risk.

## Scope
- `crates/lucidd/src/worker_llama.rs` — `resolve_model_path`, the spawn at `~386`, config default at `~126`.

## Approach
1. Replace `resolve_model_path` with a confining version:
   ```rust
   fn resolve_model_path(model_dir: &Path, model_id: &str) -> Result<PathBuf, WorkerError> {
       if model_id.is_empty()
           || model_id.contains('\0')
           || model_id.contains('/')
           || model_id.contains('\\')
           || model_id.contains("..")
           || model_id.starts_with('-')          // also closes the leading-`--` arg-injection question
       {
           return Err(WorkerError::ArtifactUnavailable("invalid model id".into()));
       }
       let candidate = model_dir.join(format!("{model_id}.gguf"));
       let canon = candidate.canonicalize()
           .map_err(|_| WorkerError::ArtifactUnavailable("not found".into()))?;
       let base = model_dir.canonicalize()
           .map_err(|_| WorkerError::Other("bad model_dir".into()))?;
       if !canon.starts_with(&base) {
           return Err(WorkerError::ArtifactUnavailable("outside model dir".into()));
       }
       Ok(canon)
   }
   ```
2. **Drop the absolute-path passthrough.** If test fixtures relied on it, move them to a dev-only path via `extra_env` or a `#[cfg(test)]` hook.
3. **Collapse the error oracle:** return a single generic "model unavailable" to the client for both not-found and parse-failure; log the detail server-side only.
4. **Subprocess hardening:** `cmd.env_clear()` then set only required vars (PATH to a minimal known value, plus anything llama-server genuinely needs); require `server_binary_path` to be **absolute** (reject relative/`$PATH`-resolved default, or canonicalize + existence-check it at startup).

## Acceptance criteria
- `model_id` containing `/`, `..`, `\0`, leading `-`, or absolute paths → rejected, never reaches spawn.
- Not-found and parse-failure return the same generic client error (oracle closed).
- Subprocess runs with a cleaned environment and an absolute binary path.
- Legitimate `model_id` (`"qwen3"`) still resolves to `model_dir/qwen3.gguf` and runs.

## Test plan
- Unit tests on `resolve_model_path`: `"/etc/passwd"`, `"../../etc/passwd"`, `"a/b"`, `"--flag"`, `"x\0y"`, `""` all `Err`; `"qwen3"` → `Ok(model_dir/qwen3.gguf)` (use a tempdir + touch a real file for the canonicalize check).
- Test that two different bad inputs yield the same client-facing error string.

## Dependencies
None. Parallel-safe with SEC-07 (same file, different functions — coordinate review).
