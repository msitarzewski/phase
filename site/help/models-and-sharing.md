# Models, Ollama import and sharing your GPU

LUCID discovers existing local GGUF models without copying or modifying them, lets you opt in to serving specific models to your circle, and can download verified models peer-to-peer with exact-offset resume.

## Automatic read-only model discovery

When `lucidd probe` or LUCID Desktop inspects your machine, it performs a strictly read-only scan of standard locations (skipping symlinks and dotfiles, verifying the 4-byte `GGUF` magic header, capped at 64 models and depth 4):

- **Ollama:** `~/.ollama/models/manifests` and `~/.ollama/models/blobs` (parses JSON manifest layer `application/vnd.ollama.image.model`)
- **LM Studio:** `~/.lmstudio/models` and `~/.cache/lm-studio/models`
- **Local LUCID folders:** `~/Models`, `<data_local_dir>/lucidd/models`, `<data_local_dir>/lucid/models`, plus any folders you explicitly add in `node.toml` or Desktop's **+ Add model folder** button

> [!IMPORTANT]
> Folder validation fails closed on relative paths, `..`, symlinks, system roots (`/`, `/etc`, `/usr`, `/System`, `/Library`, `/proc`, `/sys`, `/dev`), `$HOME` root, and sensitive directories (`.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker`, Keychains, `.config/phase`, `.config/lucidd`).

## Installing or pinning the `llama.cpp` runtime

To run GGUF models locally, LUCID uses a SHA-256-pinned `llama.cpp` release (`b12651`):

- **Desktop app:** Clicking **Provide…** or **Set up runtime** prompts for explicit confirmation (`confirm_permission: true`), then either pins an already-installed `llama-server` binary or downloads the official `b12651` archive for your hardware (`macos-arm64-metal`, `linux-x86_64-cuda`, `linux-x86_64-vulkan`, `linux-x86_64-cpu`, or `linux-aarch64-cpu`).
- **Verification:** Downloads require HTTPS, enforce a 320 MiB cap, verify exact SHA-256 **before** extracting, inspect every `tar` entry for traversal, extract into a mode-`0700` directory, and record `llama_server_binary` and `llama_server_sha256` in `node.toml`.
- **Tamper detection:** On every startup, `lucidd` re-hashes the configured `llama-server` binary and refuses to start the worker if the SHA-256 does not match (probe status reports `"tampered"`).

## Opting in to share your GPU

Sharing is off by default (`providing_enabled = false` in `node.toml`).

- **Desktop app:** Select a local model and click **Provide…**. You are asked to confirm before LUCID records the model in `approved_provide_models`, hot-imports its verified GGUF blob into the content catalog, updates `policy.toml` (`serve_models`), and advertises its signed record on the Kademlia DHT. Click **Stop Providing** at any time to revoke.
- **CLI (manual operator mode):**
  ```bash
  lucidd \
    --mode worker \
    --worker llama-cpp \
    --model-dir ~/Models \
    --llama-server-binary /absolute/path/to/llama-server
  ```

Even when sharing is enabled, only peers you explicitly invite (or list in `policy.toml`'s `authorized_submitters`) can run jobs on your machine. See [Operator policy and safety controls](operator-policy.md).

## Peer-to-peer model pull and resume

If a circle peer advertises a model with `--serve-model-content` enabled (off by default so serving inference never silently donates upload bandwidth), you can pull the verified weights directly over `/phase/blob/1.0.0`:

- **Desktop app:** Select a network model and click **Download Locally (P2P)**.
- **CLI / HTTP API:**
  ```bash
  curl -N http://127.0.0.1:11434/api/pull \
    -H "Content-Type: application/json" \
    -d '{"model": "qwen2.5-7b-q4"}'
  ```

Pin an exact content hash or publisher if an alias is ambiguous (ambiguous aliases fail closed with `409 Conflict` rather than picking arbitrarily):

```bash
curl -N http://127.0.0.1:11434/api/pull \
  -H "Content-Type: application/json" \
  -d '{"model": "qwen2.5-7b-q4", "cid": "<64-hex-sha256>", "publisher": "<peer-id>"}'
```

How pull safety and resume work:

1. **Signed alias resolution:** Resolves `SignedAliasRecord` from the DHT with monotonic sequence rollback and equivocation detection.
2. **Staging & exact-offset resume:** Downloads into `blobs/.staging/<aa>/<cid>.part` in 1–64 KiB chunks. If interrupted, the next pull requests `BlobStreamRequest { offset: existing_partial_len }` and resumes from the exact byte offset.
3. **Failover & commit verification:** Tries up to 8 providers (15-minute per-provider timeout, 1-hour total deadline). Before publishing to immutable read-only `blobs/<aa>/<cid>.bin`, `ArtifactStore::commit_staged_blob` checks the whole-file SHA-256 against the expected `ModelCid`. On mismatch, the corrupt partial is deleted and the next provider retries cleanly from offset `0`.
