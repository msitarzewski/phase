// SPDX-License-Identifier: AGPL-3.0-or-later

//! LUCID M7 — operator policy surface and auto-pause.
//!
//! `PolicyEngine` is the declarative gate every inference request the daemon
//! is asked to serve on behalf of a peer must pass through. It does not
//! "deprioritize" — when conditions are bad (laptop on battery, GPU thermal
//! limit hit, outside the operator's chosen serving window, etc.) it
//! *refuses* to serve, with a structured `PauseReason` the router (LUCID M5)
//! can surface to the requesting peer.
//!
//! The operator is sovereign. The defaults are conservative:
//!
//! - Pause when on battery.
//! - Pause when GPU/CPU temperature crosses 75 °C.
//! - Serve all models (`["*"]`) — operators narrow this themselves.
//! - No time-of-day window — always allowed by clock.
//! - Reserve 4 GiB VRAM headroom (informational; enforcement is M5's job).
//! - 4 concurrent remote jobs max.
//!
//! All of these live in a TOML file at `~/.config/lucidd/policy.toml` that
//! the engine watches for changes (notify-rs file events + SIGHUP on Unix)
//! and atomically swaps in on edit. The intent is that operators can tune
//! their node without restarting `lucidd`.
//!
//! See `memory-bank/MISSION.md` ("For everyone") and `releases/lucid/README.md`
//! (M7) for the wider framing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// We use `std::sync::RwLock` (not `tokio::sync::RwLock`) for the config and
// state caches. The reasons:
//
// 1. `should_serve` is on the hot inference path and is `fn` (not `async`).
//    The router (M5) calls it from sync handler code; making it `async`
//    would force a `.await` at every dispatch site.
// 2. We never hold either lock across an `.await`. Reads are clone-and-drop;
//    writes set fields and drop. Lock duration is microseconds.
// 3. `tokio::sync::RwLock::blocking_read` panics inside a runtime worker
//    thread (which is exactly where `should_serve` will be called from
//    axum handlers), so it's the wrong primitive here.
//
// If a future change adds an `.await` while holding one of these locks,
// switch to `tokio::sync::RwLock` and pay the async tax everywhere.

/// Default polling interval for the background system-state watcher.
///
/// Battery / temperature don't move fast enough on a typical desktop or
/// laptop to need anything tighter than this. Keeping the interval at 30s
/// also keeps the daemon's idle CPU footprint negligible.
const STATE_POLL_INTERVAL: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Public configuration surface
// ---------------------------------------------------------------------------

/// Operator-controlled policy. Loaded from TOML, edited in-place, reloaded
/// on file-change events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Pause serving remote requests while the host is on battery power.
    /// On desktop / unknown power sources this has no effect.
    pub auto_pause_on_battery: bool,

    /// Pause serving remote requests when *any* monitored thermal sensor
    /// crosses this threshold in Celsius. `None` disables the check.
    pub auto_pause_on_thermal_threshold_c: Option<f32>,

    /// Informational only at the policy layer — the router (M5) reads this
    /// to decide whether a model "fits" given current VRAM headroom.
    pub vram_reserve_gb: f32,

    /// Glob patterns matched against `model_id` on each request. `["*"]`
    /// means serve anything. Operators narrow this to declare which models
    /// they are willing to serve to peers.
    pub serve_models: Vec<String>,

    /// Optional clock-based serving window. Both bounds are local-time
    /// hours in `0..=23`. If `start > end` the window wraps midnight
    /// (e.g. `start=23, end=7` means "serve overnight"). `None` means
    /// always-allowed.
    pub time_of_day_window: Option<TimeWindow>,

    /// Manual operator-controlled pause. The CLI / future admin API flips
    /// this. Wins over every other check.
    pub manual_pause: bool,

    /// Hard ceiling on concurrent remote inference jobs we'll accept. Past
    /// this count, fresh requests get `PauseReason::ConcurrencyLimit` and
    /// the router (M5) is expected to surface that as a peer-visible
    /// refusal.
    pub max_concurrent_remote_jobs: u32,

    /// SEC-01: operator allowlist of authorized client pubkeys (lowercase
    /// hex of the 32-byte Ed25519 verifying key). A signed manifest only
    /// passes the authorization gate if its `signer_pubkey` appears here
    /// (case-insensitive) — *after* `verify()` proves the signature.
    ///
    /// Default is **empty = deny everyone**. `verify()` only proves "some
    /// keyholder signed this", not "an authorized party signed this"; the
    /// allowlist is what pins the key to an authorized identity.
    pub authorized_submitters: Vec<String>,

    /// SEC-01 escape hatch: when `true`, skip the authorization gate
    /// entirely and accept any manifest that passes `verify()`. This
    /// restores the pre-SEC-01 open behavior and is **insecure** — it lets
    /// any anonymous peer use this node's GPU. Intended only for local
    /// development / single-machine testing. Default `false`.
    pub allow_unauthenticated_jobs: bool,

    /// SEC-01: server-side hard ceiling on a manifest's `max_tokens`. Any
    /// manifest-supplied value above this is clamped down regardless of
    /// what the (untrusted) client asked for. Protects against a peer
    /// requesting an enormous generation to exhaust GPU time.
    pub max_tokens_ceiling: u32,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auto_pause_on_battery: true,
            auto_pause_on_thermal_threshold_c: Some(75.0),
            vram_reserve_gb: 4.0,
            serve_models: vec!["*".to_string()],
            time_of_day_window: None,
            manual_pause: false,
            max_concurrent_remote_jobs: 4,
            // SEC-01: default-deny. Operator must explicitly list keys, or
            // flip `allow_unauthenticated_jobs` for local dev.
            authorized_submitters: Vec::new(),
            allow_unauthenticated_jobs: false,
            // 8192 tokens is a generous default ceiling; operators can raise
            // it. Clamps a hostile manifest's `max_tokens` server-side.
            max_tokens_ceiling: 8192,
        }
    }
}

