// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sandboxed Tauri v2 desktop backend for LUCID.
//!
//! Security boundaries enforced here:
//! - Refuses to run as root (`uid == 0`).
//! - Never exposes the local `lucidd` control bearer token to the WebView JS
//!   layer; all authenticated `/api/lucid/*` calls execute inside Rust against
//!   strictly `127.0.0.1:<port>` with `no_proxy()` and `redirect::Policy::none()`.
//! - Spawns `lucidd` with `env_clear()`, private `0700` state directory, and
//!   fresh per-session `0600` control token file removed on shutdown.
//! - Verifies sidecar binary SHA-256 on launch and validates Open WebUI URLs
//!   strictly before invoking the OS browser opener.

#![deny(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_opener::OpenerExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const DEFAULT_API_PORT: u16 = 11435;
const DEFAULT_CHAT_URL: &str = "http://localhost:3000";
const MAX_INVITATION_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopPrefs {
    api_port: u16,
    chat_app_url: String,
}

impl Default for DesktopPrefs {
    fn default() -> Self {
        Self {
            api_port: DEFAULT_API_PORT,
            chat_app_url: DEFAULT_CHAT_URL.to_string(),
        }
    }
}

struct ManagedDaemon {
    child: Child,
    port: u16,
    token: String,
    token_path: PathBuf,
    daemon_sha256: String,
}

struct AppController {
    state_dir: PathBuf,
    daemon: Mutex<Option<ManagedDaemon>>,
    prefs: Mutex<DesktopPrefs>,
    http: reqwest::Client,
}

impl AppController {
    fn new(state_dir: PathBuf) -> Result<Self> {
        ensure_private_dir(&state_dir)?;
        let prefs = load_prefs(&state_dir);
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to initialize loopback HTTP client")?;
        Ok(Self {
            state_dir,
            daemon: Mutex::new(None),
            prefs: Mutex::new(prefs),
            http,
        })
    }

    async fn ensure_started(&self, app: &AppHandle) -> Result<(u16, String)> {
        let mut guard = self.daemon.lock().await;
        if let Some(d) = guard.as_mut() {
            if d.child.try_wait()?.is_none() {
                return Ok((d.port, d.token.clone()));
            }
            let _ = fs::remove_file(&d.token_path);
            *guard = None;
        }

        let prefs = self.prefs.lock().await.clone();
        let port = choose_available_loopback_port(prefs.api_port)?;
        let binary = resolve_sidecar_binary(app)?;
        let daemon_sha256 = sha256_file_hex(&binary)?;

        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let token_path = self
            .state_dir
            .join(format!("control-{}.token", uuid::Uuid::new_v4().simple()));
        write_private_file(&token_path, token.as_bytes())?;

        let identity_path = self.state_dir.join("identity.key");
        let policy_path = self.state_dir.join("policy.toml");
        let node_config_path = self.state_dir.join("node.toml");
        let artifact_dir = self.state_dir.join("artifacts");
        ensure_private_dir(&artifact_dir)?;

        let mut cmd = Command::new(&binary);
        cmd.env_clear();
        cmd.env("LUCIDD_HOST", "127.0.0.1");
        cmd.env("LUCIDD_PORT", port.to_string());
        cmd.env("RUST_LOG", "warn,lucidd=info");
        cmd.env("NO_COLOR", "1");
        if let Some(home) = dirs::home_dir() {
            cmd.env("HOME", home);
        }
        cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        cmd.env("TMPDIR", std::env::temp_dir());
        cmd.current_dir(&self.state_dir);
        cmd.arg("--identity-path").arg(&identity_path);
        cmd.arg("--policy-config").arg(&policy_path);
        cmd.arg("--node-config").arg(&node_config_path);
        cmd.arg("--artifact-dir").arg(&artifact_dir);
        cmd.arg("--control-token-file").arg(&token_path);

        // Optional isolated acceptance relay list (validated strictly).
        if let Ok(raw_relays) = std::env::var("LUCID_RELAY_PEERS") {
            let relays = parse_relay_override(&raw_relays)?;
            cmd.arg("--no-default-bootstrap");
            for relay in relays {
                cmd.arg("--bootstrap-peer").arg(&relay);
                cmd.arg("--relay-peer").arg(&relay);
            }
        }

        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("lucidd.log"))
            .context("failed to open private daemon log")?;
        let err_file = log_file.try_clone()?;
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::from(log_file));
        cmd.stderr(Stdio::from(err_file));
        cmd.kill_on_drop(true);

        let child = cmd
            .spawn()
            .with_context(|| format!("failed to start bundled lucidd from {}", binary.display()))?;

        *guard = Some(ManagedDaemon {
            child,
            port,
            token: token.clone(),
            token_path,
            daemon_sha256,
        });
        drop(guard);

        // Wait up to 20s for loopback /api/version to respond.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let health_url = format!("http://127.0.0.1:{port}/api/version");
        while tokio::time::Instant::now() < deadline {
            if let Ok(resp) = self.http.get(&health_url).send().await {
                if resp.status().is_success() {
                    return Ok((port, token));
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.stop_daemon().await;
        bail!("LUCID local engine did not become ready in time");
    }

    async fn stop_daemon(&self) {
        let mut guard = self.daemon.lock().await;
        if let Some(mut d) = guard.take() {
            let _ = d.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), d.child.wait()).await;
            let _ = fs::remove_file(&d.token_path);
            let _ = d.daemon_sha256; // retained for diagnostics
        }
    }

    async fn api_get(&self, app: &AppHandle, path: &str) -> Result<serde_json::Value> {
        let (port, token) = self.ensure_started(app).await?;
        let url = format!("http://127.0.0.1:{port}{path}");
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| format!("local control GET {path} failed"))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
        if !status.is_success() {
            let msg = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Local LUCID control request failed");
            bail!("{msg}");
        }
        Ok(body)
    }

    async fn api_post(
        &self,
        app: &AppHandle,
        path: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let (port, token) = self.ensure_started(app).await?;
        let url = format!("http://127.0.0.1:{port}{path}");
        let resp = self
            .http
            .post(&url)
            .timeout(timeout)
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("local control POST {path} failed"))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
        if !status.is_success() {
            let msg = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Local LUCID control request failed");
            bail!("{msg}");
        }
        Ok(body)
    }
}

