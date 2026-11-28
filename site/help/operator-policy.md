# Operator policy and safety controls

Two configuration files govern what a LUCID node does locally and what it serves to peers: **`policy.toml`** (live donation-protection and authorization rules) and **`node.toml`** (persistent opt-in state, model folders, and pinned runtime).

## Default file locations

| File | Linux | macOS | Permissions |
|---|---|---|---|
| `policy.toml` | `~/.config/lucidd/policy.toml` | `~/Library/Application Support/lucidd/policy.toml` | `0600` (on API write) |
| `node.toml` | `~/.config/lucidd/node.toml` | `~/Library/Application Support/lucidd/node.toml` | `0600` in `0700` dir |
| `identity.key` | `~/.config/phase/identity.key` | `~/Library/Application Support/phase/identity.key` | `0600` in `0700` dir |

LUCID Desktop isolates its sidecar state under `<data_local_dir>/LUCID` (or `LUCID_APP_STATE_DIR`) at mode `0700`.

## `policy.toml` reference

On first startup, `lucidd` seeds a commented `policy.toml` if none exists. Changes hot-reload automatically via a directory watcher and also reload on Unix `SIGHUP`.

```toml
# Pause serving remote peers when running on battery power.
auto_pause_on_battery = true

# Pause remote peers when the hottest detected sensor exceeds this threshold (°C).
# Omit or comment out to disable thermal gating.
auto_pause_on_thermal_threshold_c = 75.0

# Informational VRAM headroom target (GiB).
vram_reserve_gb = 4.0

# Glob allowlist of model IDs permitted for remote peers.
serve_models = ["*"]

# Optional local-clock serving window (supports midnight wrap, e.g. 22 -> 6).
# start_hour_local == end_hour_local means never serve remotely.
# [time_of_day_window]
# start_hour_local = 22
# end_hour_local = 6

# Master kill switch: pauses BOTH remote peers AND your own self-requests.
manual_pause = false

# Maximum simultaneous remote peer jobs.
max_concurrent_remote_jobs = 4

# Default-deny allowlist of lowercase 64-hex Ed25519 public keys.
# Circle invitations grant temporary access alongside this static list.
authorized_submitters = []

# Insecure development escape hatch. NEVER enable on untrusted networks.
# Ignored completely by /phase/shard-rpc/1.0.0 (which requires explicit enrollment).
allow_unauthenticated_jobs = false

# Server-side clamp on requested max tokens (1..=max_tokens_ceiling).
max_tokens_ceiling = 8192
```

### Decision precedence

For every inbound peer job, `PolicyEngine` checks gates in strict order:

1. `manual_pause == true` → refuse (`Manual`)
2. Running on battery and `auto_pause_on_battery == true` → refuse (`OnBattery`)
3. Hottest sensor `> auto_pause_on_thermal_threshold_c` → refuse (`ThermalLimit`)
4. Current local hour outside `time_of_day_window` → refuse (`OutsideTimeWindow`)
5. Active remote jobs `>= max_concurrent_remote_jobs` → refuse (`ConcurrencyLimit`)
6. Model ID fails all globs in `serve_models` → refuse (`ModelNotInAllowlist`)

**Your own local prompts** (`should_serve_self`) bypass battery, thermal, time-of-day, concurrency, and allowlist gates—only `manual_pause` stops your own local requests.

Battery and thermal sensors refresh every 30 seconds in a background poller on macOS and Linux (desktops without batteries or Windows hosts without sensor readings report `None` and do not false-pause).

## `node.toml` reference

`node.toml` (schema `1`) records explicit operator opt-ins managed via Desktop or `/api/lucid/*` control calls:

| Field | Default | Purpose |
|---|---|---|
| `onboarding_completed` | `false` | Whether first-run onboarding has been completed. |
| `providing_enabled` | `false` | Master opt-in to serve inference on this machine. |
| `approved_provide_models` | `[]` | Explicitly approved model names (max 256). |
| `approved_model_sources` | `[]` | `{name, path, source}` records for hot-imported Ollama/LM Studio/local GGUFs. |
| `approved_local_models` | `[]` | Models approved for local-only use (max 256). |
| `model_dirs` | `[]` | User-added model directories (max 16; validated against symlinks and sensitive dirs). |
| `llama_server_binary` | `null` | Absolute path to pinned `llama-server`. |
| `llama_server_sha256` | `null` | 64-hex SHA-256 verified on every daemon startup. |
| `serve_model_content` | `false` | Opt-in to upload verified model blobs over `/phase/blob/1.0.0`. |
| `cluster_enabled` | `false` | Live gate for Tier-1 Trusted-Circle `/phase/shard-rpc/1.0.0` and `/phase/shard-stage/1.0.0`. |
| `bootstrap_peers` | `[]` | Optional custom bootstrap multiaddrs (max 8). |

## Updating settings live

- **Desktop app:** Open **Settings & Safety** to toggle Manual Pause, Auto-pause on battery, Open WebUI URL, or remove model folders; open **Trusted-Circle Cluster** to toggle `cluster_enabled`.
- **HTTP Control API:** `POST /api/lucid/settings` updates `manual_pause`, `auto_pause_on_battery`, `thermal_limit_c` (clamped `40..=105`), `serve_model_content`, and `cluster_enabled` atomically at runtime without restarting `lucidd`. See [HTTP API reference](http-api.md#post-apilucidsettings).