/// Hour-of-day window in the host's local timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    /// First local hour at which serving is allowed (0–23).
    pub start_hour_local: u8,
    /// First local hour at which serving is *not* allowed (0–23). If
    /// `start_hour_local > end_hour_local`, the window wraps midnight —
    /// `(23, 7)` means "23:00 through 06:59".
    pub end_hour_local: u8,
}

impl PolicyConfig {
    /// SEC-01 authorization gate. Returns `true` if `pubkey_hex` (the
    /// `signer_pubkey` from a *verified* manifest) is permitted to submit
    /// work to this node. Comparison is case-insensitive on the hex.
    ///
    /// Order of decisions:
    /// 1. `allow_unauthenticated_jobs == true` → accept anything (insecure
    ///    local-dev mode).
    /// 2. Otherwise the key must appear in `authorized_submitters`.
    ///
    /// NOTE (SEC-06 / PeerID-bind hook): v0.2 will additionally accept a key
    /// whose bytes match the delivering libp2p `PeerId`. That requires the
    /// relay handler to receive the peer identity (SEC-06). Until then, the
    /// allowlist is the sole authorization source.
    pub fn is_authorized_submitter(&self, pubkey_hex: &str) -> bool {
        if self.allow_unauthenticated_jobs {
            return true;
        }
        self.authorized_submitters
            .iter()
            .any(|k| k.eq_ignore_ascii_case(pubkey_hex))
    }

    /// SEC-01: clamp a manifest-supplied `max_tokens` to the operator
    /// ceiling. `None` (client didn't ask) stays `None`; any value above
    /// the ceiling is reduced to it.
    pub fn clamp_max_tokens(&self, requested: Option<u32>) -> Option<u32> {
        requested.map(|n| n.min(self.max_tokens_ceiling))
    }
}

impl TimeWindow {
    /// Is `hour_local` inside this window? Handles midnight wrap.
    pub fn contains_hour(&self, hour_local: u8) -> bool {
        if self.start_hour_local == self.end_hour_local {
            // Degenerate: empty window. Treat as "never" — operator
            // should use `time_of_day_window = None` for always-on.
            return false;
        }
        if self.start_hour_local < self.end_hour_local {
            hour_local >= self.start_hour_local && hour_local < self.end_hour_local
        } else {
            // Wraps midnight: e.g. start=23, end=7 → [23,24) ∪ [0,7).
            hour_local >= self.start_hour_local || hour_local < self.end_hour_local
        }
    }
}

/// Outcome of a policy check.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyDecision {
    /// Serve the request.
    Allow,
    /// Refuse the request, with a machine-readable reason.
    Pause { reason: PauseReason },
}

/// Why a request was paused. Order matters: the engine evaluates these in
/// declaration order and returns the *first* match, so `Manual` and the
/// hard system-level pauses fire before the softer per-request reasons.
#[derive(Debug, Clone, PartialEq)]
pub enum PauseReason {
    /// Operator-flipped manual pause. Trumps everything.
    Manual,
    /// Host is on battery power and `auto_pause_on_battery` is on.
    OnBattery,
    /// Any monitored thermal sensor crossed the configured threshold.
    ThermalLimit { current_c: f32, threshold_c: f32 },
    /// Local clock is outside the configured serving window.
    OutsideTimeWindow,
    /// We're already serving `max_concurrent_remote_jobs`.
    ConcurrencyLimit,
    /// `model_id` doesn't match any glob in `serve_models`.
    ModelNotInAllowlist { model_id: String },
    /// Upstream-signalled stop — e.g. the daemon is shutting down.
    SystemPaused,
}