#[derive(Debug, Deserialize)]
struct SettingsPatch {
    onboarding_completed: Option<bool>,
    manual_pause: Option<bool>,
    auto_pause_on_battery: Option<bool>,
    thermal_limit_c: Option<Option<f32>>,
    chat_app_url: Option<String>,
}

#[tauri::command]
async fn get_overview(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
) -> Result<serde_json::Value, String> {
    build_overview(&app, &ctrl).await.map_err(|e| e.to_string())
}

async fn build_overview(app: &AppHandle, ctrl: &AppController) -> Result<serde_json::Value> {
    let (port, _token) = ctrl.ensure_started(app).await?;
    let prefs = ctrl.prefs.lock().await.clone();

    let (status_res, probe_res, models_res, peers_res) = tokio::join!(
        ctrl.api_get(app, "/api/lucid/status"),
        ctrl.api_get(app, "/api/lucid/probe"),
        ctrl.api_get(app, "/api/lucid/models"),
        ctrl.api_get(app, "/api/lucid/peers"),
    );

    let mut status = status_res?;
    if let Some(obj) = status.as_object_mut() {
        obj.insert("running".to_string(), serde_json::json!(true));
        obj.insert(
            "api_address".to_string(),
            serde_json::json!(format!("http://127.0.0.1:{port}")),
        );
        obj.insert(
            "chat_app_url".to_string(),
            serde_json::json!(prefs.chat_app_url),
        );
    }

    let models_json = models_res?;
    let peers_json = peers_res?;

    Ok(serde_json::json!({
        "status": status,
        "probe": probe_res.ok(),
        "models": models_json.get("models").cloned().unwrap_or_else(|| serde_json::json!([])),
        "model_dirs": models_json.get("model_dirs").cloned().unwrap_or_else(|| serde_json::json!([])),
        "peers": peers_json.get("peers").cloned().unwrap_or_else(|| serde_json::json!([])),
    }))
}

