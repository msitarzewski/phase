# Workers and GPU backends

`lucidd` delegates model execution to a pluggable `Worker` implementation selected via `--worker`. All workers emit streaming `JobEvent` chunks wrapped in Ed25519-signed receipts.

## `llama-cpp` worker (production GGUF inference & embeddings)

Supports every GPU and CPU backend compiled into `llama-server`: Apple Metal, NVIDIA CUDA, AMD ROCm, Vulkan, and CPU.

### Isolation and security hardening

Every `llama-server` subprocess launched by `LlamaCppWorker` enforces:

- **Per-spawn `LLAMA_API_KEY`:** A fresh 256-bit OS CSPRNG hex key passed strictly through the process environment (never command-line arguments) and sent as `Authorization: Bearer <key>` on `/completion`, `/embedding`, and `/apply-template`.
- **Browser and slot lockdown:** Always passes `--cors-origins localhost --no-cors-credentials --no-webui --no-slots`, clears inherited environment variables (`env_clear()`), disables HTTP proxies (`.no_proxy()`), and disables redirects.
- **Cross-tenant KV-cache isolation:** Always passes `--jinja --no-cache-prompt --cache-ram 0 --cache-reuse 0` and injects server-owned `"cache_prompt": false` on every `/completion` call. Client sampling parameters are filtered against an allowlist and rejected if they attempt to smuggle `cache_prompt`, `n_predict`, `prompt`, `messages`, or `stream`.
- **Separate embedding subprocesses:** Embedding jobs launch dedicated `--embeddings --pooling mean` subprocesses keyed separately from generation instances.
- **Path confinement:** Model IDs reject `\0`, `/`, `\`, `..`, and leading `-`, and are canonicalize-confined inside the verified model directory.

### Lifecycle, supervision, and `/api/ps`

- **RAII port allocation:** Subprocesses bind ephemeral ports from `18080..18200` managed by a `PortGuard` that releases ports immediately on drop.
- **Health probes & crash supervision:** Polls `GET /health` every 30 seconds; 5 consecutive failures `SIGKILL` the child. Crashed processes restart with exponential backoff (`1s`, `2s`, `4s`, capped at `8s`); 3 crashes within a rolling 60-second window mark the model terminally failed.
- **Per-request hang kill:** If an SSE stream stalls past the idle timeout (default 30 seconds), `lucidd` kills and reaps the hung `llama-server`, frees its port, and returns an error.
- **Deferred LRU eviction:** Default `max_loaded_models = 3`. A single-flight load gate spawns and health-checks the incoming model *before* evicting the least-recently-used warm model, so a failed cold load never evicts a working model.
- **Honest `/api/ps` residency:** `GET /api/ps` reports only instances currently in the `Healthy` state. Loading, degraded, crashing, or respawning models are omitted, and unmeasured VRAM/expiry fields are omitted rather than fabricated.
- **Linux CUDA 12 auto-discovery:** Automatically discovers installed CUDA 12 shared libraries (including Ollama's bundled CUDA runtime) when launching Linux CUDA `llama-server`.

## `mlx` worker (macOS Apple Silicon)

Runs inference on Apple Silicon (`macos`/`aarch64`) via a pinned `mlx_lm.server` binary and an immutable canonical model bundle. Embeddings are not supported by the MLX worker (use `llama-cpp` for embeddings).

### Bundle integrity and isolation

```bash
lucidd \
  --mode worker \
  --worker mlx \
  --mlx-server-binary /absolute/path/to/mlx_lm.server \
  --mlx-model-bundle /absolute/path/to/readonly-mlx-bundle \
  --mlx-model-alias qwen3-8b-mlx
```

Before starting `mlx_lm.server`, `MlxWorker` verifies:

- **Canonical bundle root CID:** `SHA-256("phase/mlx-bundle-root/v1\0" || sorted relative paths, sizes, and file SHA-256s)`.
- **Strict immutability:** Every file must be read-only (`mode & 0o222 == 0`), `nlink == 1` (no hard links), zero symlinks, and zero executable files or code extensions (`.py`, `.pyc`, `.pyo`, `.so`, `.dylib`, `.dll`, `.sh`).
- **No remote code execution:** Rejects `auto_map`, `custom_pipelines`, and `trust_remote_code` in `config.json`.
- **Restart verification:** Clean restarts perform a fast inode/identity check; any abnormal exit triggers a full content re-hash before restart.
- **Offline subprocess confinement:** Runs with `env_clear()`, `HF_HUB_OFFLINE=1`, `TRANSFORMERS_OFFLINE=1`, `PYTHONNOUSERSITE=1`, `TOKENIZERS_PARALLELISM=false`, `--allowed-origins lucid.invalid`, and `--decode-concurrency 1 --prompt-concurrency 1 --prompt-cache-size 0`, verified with `/health` plus a 1-token warm-up completion.

## `echo` worker (development and CI only)

Explicitly opted in via `--worker echo` (requires no GPU or model files):

- Reverses the last user message (or prompt) character-by-character with a 30 ms per-token delay.
- Emits deterministic L2-normalized 16-dimensional SHA-256 vectors for `/api/embed` and `/api/embeddings`.
- Signs genuine Ed25519 receipts with your node identity so you can test routing, invitations, and receipt verification on any laptop.
- Advertises a synthetic development CID and reports an empty `/api/ps` list.