/// Engine state snapshot. Returned by `state()` for `/api/status`-style
/// surfaces (the actual HTTP wiring is M5).
#[derive(Debug, Clone, Default)]
pub struct PolicyState {
    /// `Some(true)` = on battery, `Some(false)` = on AC, `None` = unknown
    /// (no battery, Windows, sensor read failed).
    pub on_battery: Option<bool>,
    /// Hottest monitored component in °C. `None` if no usable sensor.
    pub temperature_c: Option<f32>,
    /// Number of currently-running remote inference jobs. Updated by the
    /// router (M5); the engine reads it on every decision.
    pub current_concurrent: u32,
    /// Last decision the engine returned, with the wall-clock instant it
    /// was taken. Useful for the `/api/status` surface and for tests.
    pub last_decision: Option<(PolicyDecision, Instant)>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The policy engine. Cheap to clone (everything is `Arc`); the engine owns
/// a background watcher task that polls system state and reloads config on
/// file change. Drop the engine to stop the watcher.
pub struct PolicyEngine {
    config: Arc<RwLock<PolicyConfig>>,
    state: Arc<RwLock<PolicyState>>,
    config_path: Option<PathBuf>,
    _watcher_handle: JoinHandle<()>,
}

impl PolicyEngine {
    /// Load config from `path` (or `~/.config/lucidd/policy.toml` if `None`),
    /// writing a fully-commented default file if it doesn't yet exist, and
    /// spawn the background state/config watcher.
    pub async fn load_or_default(path: Option<PathBuf>) -> Result<Self> {
        let resolved = match path {
            Some(p) => Some(p),
            None => default_config_path(),
        };

        // Either parse what's there, or seed the default. We deliberately
        // tolerate a missing config dir — first run on a fresh machine
        // should "just work" with sensible defaults.
        let config = if let Some(p) = &resolved {
            if p.exists() {
                read_config(p).with_context(|| format!("reading policy config {}", p.display()))?
            } else {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let default = PolicyConfig::default();
                std::fs::write(p, DEFAULT_CONFIG_TOML)
                    .with_context(|| format!("writing default policy config {}", p.display()))?;
                default
            }
        } else {
            PolicyConfig::default()
        };

        let config = Arc::new(RwLock::new(config));
        let state = Arc::new(RwLock::new(PolicyState::default()));

        let watcher_handle = spawn_watcher(config.clone(), state.clone(), resolved.clone());

        Ok(Self {
            config,
            state,
            config_path: resolved,
            _watcher_handle: watcher_handle,
        })
    }

    /// Snapshot of the current config. Cheap (`Clone`).
    pub fn config(&self) -> PolicyConfig {
        self.config.read().expect("policy config lock poisoned").clone()
    }

    /// SEC-01: is `pubkey_hex` (from a *verified* manifest) authorized to
    /// submit work? Reads the live config under the lock. Cheap; never held
    /// across an `.await`.
    pub fn is_authorized_submitter(&self, pubkey_hex: &str) -> bool {
        self.config
            .read()
            .expect("policy config lock poisoned")
            .is_authorized_submitter(pubkey_hex)
    }

    /// SEC-01: clamp a manifest-supplied `max_tokens` to the live operator
    /// ceiling.
    pub fn clamp_max_tokens(&self, requested: Option<u32>) -> Option<u32> {
        self.config
            .read()
            .expect("policy config lock poisoned")
            .clamp_max_tokens(requested)
    }

    /// Snapshot of the current state. Cheap (`Clone`).
    pub fn state(&self) -> PolicyState {
        self.state.read().expect("policy state lock poisoned").clone()
    }

    /// The actual decision function. `current_concurrency` is supplied by
    /// the router (M5) on each call — the engine doesn't own that counter
    /// because the router's bookkeeping is per-request and lives closer to
    /// the dispatch path.
    pub fn should_serve(&self, model_id: &str, current_concurrency: u32) -> PolicyDecision {
        let config = self.config.read().expect("policy config lock poisoned").clone();
        let state = self.state.read().expect("policy state lock poisoned").clone();
        let decision = decide(&config, &state, model_id, current_concurrency);
        // Best-effort record of the last decision. We try a non-blocking
        // write so a contended watcher tick can't stall a request; if it
        // fails we accept a stale `last_decision` snapshot.
        if let Ok(mut s) = self.state.try_write() {
            s.last_decision = Some((decision.clone(), Instant::now()));
            s.current_concurrent = current_concurrency;
        }
        decision
    }

    /// Flip the operator-controlled manual pause. The change is durable for
    /// the lifetime of the process; persisting to disk is a separate
    /// concern (M7 future work — the operator can also just edit the
    /// config file).
    pub async fn set_manual_pause(&self, paused: bool) {
        let mut c = self.config.write().expect("policy config lock poisoned");
        c.manual_pause = paused;
    }