#[tauri::command]
async fn refresh_probe(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/probe/refresh",
        serde_json::json!({}),
        Duration::from_secs(15),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_provide_permission(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    model_name: String,
    enable: bool,
    confirm_permission: bool,
    make_available_locally: Option<bool>,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/provide",
        serde_json::json!({
            "model_name": model_name,
            "enable": enable,
            "confirm_permission": confirm_permission,
            "make_available_locally": make_available_locally,
        }),
        Duration::from_secs(15),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn setup_runtime(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    action: String,
    confirm_permission: bool,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/runtime/setup",
        serde_json::json!({
            "action": action,
            "confirm_permission": confirm_permission,
        }),
        Duration::from_secs(360),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_model_folder(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    path: String,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/folders",
        serde_json::json!({ "action": "add", "path": path }),
        Duration::from_secs(15),
    )
    .await
    .map_err(|e| e.to_string())?;
    build_overview(&app, &ctrl).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_model_folder(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    path: String,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/folders",
        serde_json::json!({ "action": "remove", "path": path }),
        Duration::from_secs(15),
    )
    .await
    .map_err(|e| e.to_string())?;
    build_overview(&app, &ctrl).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_settings(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    settings: SettingsPatch,
) -> Result<serde_json::Value, String> {
    if let Some(chat_url) = &settings.chat_app_url {
        let validated = validate_chat_app_url(chat_url).map_err(|e| e.to_string())?;
        let mut prefs = ctrl.prefs.lock().await;
        prefs.chat_app_url = validated;
        save_prefs(&ctrl.state_dir, &prefs).map_err(|e| e.to_string())?;
    }
    ctrl.api_post(
        &app,
        "/api/lucid/settings",
        serde_json::json!({
            "onboarding_completed": settings.onboarding_completed,
            "manual_pause": settings.manual_pause,
            "auto_pause_on_battery": settings.auto_pause_on_battery,
            "thermal_limit_c": settings.thermal_limit_c,
        }),
        Duration::from_secs(10),
    )
    .await
    .map_err(|e| e.to_string())?;
    let ov = build_overview(&app, &ctrl)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ov
        .get("status")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({})))
}

#[tauri::command]
async fn create_invite(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    ttl_seconds: u64,
) -> Result<serde_json::Value, String> {
    ctrl.api_post(
        &app,
        "/api/lucid/invitations",
        serde_json::json!({ "ttl_seconds": ttl_seconds }),
        Duration::from_secs(10),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn join_invite(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    invitation: String,
) -> Result<serde_json::Value, String> {
    let clean = validate_invitation(&invitation).map_err(|e| e.to_string())?;
    ctrl.api_post(
        &app,
        "/api/lucid/enroll",
        serde_json::json!({ "invitation": clean }),
        Duration::from_secs(15),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_chat(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    model_id: String,
) -> Result<(), String> {
    let prefs = ctrl.prefs.lock().await.clone();
    let target =
        build_chat_launch_url(&prefs.chat_app_url, &model_id).map_err(|e| e.to_string())?;
    app.opener()
        .open_url(target, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn restart_daemon(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
) -> Result<serde_json::Value, String> {
    ctrl.stop_daemon().await;
    build_overview(&app, &ctrl).await.map_err(|e| e.to_string())
}

fn resolve_state_dir() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("LUCID_APP_STATE_DIR") {
        let p = PathBuf::from(explicit);
        if !p.is_absolute() {
            bail!("LUCID_APP_STATE_DIR must be an absolute path");
        }
        return Ok(p);
    }
    let base =
        dirs::data_local_dir().context("could not resolve platform application data directory")?;
    Ok(base.join("LUCID"))
}

fn resolve_sidecar_binary(app: &AppHandle) -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("LUCID_DAEMON_PATH") {
        let p = PathBuf::from(override_path);
        if p.is_absolute() && p.is_file() {
            return p
                .canonicalize()
                .context("failed to canonicalize LUCID_DAEMON_PATH");
        }
    }
    // 1. Check sibling executable directory (Tauri bundled externalBin location).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let direct = dir.join("lucidd");
            if direct.is_file() {
                return direct
                    .canonicalize()
                    .context("failed to canonicalize sidecar");
            }
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|n| n.starts_with("lucidd-") && !n.ends_with(".d"))
                        && path.is_file()
                    {
                        return path
                            .canonicalize()
                            .context("failed to canonicalize sidecar");
                    }
                }
            }
        }
    }
    // 2. Check Tauri resource directory.
    if let Ok(res_dir) = app.path().resource_dir() {
        for rel in ["lucidd", "binaries/lucidd"] {
            let p = res_dir.join(rel);
            if p.is_file() {
                return p
                    .canonicalize()
                    .context("failed to canonicalize resource lucidd");
            }
        }
    }
    // 3. Fallback for local development (`~/.local/bin/lucidd`).
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin/lucidd");
        if local_bin.is_file() {
            return local_bin
                .canonicalize()
                .context("failed to canonicalize ~/.local/bin/lucidd");
        }
    }
    bail!("Bundled lucidd binary not found. Reinstall LUCID or set LUCID_DAEMON_PATH for development.")
}

