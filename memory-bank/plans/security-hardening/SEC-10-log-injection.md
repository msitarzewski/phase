# SEC-10 — Ollama API log-injection / URI sanitization

**Severity:** 🟡 MEDIUM (M6) | **Phase:** 3 | **Effort:** trivial (well under 1 agent-wave) | **Status:** planned

## Why
The fallback handler `unknown()` (`ollama.rs:212-215`) logs `uri = %req.uri()` at WARN with no sanitization. With `LUCIDD_HOST=0.0.0.0` (a documented gotcha — the API is then unauthenticated on the network), a request path containing CRLF / ANSI escape sequences (e.g. `/api/%0d%0afake-log-line` or terminal-escape payloads) is written verbatim into logs → log forging / injection against text log sinks, and ANSI-escape abuse against anyone tailing logs in a terminal. No reflected XSS (404 body is empty), so this is log-integrity, not RCE.

## Scope
- `crates/lucidd/src/ollama.rs` — the `unknown()` fallback handler (and any other handler that logs request-derived strings).

## Approach
1. Log only `uri.path()` (not query/fragment), percent-decoded then **control-character-stripped** (drop bytes `< 0x20` and `0x7f`, and the CSI/escape introducer), and **length-capped** (e.g. 256 chars).
2. Audit other `tracing` calls in `ollama.rs`/`router.rs` for unsanitized attacker-derived fields (model names, headers) logged at info/warn — apply the same sanitize-or-cap.
3. Independently: reinforce that binding `0.0.0.0` without auth is dangerous — at minimum a loud startup WARN when `LUCIDD_HOST` is non-loopback (may already exist; verify). This is defense-in-depth, not the core fix.

## Acceptance criteria
- A request path with embedded CRLF/ANSI bytes produces a single sanitized log line (no forged second line, no raw escapes).
- URI logging is length-capped.
- Non-loopback bind emits a clear security warning at startup.

## Test plan
- Unit test on the sanitizer: input with `\r\n`, `\x1b[`, NUL, and a 10 KB path → output is single-line, escape-free, ≤ cap.
- Manual: `curl --path-as-is 'http://127.0.0.1:11434/api/%0d%0aFAKE'` → log shows one sanitized line.

## Dependencies
None. `ollama.rs` only; fully parallel.