    /// Force a re-read of the config file. Used by the SIGHUP handler and
    /// available to admin code paths that prefer not to wait for the
    /// filesystem watcher. `async` because the I/O happens on a blocking
    /// pool via `spawn_blocking` — that keeps any future watchers (which
    /// may also call `reload`) from stalling the runtime.
    pub async fn reload(&self) -> Result<()> {
        let Some(path) = self.config_path.clone() else {
            return Ok(()); // No file backing → nothing to reload.
        };
        let new = tokio::task::spawn_blocking(move || read_config(&path))
            .await
            .context("reload join")??;
        *self.config.write().expect("policy config lock poisoned") = new;
        if let Some(p) = &self.config_path {
            tracing::info!(path = %p.display(), "policy config reloaded");
        }
        Ok(())
    }

    // --- test helpers -------------------------------------------------------

    /// Test-only: construct an engine with a fixed config + state and no
    /// background watcher. Lets us drive each `PauseReason` branch without
    /// needing real battery / thermal hardware.
    #[cfg(test)]
    pub fn new_for_tests(config: PolicyConfig, state: PolicyState) -> Self {
        let handle = tokio::spawn(async {});
        Self {
            config: Arc::new(RwLock::new(config)),
            state: Arc::new(RwLock::new(state)),
            config_path: None,
            _watcher_handle: handle,
        }
    }

