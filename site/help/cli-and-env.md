# CLI, subcommands and environment variables

`lucidd` acts as both the long-running background daemon and a command-line client for inspecting hardware, managing circles, benchmarking throughput, and planning Tier-1 GPU splits.

## Subcommands

Control-API subcommands enforce an `http://` loopback-only `--api` URL (default `http://127.0.0.1:11435`) and automatically load the private `.control-token` sidecar.

### User and circle commands

| Command | Description |
|---|---|
| `lucidd probe` | Read-only hardware, GPU backend, Ollama/LM Studio, runtime, and local GGUF discovery. |
| `lucidd models [--api URL]` | Lists local, imported, and discovered network models from `/api/lucid/models`. |
| `lucidd peers [--api URL]` | Lists connected libp2p peers, active transport path (`direct` / `relayed` / `hole-punched`), and circle authorization. |
| `lucidd invite [--api URL] [--ttl-seconds 1800]` | Mints a single-use `lucid://join/...` invitation link (TTL `1..=3600` seconds). |
| `lucidd join <INVITATION> [--api URL]` | Submits a `lucid://join/...` invitation to enroll with the issuing node. |

### Benchmarking and Tier-1 cluster commands

| Command | Description |
|---|---|
| `lucidd bench --model <M> [--requests 12] [--concurrency 2] [--max-tokens 64] [--api URL] [--output PATH]` | Measures p50/p95 TTFT, duration, throughput, route distribution, and verified receipt count (records metrics only, never prompt payloads). |
| `lucidd cluster status [--api URL]` | Shows `cluster_enabled`, local node identity/memory, connected peers, and GGUF layer counts. |
| `lucidd cluster inspect <PATH>` | Read-only GGUF header parser reporting architecture, total layers, embedding dimension, and byte size. |
| `lucidd cluster plan --model <NAME> [--gguf PATH] [--layers 80] [--size-gib 42] --peer <NAME:GIB:BACKEND>...` | Generates and verifies a coordinator-signed Tier-1 `SignedShardPartitionPlan` across 2–16 peers. |

## Daemon flags

### Mode, identity, and configuration

| Flag | Default | Purpose |
|---|---|---|
| `--mode <auto\|worker\|consume-only\|infrastructure>` | `auto` | Operating mode (`relay` aliases `consume-only`). `auto` starts consume-only unless `--worker` is set or `node.toml` has `providing_enabled = true` with a valid pinned runtime and approved models. |
| `--no-local-worker` | `false` | Force consume-only mode. |
| `--identity-path <PATH>` | `<config_dir>/phase/identity.key` | Persistent 32-byte Ed25519 identity key (`0600`). |
| `--node-config <PATH>` | `<config_dir>/lucidd/node.toml` | Path to `node.toml`. |
| `--policy-config <PATH>` | `<config_dir>/lucidd/policy.toml` | Path to hot-reloadable `policy.toml`. |
| `--control-token-file <PATH>` | `<identity>.control-token` | Private Bearer token file for `/api/lucid/*`. |

### Workers, models, and verification

| Flag | Default | Purpose |
|---|---|---|
| `--worker <llama-cpp\|mlx\|echo>` | — | Explicit worker backend (required in `--mode worker`). |
| `--model-dir <DIR>` | — | Directory of `.gguf` files for `llama-cpp`. |
| `--artifact-dir <DIR>` | `<model-dir>/.phase-artifacts` or `<data_local_dir>/lucidd/artifacts` | Verified immutable content store for P2P pulls/imports. |
| `--serve-model-content` | `false` | Opt in to serving verified blobs over `/phase/blob/1.0.0`. |
| `--llama-server-binary <ABS_PATH>` | — | Absolute path to `llama-server` (no `PATH` lookup). |
| `--llama-n-gpu-layers <INT>` | `-1` (`all`) | GPU layer offload count passed to `llama-server`. |
| `--llama-ctx-size <USIZE>` | `8192` | Context window (`1..=u32::MAX`). |
| `--mlx-server-binary <ABS_PATH>` | — | Pinned `mlx_lm.server` binary (macOS Apple Silicon). |
| `--mlx-model-bundle <ABS_PATH>` | — | Immutable read-only canonical MLX model directory. |
| `--mlx-model-alias <ALIAS>` | — | Canonical alias advertised for the MLX bundle. |
| `--mlx-server-port <U16>` | `0` (ephemeral) | Loopback port for `mlx_lm.server`. |
| `--evidence-store <PATH>` | `<identity>.reputation-evidence-v1.log` | Local append-only execution evidence log. |
| `--alias-replay-state <PATH>` | `<identity>.alias-replay-state-v1.json` | Durable `0600` alias rollback/equivocation ledger. |
| `--pin-peer <PEER_ID>` / `--block-peer <PEER_ID>` | — | Repeatable local routing overrides. |
| `--allow-open-peers` | `false` | Allow routing to unenrolled/unpinned peers (testing only). |
| `--redundant-verification-permille <0..=1000>` | `0` | Deterministic spot-check sampling rate (per mille). |