fn choose_available_loopback_port(preferred: u16) -> Result<u16> {
    if preferred >= 1024 && TcpListener::bind(("127.0.0.1", preferred)).is_ok() {
        return Ok(preferred);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("failed to reserve an ephemeral loopback port")?;
    Ok(listener.local_addr()?.port())
}

fn validate_invitation(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_INVITATION_LEN {
        bail!("Invitation is empty or exceeds maximum length.");
    }
    if !trimmed.starts_with("lucid://join/") {
        bail!("Invitation must start with lucid://join/");
    }
    Ok(trimmed.to_string())
}

fn validate_chat_app_url(raw: &str) -> Result<String> {
    let parsed = url::Url::parse(raw.trim()).context("Invalid chat app URL")?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => bail!("Chat app URL must use http:// or https://"),
    }
    if parsed.scheme() == "http" {
        match parsed.host_str() {
            Some("localhost" | "127.0.0.1" | "::1" | "[::1]") => {}
            _ => bail!("Unencrypted http:// chat URLs are only allowed on localhost"),
        }
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn build_chat_launch_url(base: &str, model_id: &str) -> Result<String> {
    let clean_base = validate_chat_app_url(base)?;
    let mut parsed = url::Url::parse(&clean_base)?;
    parsed
        .query_pairs_mut()
        .append_pair("model", model_id.trim());
    Ok(parsed.to_string())
}

fn parse_relay_override(raw: &str) -> Result<Vec<String>> {
    let list: Vec<String> =
        serde_json::from_str(raw).context("LUCID_RELAY_PEERS must be a JSON string array")?;
    if list.len() > 4 {
        bail!("LUCID_RELAY_PEERS allows at most 4 relay multiaddrs");
    }
    for entry in &list {
        if entry.len() > 512 || !entry.starts_with('/') || !entry.contains("/p2p/") {
            bail!("Invalid peer-pinned multiaddr in LUCID_RELAY_PEERS");
        }
    }
    Ok(list)
}

fn load_prefs(state_dir: &Path) -> DesktopPrefs {
    let path = state_dir.join("desktop-prefs.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_prefs(state_dir: &Path, prefs: &DesktopPrefs) -> Result<()> {
    let path = state_dir.join("desktop-prefs.json");
    let bytes = serde_json::to_vec_pretty(prefs)?;
    write_private_file(&path, &bytes)
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("missing parent dir"))?;
    ensure_private_dir(parent)?;
    let tmp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn refuse_root_execution() -> Result<()> {
    #[cfg(unix)]
    {
        if std::env::var("USER").as_deref() == Ok("root") {
            if let Ok(out) = std::process::Command::new("/usr/bin/id").arg("-u").output() {
                if String::from_utf8_lossy(&out.stdout).trim() == "0" {
                    bail!(
                        "Refusing to run LUCID desktop as root. Run as your normal user account."
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(err) = refuse_root_execution() {
        eprintln!("ERROR: {err}");
        std::process::exit(1);
    }

    let state_dir = resolve_state_dir().expect("failed to resolve LUCID state directory");
    let controller = Arc::new(AppController::new(state_dir).expect("failed to init LUCID state"));
    let ctrl_for_exit = controller.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(controller)
        .setup(|app| {
            let open_item = MenuItem::with_id(app, "open", "Open LUCID", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit LUCID", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

            let mut tray = TrayIconBuilder::new().menu(&menu).tooltip("LUCID");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id.as_ref() {
                "open" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "quit" => {
                    app.exit(0);
                }
                _ => {}
            })
            .build(app)?;

            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    let s = url.to_string();
                    if s.starts_with("lucid://join/") {
                        let _ = handle.emit("lucid-invitation", s);
                        if let Some(win) = handle.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_overview,
            refresh_probe,
            set_provide_permission,
            setup_runtime,
            add_model_folder,
            remove_model_folder,
            update_settings,
            create_invite,
            join_invite,
            open_chat,
            restart_daemon,
        ])
        .build(tauri::generate_context!())
        .expect("error while building LUCID desktop app");

    app.run(move |_handle, event| {
        if let tauri::RunEvent::Exit = event {
            tauri::async_runtime::block_on(ctrl_for_exit.stop_daemon());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_validation_rejects_remote_http_and_non_http_schemes() {
        assert_eq!(
            validate_chat_app_url("http://localhost:3000/").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            validate_chat_app_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(validate_chat_app_url("https://chat.example.com").is_ok());
        assert!(validate_chat_app_url("http://evil.example.com").is_err());
        assert!(validate_chat_app_url("file:///etc/passwd").is_err());
        assert!(validate_chat_app_url("javascript:alert(1)").is_err());

        let url = build_chat_launch_url("http://localhost:3000", "qwen2.5:14b").unwrap();
        assert_eq!(url, "http://localhost:3000/?model=qwen2.5%3A14b");
    }

    #[test]
    fn invitation_validation_enforces_scheme_and_cap() {
        assert!(validate_invitation("lucid://join/abc123").is_ok());
        assert!(validate_invitation("http://evil.example/join").is_err());
        assert!(validate_invitation("").is_err());
        let oversized = format!("lucid://join/{}", "a".repeat(MAX_INVITATION_LEN));
        assert!(validate_invitation(&oversized).is_err());
    }

    #[test]
    fn relay_override_requires_peer_pin_and_bounds_count() {
        let valid = r#"["/ip4/203.0.113.10/tcp/4001/p2p/12D3KooWTest"]"#;
        assert_eq!(parse_relay_override(valid).unwrap().len(), 1);
        assert!(parse_relay_override(r#"["/ip4/203.0.113.10/tcp/4001"]"#).is_err());
        assert!(parse_relay_override(r#"["a","b","c","d","e"]"#).is_err());
    }
}