    /// Test-only: overwrite the state snapshot in-place.
    #[cfg(test)]
    pub fn set_test_state(&self, state: PolicyState) {
        *self.state.write().expect("policy state lock poisoned") = state;
    }
}

// ---------------------------------------------------------------------------
// Pure decision function (no I/O — easy to test)
// ---------------------------------------------------------------------------

fn decide(
    config: &PolicyConfig,
    state: &PolicyState,
    model_id: &str,
    current_concurrency: u32,
) -> PolicyDecision {
    // 1. Manual override wins.
    if config.manual_pause {
        return PolicyDecision::Pause {
            reason: PauseReason::Manual,
        };
    }

    // 2. On battery — only if we *know* we're on battery. `None` (desktop /
    //    unknown) is treated as "fine to serve".
    if config.auto_pause_on_battery && state.on_battery == Some(true) {
        return PolicyDecision::Pause {
            reason: PauseReason::OnBattery,
        };
    }

    // 3. Thermal threshold. If the threshold is set and we have a reading
    //    above it, pause. A missing sensor is "fine to serve" — operators
    //    on hardware without sensors can pause manually.
    if let (Some(threshold), Some(current)) =
        (config.auto_pause_on_thermal_threshold_c, state.temperature_c)
    {
        if current > threshold {
            return PolicyDecision::Pause {
                reason: PauseReason::ThermalLimit {
                    current_c: current,
                    threshold_c: threshold,
                },
            };
        }
    }

    // 4. Time-of-day window.
    if let Some(window) = config.time_of_day_window {
        let hour = Local::now().hour() as u8;
        if !window.contains_hour(hour) {
            return PolicyDecision::Pause {
                reason: PauseReason::OutsideTimeWindow,
            };
        }
    }

    // 5. Concurrency ceiling.
    if current_concurrency >= config.max_concurrent_remote_jobs {
        return PolicyDecision::Pause {
            reason: PauseReason::ConcurrencyLimit,
        };
    }

    // 6. Model allowlist (glob). Operators write things like
    //    `["qwen3-*", "deepseek-v3-*"]`; we match the request's `model_id`
    //    against each pattern and accept on first hit.
    if !matches_any_glob(&config.serve_models, model_id) {
        return PolicyDecision::Pause {
            reason: PauseReason::ModelNotInAllowlist {
                model_id: model_id.to_string(),
            },
        };
    }

    PolicyDecision::Allow
}

fn matches_any_glob(patterns: &[String], model_id: &str) -> bool {
    patterns.iter().any(|p| {
        glob::Pattern::new(p)
            .map(|pat| pat.matches(model_id))
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Config I/O
// ---------------------------------------------------------------------------

fn read_config(path: &Path) -> Result<PolicyConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let cfg: PolicyConfig = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

/// Default config file path. Honors `XDG_CONFIG_HOME` via `dirs::config_dir()`
/// on Linux, and Application Support on macOS. Returns `None` only when the
/// OS doesn't expose a config dir (unusual).
fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lucidd").join("policy.toml"))
}

/// Fully-commented default policy.toml — what we drop on first run so
/// operators see exactly which knobs are available without having to read
/// source.
pub const DEFAULT_CONFIG_TOML: &str = r#"# LUCID operator policy
# ----------------------
# This file controls when *this* node will serve inference requests from
# peers on the Phase DHT. It does NOT affect requests made directly to your
# own local Ollama API on :11434 — those always run locally.
#
# Edits are picked up automatically (file watcher + SIGHUP). No daemon
# restart required.

# Pause serving remote requests while the laptop is on battery power.
# Desktops / unknown power sources are unaffected.
auto_pause_on_battery = true

# Pause serving remote requests when any monitored thermal sensor crosses
# this temperature in Celsius. Comment out to disable thermal-based pause.
auto_pause_on_thermal_threshold_c = 75.0

# How many GiB of VRAM to keep free for local work. Informational at the
# policy layer — the router (LUCID M5) uses this to decide whether a model
# "fits" before accepting a remote job.
vram_reserve_gb = 4.0

# Glob patterns of model_ids you are willing to serve to peers. "*" serves
# every model you have loaded. Narrow this if you only want to share
# specific models — for example:
#   serve_models = ["qwen3-*", "deepseek-v3-*"]
serve_models = ["*"]

# Optional clock-based serving window in local time. Comment out to allow
# at any hour. If start > end, the window wraps midnight (so 23 → 7 means
# "serve overnight, sleep during the day").
# [time_of_day_window]
# start_hour_local = 23
# end_hour_local = 7

# Operator-controlled manual pause. Set to true to stop serving remote
# requests without changing any other setting. Wins over every other check.
manual_pause = false

# Hard ceiling on concurrent remote jobs. Past this, fresh requests are
# refused with a structured reason. Local requests are not counted.
max_concurrent_remote_jobs = 4

# --- SEC-01: signer authorization (default-DENY) ---------------------------
#
# A signed job manifest only proves *some* keyholder signed it. To prove an
# *authorized* party signed it, this node checks the manifest's signer pubkey
# against the allowlist below AFTER verifying the signature.
#
# List the lowercase-hex Ed25519 public keys (64 hex chars each) you trust to
# submit work to this node. Empty list = nobody is authorized (default-deny).
#   authorized_submitters = [
#     "3b6a...e9",   # alice's client key
#     "f10c...22",   # ci runner
#   ]
authorized_submitters = []

# INSECURE escape hatch for local development only. When true, the signer
# allowlist is bypassed and ANY peer whose manifest verifies can use this
# node's GPU. Never enable on an internet-exposed node.
allow_unauthenticated_jobs = false

# Server-side hard ceiling on a job's requested max_tokens. A hostile client
# can ask for an enormous generation; this clamps it regardless of what the
# manifest claims.
max_tokens_ceiling = 8192
"#;

// ---------------------------------------------------------------------------
// Background watcher: system state + file change + SIGHUP
// ---------------------------------------------------------------------------

/// Internal event types the watcher loop handles.
enum WatcherEvent {
    /// Filesystem said the config file changed — re-read.
    ConfigChanged,
    /// SIGHUP (Unix only) — re-read.
    SighupReload,
    /// Periodic tick — refresh system state.
    PollState,
}

fn spawn_watcher(
    config: Arc<RwLock<PolicyConfig>>,
    state: Arc<RwLock<PolicyState>>,
    config_path: Option<PathBuf>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<WatcherEvent>(16);

        // --- filesystem watcher ---
        // `notify`'s watcher emits events on a crossbeam channel. We adapt
        // to tokio by forwarding from a sync callback.
        let _fs_watcher = config_path
            .as_ref()
            .and_then(|path| spawn_fs_watcher(path, tx.clone()).ok());

        // --- SIGHUP (Unix only) ---
        #[cfg(unix)]
        {
            let tx_sig = tx.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                if let Ok(mut hup) = signal(SignalKind::hangup()) {
                    while hup.recv().await.is_some() {
                        let _ = tx_sig.send(WatcherEvent::SighupReload).await;
                    }
                }
            });
        }

        // --- periodic state poll ---
        let tx_poll = tx.clone();
        tokio::spawn(async move {
            // Tick once immediately so first state is populated quickly.
            let _ = tx_poll.send(WatcherEvent::PollState).await;
            let mut interval = tokio::time::interval(STATE_POLL_INTERVAL);
            interval.tick().await; // first tick is immediate; we already sent.
            loop {
                interval.tick().await;
                if tx_poll.send(WatcherEvent::PollState).await.is_err() {
                    break;
                }
            }
        });

        // --- main loop ---
        while let Some(event) = rx.recv().await {
            match event {
                WatcherEvent::ConfigChanged | WatcherEvent::SighupReload => {
                    if let Some(p) = &config_path {
                        let p_clone = p.clone();
                        let read = tokio::task::spawn_blocking(move || read_config(&p_clone)).await;
                        match read {
                            Ok(Ok(new)) => {
                                if let Ok(mut guard) = config.write() {
                                    *guard = new;
                                }
                                tracing::info!(path = %p.display(), "policy config reloaded");
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(
                                    path = %p.display(),
                                    error = %e,
                                    "policy config reload failed; keeping previous config"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "policy reload join failed");
                            }
                        }
                    }
                }
                WatcherEvent::PollState => {
                    let probed = tokio::task::spawn_blocking(probe_system_state)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if let Ok(mut s) = state.write() {
                        s.on_battery = probed.on_battery;
                        s.temperature_c = probed.temperature_c;
                    }
                }
            }
        }
    })
}

