# lucidd 0.1.0 — Linux ARM64

LUCID inference daemon. Ollama-compatible API on `:11434` backed by Phase's libp2p substrate. AGPL-3.0-or-later.

## Install

Drop the `lucidd` binary anywhere on `$PATH` (e.g. `/usr/local/bin/`).

```bash
sudo install -m 0755 lucidd /usr/local/bin/lucidd
```

## Run (consume-only — no local model)

The simplest test on a GPU-less machine: route every request through Phase's DHT to a peer that has the model loaded.

```bash
lucidd --no-local-worker
```

Then `curl http://localhost:11434/api/tags` to confirm the daemon is up. Requests to `/api/chat` will look up the model on the DHT and relay.

## Run with a local model (requires llama.cpp)

There's no `llama-cpp` package in Debian/Ubuntu repos. Build from source (universal, ~5 min on a fast box):

```bash
sudo apt update
sudo apt install -y build-essential cmake git
git clone https://github.com/ggml-org/llama.cpp ~/llama.cpp
cd ~/llama.cpp
cmake -B build
cmake --build build --config Release -j
sudo install -m 0755 build/bin/llama-server /usr/local/bin/llama-server
```

Or grab a prebuilt binary from the [llama.cpp releases page](https://github.com/ggml-org/llama.cpp/releases) — they publish for Linux ARM64.

Then put GGUF files in a directory and:

```bash
lucidd --worker llama-cpp --model-dir /opt/lucidd/models
```

> Note: in a Parallels VM there's no GPU pass-through (no CUDA / Metal), so inference is CPU-only. Fine for protocol testing, slow for real workloads. Run the heavyweight worker on the host Mac instead and let the VM be `--no-local-worker`.

## Environment variables

- `LUCIDD_PORT` — HTTP port (default 11434)
- `LUCIDD_HOST` — bind address (default 127.0.0.1)
- `RUST_LOG` — log filter (default `info,lucidd=debug`)

## CLI help

`lucidd --help` lists every flag, including the policy config path, llama-server binary override, context size, and GPU layer count.

## Verify the build

```
$ file lucidd
ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV), dynamically linked,
interpreter /lib/ld-linux-aarch64.so.1, for GNU/Linux 3.7.0, stripped
```

Built reproducibly from the Phase monorepo via:

```bash
docker run --rm -v $(pwd):/workspace -w /workspace --platform linux/arm64 \
  rust:1.95-bookworm cargo build --release \
  --target-dir /workspace/target-linux-arm64 -p lucidd
```