### Networking, relay, and infrastructure

| Flag | Default | Purpose |
|---|---|---|
| `--libp2p-port <U16>` | `0` (ephemeral) | TCP/QUIC listen port (use `4001` or `443` for relays). |
| `--listen-address <MULTIADDR>` | — | Explicit listen multiaddr (repeatable, max 4; incompatible with `--outbound-only`). |
| `--outbound-only` | `false` | Disable direct listeners, mDNS, DCUtR, and AutoNAT client (outbound + relay circuits only). |
| `--bootstrap-peer <MULTIADDR>` | — | Explicit PeerID-pinned bootstrap peer (repeatable). |
| `--relay-peer <MULTIADDR>` | — | Explicit PeerID-pinned Circuit Relay v2 candidate (repeatable, max 8). |
| `--no-bootstrap-relays` | `false` | Do not treat bootstrap peers as relay candidates. |
| `--bootstrap-dns <DOMAIN>` | `bootstrap.phasebased.net` | DNS TXT bootstrap domain (repeatable, max 64 addrs/domain). |
| `--no-default-bootstrap` | `false` | Disable default foundation DNS bootstrap. |
| `--dns-fallback` | `false` | Opt in to Cloudflare `1.1.1.1` fallback if system resolver fails (fail-closed by default). |
| `--external-address <MULTIADDR>` | — | Public listen address (required in `--mode infrastructure`). |
| `--no-relay-server` / `--no-autonat-server` | `false` | Disable relay or AutoNAT server in `--mode infrastructure`. |

### Tier-1 Trusted-Circle cluster & RPC flags

| Flag | Purpose |
|---|---|
| `--llama-rpc-peers <PEER_ID,...>` | Comma-separated enrolled `PeerId`s tunneled over `/phase/shard-rpc/1.0.0` (combined RPC limit: 16). |
| `--llama-rpc-servers <ADDR,...>` | Comma-separated loopback-only (`127.0.0.1:PORT` / `[::1]:PORT`) `ggml-rpc-server` endpoints. |
| `--llama-tensor-split <W0,W1,...>` | Layer split weights (1–16 non-negative weights, sum > 0; **RPC-first order**, then local GPUs). |
| `--shard-rpc-bridge-port <U16>` | Manual loopback port for operator-run `ggml-rpc-server` (omitting arms auto-managed `ggml-rpc-server`). |
| `--shard-stage-bridge-port <U16>` | Loopback port for `/phase/shard-stage/1.0.0` pipeline stage runner. |

## Environment variables

### `lucidd` runtime

- `LUCIDD_HOST` — HTTP bind address (default `127.0.0.1`; non-loopback values are rejected).
- `LUCIDD_PORT` — HTTP bind port (default `11434`).
- `RUST_LOG` — tracing filter (default `info,lucidd=debug`).

### LUCID Desktop (`apps/lucid-desktop`)

- `LUCID_APP_STATE_DIR` — override mode-`0700` state directory (default `<data_local_dir>/LUCID`).
- `LUCID_DAEMON_PATH` — override `lucidd` sidecar executable path.
- `LUCID_RELAY_PEERS` — JSON array of up to 4 PeerID-pinned relay multiaddrs (overrides default DNS bootstrap).

### Relay systemd & maintainer release signing

- `LUCIDD_EXTERNAL_ADDRESS`, `LUCIDD_LIBP2P_PORT` — used by `lucidd-relay.service`.
- `LUCID_APPLE_TEAM_ID` (or `LUCID_APPLE_SIGNING_IDENTITY`), `LUCID_APPLE_NOTARY_PROFILE` — local macOS Keychain signing/notarization in `scripts/sign-macos-release.sh`.
- `LUCID_AZURE_SIGN_ENDPOINT`, `LUCID_AZURE_SIGN_ACCOUNT`, `LUCID_AZURE_SIGN_PROFILE`, plus `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` (OIDC) or `AZURE_CLIENT_SECRET` (Service Principal) — used by `scripts/sign-windows-binary.ps1` and `.github/workflows/release.yml`.