/// Spawn a `notify` watcher on `path` that forwards `ConfigChanged` events
/// to the main watcher loop. We hold the watcher in a Box so the caller's
/// `Option<_>` keeps it alive for the duration of the engine.
fn spawn_fs_watcher(
    path: &Path,
    tx: mpsc::Sender<WatcherEvent>,
) -> Result<Box<dyn std::any::Any + Send>> {
    use notify::{RecursiveMode, Watcher};

    let watch_path = path.to_path_buf();
    let mut watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(_event) => {
                // We don't filter on event kind — text editors do all sorts
                // of weird things (atomic-rename writes, vim swap files,
                // etc.). Re-reading on any event is cheap and correct.
                let _ = tx.blocking_send(WatcherEvent::ConfigChanged);
            }
            Err(e) => {
                tracing::debug!(error = %e, "notify watcher error");
            }
        })?;

    // Watch the *parent* directory non-recursively. Watching the file
    // itself misses atomic-rename writes (editors do `write to tmp; rename
    // over original`), which would silently break reload-on-edit.
    if let Some(parent) = watch_path.parent() {
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
    } else {
        watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
    }

    Ok(Box::new(watcher))
}

// ---------------------------------------------------------------------------
// System state probing
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ProbedState {
    on_battery: Option<bool>,
    temperature_c: Option<f32>,
}

fn probe_system_state() -> ProbedState {
    ProbedState {
        on_battery: probe_battery(),
        temperature_c: probe_temperature(),
    }
}

