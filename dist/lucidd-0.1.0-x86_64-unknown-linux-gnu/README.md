# lucidd 0.1.0 — Linux x86_64

LUCID inference daemon. Ollama-compatible API on `:11434` backed by Phase's libp2p substrate. AGPL-3.0-or-later.

## Install

Drop the `lucidd` binary anywhere on `$PATH`:

```bash
sudo install -m 0755 lucidd /usr/local/bin/lucidd
```

## Run modes

**As a relay / consume-only node** (no GPU, no model — routes requests to peers):

```bash
lucidd --mode relay
```

**With a local model via llama.cpp**:

```bash
sudo apt install -y build-essential cmake git
git clone https://github.com/ggml-org/llama.cpp ~/llama.cpp
cd ~/llama.cpp && cmake -B build && cmake --build build --config Release -j
sudo install -m 0755 build/bin/llama-server /usr/local/bin/llama-server

lucidd --worker llama-cpp --model-dir /opt/lucidd/models
```

## Flags worth knowing

- `--mode worker|relay` — relay = no local worker
- `--libp2p-port <N>` — pin a libp2p port (default 0 = random). Use a known value (e.g. 4001) when you want to forward the port on your router and let WAN peers dial you with a stable multiaddr.
- `--identity-path <path>` — persistent libp2p identity file. Default `~/.config/phase/identity.key` (platform-aware). Same path = same peer-id across restarts.
- `--bootstrap-peer <multiaddr>` — repeatable. Format `/dns4/host/tcp/<port>/p2p/<peer-id>` or `/ip4/.../tcp/.../p2p/<peer-id>`. Required for WAN discovery (mDNS only crosses LANs).

`LUCIDD_PORT` and `LUCIDD_HOST` env vars override the HTTP API bind. `--help` lists everything.

## Verify the build

```
$ file lucidd
ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked,
interpreter /lib64/ld-linux-x86-64.so.2, for GNU/Linux 3.2.0, stripped
```

Built via:

```bash
docker run --rm -v $(pwd):/workspace -w /workspace --platform linux/amd64 \
  rust:1.95-bookworm cargo build --release \
  --target-dir /workspace/target-linux-amd64 -p lucidd
```
