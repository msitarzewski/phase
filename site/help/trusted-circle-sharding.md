# Pool GPUs across machines (Tier-1 Trusted-Circle)

When a model is too large for one machine's memory, LUCID lets you slice its layers across machines you personally trust over Noise-encrypted libp2p tunnels—with no manual SSH port forwarding required.

> [!WARNING]
> **SSH-equivalent trust boundary:** Enabling `cluster_enabled = true` lets explicitly enrolled circle peers connect to a loopback `ggml-rpc-server` on your GPU over `/phase/shard-rpc/1.0.0`. Only enable Tier-1 GPU pooling among machines you own or trust at the OS level. Tier-2 `UntrustedSpotChecked` sharding is **not implemented** and fails closed (`403 Forbidden`).

## How Tier-1 pooling works

1. **Helper node (contributes GPU memory):** When `cluster_enabled = true` and a pinned `llama.cpp b12651` `ggml-rpc-server` sits next to `llama-server` (and `--shard-rpc-bridge-port` is not manually overridden), `lucidd` automatically starts and supervises a loopback-only `ggml-rpc-server` on `127.0.0.1:<ephemeral>` with `-c` (disk cache) and a cleared environment.
2. **Coordinator node (runs the model):** Started with `--llama-rpc-peers <HELPER_PEER_ID>` and `--llama-tensor-split <W_RPC,W_LOCAL>`. `lucidd` binds an ephemeral loopback proxy per peer, validates the `ggml-rpc` `HELLO` packet (`cmd == 0x0e`), and tunnels bytes over Noise-encrypted `/phase/shard-rpc/1.0.0`.
3. **Honest labels & privacy:** Every response from an RPC-offloaded model reports `X-Lucid-Routed-Via: trusted-circle-sharded` and `X-Lucid-Trust-Label: trusted-circle-sharded`. Requests carrying `X-Lucid-Local-Only: true` are refused (`503`) because activations cross the network to the helper GPU.

## Zero-CLI helper setup

If you installed `llama.cpp b12651` via LUCID Desktop (or placed the official `b12651` archive binaries in one folder), the helper node needs **no extra CLI flags**:

1. Enroll the coordinator's identity with the helper via a normal `lucid://join/...` invitation (or list the coordinator's 64-hex Ed25519 public key in `policy.toml`'s `authorized_submitters`).
2. Toggle **Participate in Tier 1** (`cluster_enabled = true`) in Desktop's **Trusted-Circle Cluster** modal or via `POST /api/lucid/settings`.

What `ManagedShardRpcProvider` enforces automatically on the helper:

- **SHA-256 pinning:** Verifies `ggml-rpc-server` against the 5 pinned `b12651` platform digests before arming and before every spawn, plus ownership and non-world/group-writable checks on the binary, parent directory, and `libggml*` libraries.
- **No orphan processes:** Wraps `ggml-rpc-server` in a stdin-EOF lifeline guard so even `SIGKILL` of `lucidd` immediately kills the RPC server.
- **Strict enrollment-only admission:** Drops non-enrolled peers *before reading a single byte* (`allow_unauthenticated_jobs` is **never** honored for `/phase/shard-rpc/1.0.0`).
- **Live revocation:** Re-checks `cluster_enabled`, `!manual_pause`, and enrollment every 250 ms. Turning `cluster_enabled` off immediately tears down live tunnels and kills `ggml-rpc-server`.

## Coordinator setup

Start `lucidd` on the coordinator with `--llama-rpc-peers` and `--llama-tensor-split`:

```bash
lucidd \
  --mode worker \
  --worker llama-cpp \
  --model-dir ~/Models \
  --llama-server-binary /absolute/path/to/llama-server \
  --llama-rpc-peers 12D3KooWHelperPeerIdHere \
  --llama-tensor-split 10,90
```

> [!IMPORTANT]
> **`--llama-tensor-split` is RPC-first:** `llama-server` assigns weights to remote `--rpc` devices *first* (in the order listed), followed by local GPUs. In the example above, `10,90` puts 10% of layers on the remote helper peer and 90% on the coordinator's local GPU.

### Refusal of silent local fallback

Upstream `llama-server b12651` will silently drop an unreachable `--rpc` device and load all layers locally instead. `lucidd` tracks attached tunnel generations per configured peer and **refuses the model load or respawn** if any configured `--llama-rpc-peers` device did not actually attach—guaranteeing that a request labeled `trusted-circle-sharded` never silently ran locally or silently dropped a peer.

### Manual loopback bridge (advanced)

If you prefer to manage `ggml-rpc-server` yourself on the helper, pass `--shard-rpc-bridge-port <PORT>` on the helper (disabling auto-management) or `--llama-rpc-servers 127.0.0.1:<PORT>` on the coordinator for local loopback bridges.

## Inspecting GGUF layers and planning splits

Use the `lucidd cluster` subcommands to inspect any local GGUF file's layer count and generate a coordinator-signed `SignedShardPartitionPlan`:

```bash
# Inspect GGUF architecture, layer count, and size (read-only)
lucidd cluster inspect ~/Models/gemma-4-12B-it-QAT-Q4_0.gguf

# Check live daemon cluster status
lucidd cluster status

# Generate and verify a signed Tier-1 partition plan across >=2 peers
lucidd cluster plan \
  --model gemma-4-12b \
  --gguf ~/Models/gemma-4-12B-it-QAT-Q4_0.gguf \
  --peer mac-main:96:metal \
  --peer mac-helper:32:metal
```

For multi-stage pipeline execution with chained Ed25519 stage receipts (`/phase/shard-stage/1.0.0`), see `POST /api/lucid/cluster/execute` in the [HTTP API reference](http-api.md#post-apilucidclusterexecute) and [Phase network protocols](protocols.md#phaseshard-stage100).