/// `Some(true)` if any reported battery is currently discharging. `None`
/// if the host has no batteries (desktop) or we can't read them.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_battery() -> Option<bool> {
    let manager = battery::Manager::new().ok()?;
    let batteries = manager.batteries().ok()?;
    let mut saw_any = false;
    let mut any_discharging = false;
    for b in batteries.flatten() {
        saw_any = true;
        if matches!(b.state(), battery::State::Discharging) {
            any_discharging = true;
        }
    }
    if saw_any {
        Some(any_discharging)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe_battery() -> Option<bool> {
    None
}

/// Hottest component temperature in °C across the available sensors. `None`
/// if no usable sensor is exposed by the OS.
///
/// `sysinfo 0.32::Component::temperature()` returns `f32` directly. `NaN`
/// or negative values mean "no reading"; we filter those out so callers see
/// a real temperature or `None`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_temperature() -> Option<f32> {
    use sysinfo::Components;
    let mut components = Components::new_with_refreshed_list();
    components.refresh();
    let mut hottest: Option<f32> = None;
    for comp in components.iter() {
        let t = comp.temperature();
        if t.is_nan() || t <= 0.0 {
            continue;
        }
        hottest = Some(match hottest {
            Some(prev) if prev >= t => prev,
            _ => t,
        });
    }
    hottest
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn probe_temperature() -> Option<f32> {
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PolicyConfig {
        PolicyConfig::default()
    }

    fn st() -> PolicyState {
        PolicyState::default()
    }

    // --- decision branches -------------------------------------------------

    #[test]
    fn allow_when_defaults_and_known_model() {
        let d = decide(&cfg(), &st(), "qwen3-next-80b-q4", 0);
        assert_eq!(d, PolicyDecision::Allow);
    }

    #[test]
    fn manual_pause_wins_over_everything() {
        let mut c = cfg();
        c.manual_pause = true;
        // Even with a fine state and a known model, manual wins.
        let d = decide(&c, &st(), "qwen3-next-80b-q4", 0);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::Manual
            }
        );
    }

    #[test]
    fn pause_on_battery_when_configured() {
        let c = cfg(); // auto_pause_on_battery defaults to true.
        let mut s = st();
        s.on_battery = Some(true);
        let d = decide(&c, &s, "qwen3-next-80b-q4", 0);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::OnBattery
            }
        );
    }

    #[test]
    fn no_pause_on_battery_when_disabled() {
        let mut c = cfg();
        c.auto_pause_on_battery = false;
        let mut s = st();
        s.on_battery = Some(true);
        let d = decide(&c, &s, "qwen3-next-80b-q4", 0);
        assert_eq!(d, PolicyDecision::Allow);
    }

    #[test]
    fn unknown_battery_does_not_pause() {
        // Desktop / Windows / sensor-failure should not pause.
        let c = cfg();
        let s = st(); // on_battery = None.
        let d = decide(&c, &s, "qwen3-next-80b-q4", 0);
        assert_eq!(d, PolicyDecision::Allow);
    }

    #[test]
    fn pause_on_thermal_limit() {
        let c = cfg(); // threshold defaults to 75.0
        let mut s = st();
        s.temperature_c = Some(80.0);
        let d = decide(&c, &s, "qwen3-next-80b-q4", 0);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::ThermalLimit {
                    current_c: 80.0,
                    threshold_c: 75.0
                }
            }
        );
    }

    #[test]
    fn thermal_unset_disables_check() {
        let mut c = cfg();
        c.auto_pause_on_thermal_threshold_c = None;
        let mut s = st();
        s.temperature_c = Some(200.0); // would otherwise trip.
        let d = decide(&c, &s, "qwen3-next-80b-q4", 0);
        assert_eq!(d, PolicyDecision::Allow);
    }

    #[test]
    fn pause_on_outside_time_window_normal() {
        // Construct a window that *excludes* the current hour.
        let hour = chrono::Local::now().hour() as u8;
        // A 1-hour window 2 hours ahead of "now", non-wrapping.
        let start = (hour + 2) % 24;
        let end = (hour + 3) % 24;
        // Skip if the synthetic window happened to wrap or include now.
        if start == end || start >= end {
            return;
        }
        let mut c = cfg();
        c.time_of_day_window = Some(TimeWindow {
            start_hour_local: start,
            end_hour_local: end,
        });
        let d = decide(&c, &st(), "qwen3-next-80b-q4", 0);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::OutsideTimeWindow
            }
        );
    }

    #[test]
    fn pause_on_concurrency_limit() {
        let mut c = cfg();
        c.max_concurrent_remote_jobs = 2;
        let d = decide(&c, &st(), "qwen3-next-80b-q4", 2);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::ConcurrencyLimit
            }
        );
    }

    #[test]
    fn pause_on_model_not_in_allowlist() {
        let mut c = cfg();
        c.serve_models = vec!["qwen3-*".to_string()];
        let d = decide(&c, &st(), "deepseek-v3-q4", 0);
        assert_eq!(
            d,
            PolicyDecision::Pause {
                reason: PauseReason::ModelNotInAllowlist {
                    model_id: "deepseek-v3-q4".to_string()
                }
            }
        );
    }

    // --- glob matching -----------------------------------------------------

    #[test]
    fn glob_star_matches_everything() {
        let pats = vec!["*".to_string()];
        assert!(matches_any_glob(&pats, "qwen3-next-80b-q4"));
        assert!(matches_any_glob(&pats, "deepseek-v3-q4"));
        assert!(matches_any_glob(&pats, ""));
    }

    #[test]
    fn glob_qwen3_prefix_filters_correctly() {
        let pats = vec!["qwen3-*".to_string()];
        assert!(matches_any_glob(&pats, "qwen3-next-80b-q4"));
        assert!(!matches_any_glob(&pats, "deepseek-v3-q4"));
    }

    #[test]
    fn glob_multiple_patterns_any_match() {
        let pats = vec!["qwen3-*".to_string(), "deepseek-*".to_string()];
        assert!(matches_any_glob(&pats, "qwen3-next-80b-q4"));
        assert!(matches_any_glob(&pats, "deepseek-v3-q4"));
        assert!(!matches_any_glob(&pats, "llama-4-q4"));
    }

    #[test]
    fn glob_empty_allowlist_matches_nothing() {
        let pats: Vec<String> = vec![];
        assert!(!matches_any_glob(&pats, "qwen3-next-80b-q4"));
    }

    // --- TimeWindow --------------------------------------------------------

    #[test]
    fn time_window_normal_range() {
        let w = TimeWindow {
            start_hour_local: 9,
            end_hour_local: 17,
        };
        assert!(!w.contains_hour(8));
        assert!(w.contains_hour(9));
        assert!(w.contains_hour(12));
        assert!(w.contains_hour(16));
        assert!(!w.contains_hour(17));
        assert!(!w.contains_hour(23));
    }

    #[test]
    fn time_window_wraps_midnight() {
        // Overnight serving: 23:00 through 06:59.
        let w = TimeWindow {
            start_hour_local: 23,
            end_hour_local: 7,
        };
        assert!(w.contains_hour(23));
        assert!(w.contains_hour(0));
        assert!(w.contains_hour(3));
        assert!(w.contains_hour(6));
        assert!(!w.contains_hour(7));
        assert!(!w.contains_hour(12));
        assert!(!w.contains_hour(22));
    }

    #[test]
    fn time_window_degenerate_empty() {
        let w = TimeWindow {
            start_hour_local: 5,
            end_hour_local: 5,
        };
        // Operators wanting "always on" should set None, not start==end.
        // start==end is treated as "never" so the type is total.
        for h in 0..24 {
            assert!(!w.contains_hour(h), "hour {h} unexpectedly inside empty window");
        }
    }

    // --- TOML round-trip ---------------------------------------------------

    #[test]
    fn default_toml_round_trips() {
        let parsed: PolicyConfig = toml::from_str(DEFAULT_CONFIG_TOML)
            .expect("default TOML should parse");
        assert_eq!(parsed, PolicyConfig::default());
    }

    #[test]
    fn round_trip_explicit_config() {
        let original = PolicyConfig {
            auto_pause_on_battery: false,
            auto_pause_on_thermal_threshold_c: Some(82.5),
            vram_reserve_gb: 8.0,
            serve_models: vec!["qwen3-*".into(), "deepseek-v3-*".into()],
            time_of_day_window: Some(TimeWindow {
                start_hour_local: 22,
                end_hour_local: 6,
            }),
            manual_pause: false,
            max_concurrent_remote_jobs: 2,
            authorized_submitters: vec!["aa".repeat(32), "bb".repeat(32)],
            allow_unauthenticated_jobs: false,
            max_tokens_ceiling: 4096,
        };
        let serialized = toml::to_string(&original).expect("serialize");
        let parsed: PolicyConfig = toml::from_str(&serialized).expect("parse back");
        assert_eq!(parsed, original);
    }

    // --- PolicyEngine surface ---------------------------------------------

    #[tokio::test]
    async fn set_manual_pause_toggles_decisions() {
        let engine = PolicyEngine::new_for_tests(PolicyConfig::default(), PolicyState::default());

        // Initially: Allow.
        assert_eq!(
            engine.should_serve("qwen3-next-80b-q4", 0),
            PolicyDecision::Allow
        );

        engine.set_manual_pause(true).await;
        assert_eq!(
            engine.should_serve("qwen3-next-80b-q4", 0),
            PolicyDecision::Pause {
                reason: PauseReason::Manual
            }
        );

        engine.set_manual_pause(false).await;
        assert_eq!(
            engine.should_serve("qwen3-next-80b-q4", 0),
            PolicyDecision::Allow
        );
    }

    #[tokio::test]
    async fn injected_state_drives_battery_pause() {
        let engine = PolicyEngine::new_for_tests(PolicyConfig::default(), PolicyState::default());
        // Default state has on_battery = None → Allow.
        assert_eq!(
            engine.should_serve("qwen3-next-80b-q4", 0),
            PolicyDecision::Allow
        );

        engine.set_test_state(PolicyState {
            on_battery: Some(true),
            ..PolicyState::default()
        });
        assert_eq!(
            engine.should_serve("qwen3-next-80b-q4", 0),
            PolicyDecision::Pause {
                reason: PauseReason::OnBattery
            }
        );
    }

    #[tokio::test]
    async fn load_or_default_seeds_file_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("policy.toml");
        assert!(!path.exists());

        let engine = PolicyEngine::load_or_default(Some(path.clone()))
            .await
            .expect("load_or_default");
        assert!(path.exists(), "default file should have been written");

        // Round-trip: the seeded file should parse back to defaults.
        let parsed = read_config(&path).expect("re-read seeded file");
        assert_eq!(parsed, PolicyConfig::default());

        drop(engine);
    }

    #[tokio::test]
    async fn load_or_default_reads_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("policy.toml");
        std::fs::write(
            &path,
            "auto_pause_on_battery = false\n\
             auto_pause_on_thermal_threshold_c = 90.0\n\
             vram_reserve_gb = 2.0\n\
             serve_models = [\"qwen3-*\"]\n\
             manual_pause = false\n\
             max_concurrent_remote_jobs = 1\n",
        )
        .expect("seed file");

        let engine = PolicyEngine::load_or_default(Some(path))
            .await
            .expect("load_or_default");
        let cfg = engine.config();
        assert!(!cfg.auto_pause_on_battery);
        assert_eq!(cfg.auto_pause_on_thermal_threshold_c, Some(90.0));
        assert_eq!(cfg.max_concurrent_remote_jobs, 1);
        assert_eq!(cfg.serve_models, vec!["qwen3-*".to_string()]);
    }

    #[tokio::test]
    async fn reload_picks_up_file_edits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("policy.toml");
        let engine = PolicyEngine::load_or_default(Some(path.clone()))
            .await
            .expect("load_or_default");

        // Default: manual_pause = false.
        assert!(!engine.config().manual_pause);

        // Edit the file to flip manual_pause on.
        let mut edited = DEFAULT_CONFIG_TOML.to_string();
        edited = edited.replace("manual_pause = false", "manual_pause = true");
        std::fs::write(&path, edited).expect("rewrite");

        engine.reload().await.expect("reload");
        assert!(engine.config().manual_pause);
    }
}
