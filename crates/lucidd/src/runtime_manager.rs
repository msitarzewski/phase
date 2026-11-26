// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only host capability probe, existing-inference discovery, and
//! SHA-256-pinned `llama-server` runtime manager.
//!
//! Security & privacy invariants:
//! - Probing is **strictly read-only**: it never writes files, never copies
//!   weights, never enables serving, and never shares anything over the
//!   network until the user explicitly grants permission.
//! - Subprocess probes (`nvidia-smi`, `llama-server --version`) run with an
//!   empty environment (`env_clear()`), absolute binary paths (no runtime
//!   `PATH` lookup), and tight timeouts.
//! - Pinned `llama-server` downloads enforce HTTPS-only, strict byte cap,
//!   fail-closed SHA-256 verification BEFORE extraction, archive-entry path
//!   validation (rejecting `..`, leading `/`, and symlink escapes), and unpack
//!   strictly inside a `0700` private runtime directory.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::System;

use crate::node_config::{ensure_private_dir, NodeConfig};

/// Upstream `ggml-org/llama.cpp` release tag pinned for automatic runtime
/// setup. Every downloadable archive below has its exact upstream SHA-256.
pub const PINNED_LLAMA_CPP_TAG: &str = "b12651";

/// Maximum download size for a pinned `llama.cpp` archive (320 MiB — covers
/// the ~220 MiB CUDA Linux tarball while rejecting unbounded streams).
const MAX_RUNTIME_ARCHIVE_BYTES: u64 = 320 * 1024 * 1024;
const MAX_DISCOVERED_MODELS: usize = 64;
const MAX_SCAN_DEPTH: usize = 4;
const GGUF_MAGIC: &[u8; 4] = b"GGUF";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedLlamaAsset {
    pub tag: &'static str,
    pub platform_id: &'static str,
    pub backend: &'static str,
    pub filename: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
}

impl PinnedLlamaAsset {
    pub fn download_url(&self) -> String {
        format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{}/{}",
            self.tag, self.filename
        )
    }
}

/// Verified upstream `ggml-org/llama.cpp` `b12651` release assets.
pub const PINNED_LLAMA_ASSETS: &[PinnedLlamaAsset] = &[
    PinnedLlamaAsset {
        tag: PINNED_LLAMA_CPP_TAG,
        platform_id: "macos-arm64-metal",
        backend: "metal",
        filename: "llama-b12651-bin-macos-arm64.tar.gz",
        sha256: "264fd872de61679c45ad57e1e21c2a2a54e29b35734ecf3d50f24166447ab8d2",
        size_bytes: 16_644_358,
    },
    PinnedLlamaAsset {
        tag: PINNED_LLAMA_CPP_TAG,
        platform_id: "linux-x86_64-cuda",
        backend: "cuda",
        filename: "llama-b12651-bin-ubuntu-cuda-12.8-x64.tar.gz",
        sha256: "cb099350c6a025e67eefaa13f2b67b2dcce53e38a0da22476e070d166ef367ea",
        size_bytes: 220_195_495,
    },
    PinnedLlamaAsset {
        tag: PINNED_LLAMA_CPP_TAG,
        platform_id: "linux-x86_64-vulkan",
        backend: "vulkan",
        filename: "llama-b12651-bin-ubuntu-vulkan-x64.tar.gz",
        sha256: "0bbcf2093f15c64c8a9bf54b809c9b2c812b982992dc2fbc9b36720c1d32cf13",
        size_bytes: 36_976_824,
    },
    PinnedLlamaAsset {
        tag: PINNED_LLAMA_CPP_TAG,
        platform_id: "linux-x86_64-cpu",
        backend: "cpu",
        filename: "llama-b12651-bin-ubuntu-x64.tar.gz",
        sha256: "17b242259181d5b1b1f7719aba27929700a8b0dab39bfaef1b971a992b397b38",
        size_bytes: 23_297_838,
    },
    PinnedLlamaAsset {
        tag: PINNED_LLAMA_CPP_TAG,
        platform_id: "linux-aarch64-cpu",
        backend: "cpu",
        filename: "llama-b12651-bin-ubuntu-arm64.tar.gz",
        sha256: "b4a8d17e16a1b42f6a1395dc5cd6967ec0d98005eb3be85b7eb893b082306ed4",
        size_bytes: 18_813_487,
    },
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareCapability {
    pub os: String,
    pub arch: String,
    pub cpu_cores: usize,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub gpu_backend: String,
    pub gpu_name: Option<String>,
    pub gpu_vram_bytes: Option<u64>,
    pub recommended_max_model_bytes: u64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingInferenceStatus {
    pub ollama_running: bool,
    pub ollama_version: Option<String>,
    pub ollama_store_found: bool,
    pub lmstudio_store_found: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeBinaryStatus {
    /// `"ready"` (pinned & hash-verified), `"detected_unpinned"` (found on
    /// machine, waiting for user permission to pin), `"not_installed"`, or
    /// `"tampered"` (pinned hash mismatch — fails closed).
    pub status: String,
    pub pinned_path: Option<String>,
    pub pinned_sha256: Option<String>,
    pub detected_path: Option<String>,
    pub downloadable_tag: Option<String>,
    pub downloadable_backend: Option<String>,
    pub downloadable_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredLocalModel {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    /// `"ollama"`, `"lmstudio"`, or `"local"`
    pub source: String,
    pub quantization: Option<String>,
    pub fits_hardware: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProbe {
    pub hardware: HardwareCapability,
    pub existing_inference: ExistingInferenceStatus,
    pub runtime: RuntimeBinaryStatus,
    pub discovered_models: Vec<DiscoveredLocalModel>,
    pub onboarding_completed: bool,
    pub providing_enabled: bool,
}

/// Select the best pinned upstream `llama.cpp` release asset for this host.
pub fn select_pinned_asset(os: &str, arch: &str, gpu_backend: &str) -> Option<PinnedLlamaAsset> {
    let target_id = match (os, arch, gpu_backend) {
        ("macos", "aarch64", _) => "macos-arm64-metal",
        ("linux", "x86_64", "cuda") => "linux-x86_64-cuda",
        ("linux", "x86_64", "vulkan") => "linux-x86_64-vulkan",
        ("linux", "x86_64", _) => "linux-x86_64-cpu",
        ("linux", "aarch64", _) => "linux-aarch64-cpu",
        _ => return None,
    };
    PINNED_LLAMA_ASSETS
        .iter()
        .copied()
        .find(|a| a.platform_id == target_id)
}

/// Default private directory for LUCID-managed runtime binaries
/// (`~/.local/share/lucidd/runtime` on Linux, `~/Library/Application Support/lucidd/runtime` on macOS).
pub fn default_runtime_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir()
        .context("could not resolve local data directory for LUCID runtime")?;
    Ok(base.join("lucidd").join("runtime"))
}

/// Compute lowercase hex SHA-256 of a regular file on disk.
pub fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Run a strictly read-only probe of host hardware, existing inference tools
/// (Ollama / LM Studio), local `.gguf` files, and `llama-server` status.
pub async fn probe_system(node_cfg: &NodeConfig) -> SystemProbe {
    let extra_dirs = node_cfg.model_dirs.clone();
    let cfg_clone = node_cfg.clone();

    let ollama_version = probe_local_ollama_version().await;
    let ollama_running = ollama_version.is_some();

    let (hardware, existing_inference, runtime, discovered_models) =
        tokio::task::spawn_blocking(move || {
            let hardware = probe_hardware_blocking();
            let (ollama_models, ollama_store_found) =
                scan_ollama_models_readonly(hardware.recommended_max_model_bytes);
            let (lmstudio_models, lmstudio_store_found) =
                scan_lmstudio_models_readonly(hardware.recommended_max_model_bytes);
            let local_models =
                scan_user_model_dirs_readonly(&extra_dirs, hardware.recommended_max_model_bytes);

            let mut discovered = Vec::new();
            let mut seen_paths = HashSet::new();
            for model in ollama_models
                .into_iter()
                .chain(lmstudio_models)
                .chain(local_models)
            {
                if discovered.len() >= MAX_DISCOVERED_MODELS {
                    break;
                }
                if seen_paths.insert(model.path.clone()) {
                    discovered.push(model);
                }
            }
            discovered.sort_by(|a, b| a.name.cmp(&b.name));

            let runtime = inspect_runtime_status(&cfg_clone, &hardware);
            let existing = ExistingInferenceStatus {
                ollama_running,
                ollama_version,
                ollama_store_found,
                lmstudio_store_found,
            };
            (hardware, existing, runtime, discovered)
        })
        .await
        .unwrap_or_else(|_| {
            let hardware = fallback_hardware();
            let runtime = inspect_runtime_status(node_cfg, &hardware);
            (
                hardware,
                ExistingInferenceStatus {
                    ollama_running: false,
                    ollama_version: None,
                    ollama_store_found: false,
                    lmstudio_store_found: false,
                },
                runtime,
                Vec::new(),
            )
        });

    SystemProbe {
        hardware,
        existing_inference,
        runtime,
        discovered_models,
        onboarding_completed: node_cfg.onboarding_completed,
        providing_enabled: node_cfg.providing_enabled,
    }
}

/// Pin an already-installed `llama-server` executable after verifying it is an
/// absolute regular executable file and passes a clean `--version` check.
pub async fn pin_existing_runtime(candidate: &Path) -> Result<(PathBuf, String)> {
    let candidate = candidate.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if !candidate.is_absolute() {
            bail!("runtime binary path must be absolute");
        }
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("cannot find {}", candidate.display()))?;
        verify_executable_file(&canonical)?;
        verify_llama_server_version(&canonical)?;
        let sha256 = sha256_file_hex(&canonical)?;
        Ok((canonical, sha256))
    })
    .await
    .context("runtime pin task failed")?
}

/// Download the pinned upstream `llama.cpp` archive for this host, verify its
/// exact SHA-256 BEFORE extraction, validate archive paths against traversal
/// and symlink escapes, unpack into `runtime_root/<tag>/`, and return the
/// canonical `llama-server` path and its executable SHA-256.
pub async fn download_and_install_pinned_runtime(
    hardware: &HardwareCapability,
    runtime_root: Option<PathBuf>,
) -> Result<(PathBuf, String, PinnedLlamaAsset)> {
    let asset = select_pinned_asset(&hardware.os, &hardware.arch, &hardware.gpu_backend)
        .ok_or_else(|| {
            anyhow!(
                "no pinned llama-server release available for {}/{} ({})",
                hardware.os,
                hardware.arch,
                hardware.gpu_backend
            )
        })?;

    let root = match runtime_root {
        Some(dir) => dir,
        None => default_runtime_dir()?,
    };
    ensure_private_dir(&root)?;

    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent(format!("lucidd/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTPS client for runtime download")?;

    let url = asset.download_url();
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to download {}", asset.filename))?;
    if !resp.status().is_success() {
        bail!(
            "download failed for {} with HTTP {}",
            asset.filename,
            resp.status()
        );
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_RUNTIME_ARCHIVE_BYTES {
            bail!(
                "remote archive {} exceeds safety cap ({len} > {MAX_RUNTIME_ARCHIVE_BYTES})",
                asset.filename
            );
        }
    }

    let tmp_archive = root.join(format!(
        ".download-{}.tar.gz",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&tmp_archive)?
    };

    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut stream = resp.bytes_stream();
    let download_res: Result<()> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("interrupted runtime download")?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_RUNTIME_ARCHIVE_BYTES {
                bail!("runtime archive exceeded maximum allowed size");
            }
            hasher.update(&chunk);
            file.write_all(&chunk)?;
        }
        file.sync_all()?;
        Ok(())
    }
    .await;

    if let Err(err) = download_res {
        let _ = fs::remove_file(&tmp_archive);
        return Err(err);
    }
    drop(file);

    let actual_sha256 = hex::encode(hasher.finalize());
    if actual_sha256 != asset.sha256 {
        let _ = fs::remove_file(&tmp_archive);
        bail!(
            "SHA-256 verification failed for {}: expected {}, got {}",
            asset.filename,
            asset.sha256,
            actual_sha256
        );
    }

    let dest_dir = root.join(format!("{}-{}", asset.tag, asset.platform_id));
    let extract_res = tokio::task::spawn_blocking(move || -> Result<(PathBuf, String)> {
        let res = extract_verified_tar_gz(&tmp_archive, &dest_dir);
        let _ = fs::remove_file(&tmp_archive);
        let binary = res?;
        verify_executable_file(&binary)?;
        let bin_sha = sha256_file_hex(&binary)?;
        Ok((binary, bin_sha))
    })
    .await
    .context("runtime extraction task failed")??;

    Ok((extract_res.0, extract_res.1, asset))
}

/// Validate every entry in a `.tar.gz` before unpacking (rejecting `..`,
/// leading `/`, or non-regular/non-dir/non-internal entries), unpack into a
/// private staging directory, atomically move into `dest_dir`, and locate
/// `llama-server`.
fn extract_verified_tar_gz(archive: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let tar_bin = find_system_tar()?;

    // 1. Inspect archive table of contents BEFORE extracting a single byte.
    let list_out = std::process::Command::new(&tar_bin)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("-tzf")
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .context("failed to list runtime archive entries")?;
    if !list_out.status.success() {
        bail!("failed to inspect runtime archive");
    }
    let listing = String::from_utf8_lossy(&list_out.stdout);
    let mut entry_count = 0usize;
    for raw_line in listing.lines() {
        let entry = raw_line.trim();
        if entry.is_empty() {
            continue;
        }
        entry_count += 1;
        if entry_count > 1024 {
            bail!("runtime archive contains too many entries");
        }
        validate_archive_entry_path(entry)?;
    }

    // 2. Unpack into a private sibling staging directory.
    let parent = dest_dir
        .parent()
        .context("runtime destination has no parent")?;
    let stage = parent.join(format!(".stage-{}", uuid::Uuid::new_v4().simple()));
    ensure_private_dir(&stage)?;

    let extract_status = std::process::Command::new(&tar_bin)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&stage)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .context("failed to extract runtime archive")?;

    if !extract_status.success() {
        let _ = fs::remove_dir_all(&stage);
        bail!("tar extraction failed with status {extract_status}");
    }

    // 3. Reject any extracted symlink pointing outside the staging root.
    if let Err(err) = ensure_no_escaping_symlinks(&stage, &stage) {
        let _ = fs::remove_dir_all(&stage);
        return Err(err);
    }

    // 4. Atomically swap into place.
    if dest_dir.exists() {
        fs::remove_dir_all(dest_dir)
            .with_context(|| format!("failed to replace {}", dest_dir.display()))?;
    }
    fs::rename(&stage, dest_dir)
        .with_context(|| format!("failed to finalize runtime dir {}", dest_dir.display()))?;

    // 5. Locate `llama-server` executable inside `dest_dir`.
    let binary = find_file_named(dest_dir, "llama-server", 4)
        .ok_or_else(|| anyhow!("extracted archive did not contain llama-server"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    }
    binary
        .canonicalize()
        .context("failed to canonicalize extracted llama-server")
}

fn validate_archive_entry_path(entry: &str) -> Result<()> {
    if entry.starts_with('/') || entry.starts_with('\\') {
        bail!("archive entry has absolute path: {entry}");
    }
    let path = Path::new(entry);
    for comp in path.components() {
        if matches!(
            comp,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("archive entry attempts path traversal: {entry}");
        }
    }
    Ok(())
}

fn ensure_no_escaping_symlinks(root: &Path, current: &Path) -> Result<()> {
    let canonical_root = root.canonicalize()?;
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            let resolved = path
                .canonicalize()
                .with_context(|| format!("dangling symlink in archive: {}", path.display()))?;
            if !resolved.starts_with(&canonical_root) {
                bail!("archive symlink escapes staging root: {}", path.display());
            }
        } else if meta.is_dir() {
            ensure_no_escaping_symlinks(root, &path)?;
        }
    }
    Ok(())
}

fn find_system_tar() -> Result<PathBuf> {
    for candidate in ["/usr/bin/tar", "/bin/tar"] {
        let p = Path::new(candidate);
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
    }
    bail!("system tar binary not found at /usr/bin/tar or /bin/tar")
}

fn find_file_named(dir: &Path, name: &str, max_depth: usize) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_file() && path.file_name().and_then(|s| s.to_str()) == Some(name) {
            return Some(path);
        }
        if meta.is_dir() {
            if let Some(found) = find_file_named(&path, name, max_depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn inspect_runtime_status(
    node_cfg: &NodeConfig,
    hardware: &HardwareCapability,
) -> RuntimeBinaryStatus {
    let downloadable = select_pinned_asset(&hardware.os, &hardware.arch, &hardware.gpu_backend);

    if let Some(pinned) = &node_cfg.llama_server_binary {
        let verified = match (
            verify_executable_file(pinned),
            &node_cfg.llama_server_sha256,
        ) {
            (Ok(()), Some(expected)) => match sha256_file_hex(pinned) {
                Ok(actual) if actual.eq_ignore_ascii_case(expected) => "ready",
                Ok(_) => "tampered",
                Err(_) => "not_installed",
            },
            (Ok(()), None) => "ready",
            (Err(_), _) => "not_installed",
        };
        return RuntimeBinaryStatus {
            status: verified.to_string(),
            pinned_path: Some(pinned.display().to_string()),
            pinned_sha256: node_cfg.llama_server_sha256.clone(),
            detected_path: None,
            downloadable_tag: downloadable.map(|a| a.tag.to_string()),
            downloadable_backend: downloadable.map(|a| a.backend.to_string()),
            downloadable_size_bytes: downloadable.map(|a| a.size_bytes),
        };
    }

    let detected = detect_existing_llama_server();
    RuntimeBinaryStatus {
        status: if detected.is_some() {
            "detected_unpinned".to_string()
        } else {
            "not_installed".to_string()
        },
        pinned_path: None,
        pinned_sha256: None,
        detected_path: detected.map(|p| p.display().to_string()),
        downloadable_tag: downloadable.map(|a| a.tag.to_string()),
        downloadable_backend: downloadable.map(|a| a.backend.to_string()),
        downloadable_size_bytes: downloadable.map(|a| a.size_bytes),
    }
}

fn detect_existing_llama_server() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(rt_dir) = default_runtime_dir() {
        if let Some(found) = find_file_named(&rt_dir, "llama-server", 3) {
            candidates.push(found);
        }
    }
    for fixed in [
        "/opt/homebrew/bin/llama-server",
        "/usr/local/bin/llama-server",
        "/usr/bin/llama-server",
    ] {
        candidates.push(PathBuf::from(fixed));
    }
    for path in candidates {
        if let Ok(canon) = path.canonicalize() {
            if verify_executable_file(&canon).is_ok() {
                return Some(canon);
            }
        }
    }
    None
}

fn verify_executable_file(path: &Path) -> Result<()> {
    let meta = fs::metadata(path)
        .with_context(|| format!("cannot inspect executable {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            bail!("{} is not executable", path.display());
        }
    }
    Ok(())
}

fn verify_llama_server_version(path: &Path) -> Result<()> {
    let output = std::process::Command::new(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {} --version", path.display()))?;
    if !output.status.success() {
        bail!(
            "{} --version exited with status {}",
            path.display(),
            output.status
        );
    }
    Ok(())
}

async fn probe_local_ollama_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(400))
        .build()
        .ok()?;
    let resp = client
        .get("http://127.0.0.1:11434/api/version")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("version")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn probe_hardware_blocking() -> HardwareCapability {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::new());

    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let cpu_cores = sys.cpus().len().max(1);
    let total_memory_bytes = sys.total_memory();
    let available_memory_bytes = sys.available_memory();

    let (gpu_backend, gpu_name, gpu_vram_bytes) = detect_gpu_blocking(&os, &arch);

    let recommended_max_model_bytes = match gpu_backend.as_str() {
        "metal" => (total_memory_bytes as f64 * 0.65) as u64,
        "cuda" | "vulkan" => gpu_vram_bytes
            .map(|v| (v as f64 * 0.80) as u64)
            .unwrap_or_else(|| (total_memory_bytes as f64 * 0.50) as u64),
        _ => (total_memory_bytes as f64 * 0.50) as u64,
    };

    let ram_gb = total_memory_bytes / (1024 * 1024 * 1024);
    let summary = match (&gpu_backend[..], &gpu_name, gpu_vram_bytes) {
        ("metal", Some(name), _) => format!("{name} · {ram_gb} GB unified memory (Metal)"),
        ("metal", None, _) => format!("Apple Silicon · {ram_gb} GB unified memory (Metal)"),
        ("cuda", Some(name), Some(vram)) => {
            let vram_gb = vram / (1024 * 1024 * 1024);
            format!("{name} ({vram_gb} GB VRAM) · {ram_gb} GB RAM (CUDA)")
        }
        ("cuda", Some(name), None) => format!("{name} · {ram_gb} GB RAM (CUDA)"),
        ("vulkan", Some(name), Some(vram)) => {
            let vram_gb = vram / (1024 * 1024 * 1024);
            format!("{name} ({vram_gb} GB VRAM) · {ram_gb} GB RAM (Vulkan)")
        }
        ("vulkan", Some(name), None) => format!("{name} · {ram_gb} GB RAM (Vulkan)"),
        _ => format!("{cpu_cores}-core CPU · {ram_gb} GB RAM"),
    };

    HardwareCapability {
        os,
        arch,
        cpu_cores,
        total_memory_bytes,
        available_memory_bytes,
        gpu_backend,
        gpu_name,
        gpu_vram_bytes,
        recommended_max_model_bytes,
        summary,
    }
}

fn fallback_hardware() -> HardwareCapability {
    HardwareCapability {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_cores: 1,
        total_memory_bytes: 0,
        available_memory_bytes: 0,
        gpu_backend: "cpu".to_string(),
        gpu_name: None,
        gpu_vram_bytes: None,
        recommended_max_model_bytes: 0,
        summary: "Unknown hardware".to_string(),
    }
}

fn detect_gpu_blocking(os: &str, arch: &str) -> (String, Option<String>, Option<u64>) {
    if os == "macos" && arch == "aarch64" {
        let chip = std::process::Command::new("/usr/sbin/sysctl")
            .env_clear()
            .args(["-n", "machdep.cpu.brand_string"])
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        return ("metal".to_string(), chip, None);
    }

    if os == "linux" {
        for smi in ["/usr/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"] {
            if !Path::new(smi).is_file() {
                continue;
            }
            if let Ok(out) = std::process::Command::new(smi)
                .env_clear()
                .args([
                    "--query-gpu=name,memory.total",
                    "--format=csv,noheader,nounits",
                ])
                .stdin(Stdio::null())
                .output()
            {
                if out.status.success() {
                    let line = String::from_utf8_lossy(&out.stdout);
                    if let Some(first) = line.lines().next() {
                        let mut parts = first.split(',').map(str::trim);
                        let name = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
                        let vram = parts
                            .next()
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|mib| mib.saturating_mul(1024 * 1024));
                        if name.is_some() {
                            // Prefer CUDA only when the host actually has the
                            // CUDA 12 runtime libraries (`libcudart.so.12` +
                            // `libcublas.so.12`); otherwise use Vulkan GPU
                            // acceleration (zero CUDA toolkit required) so
                            // `llama-server` never silently falls back to CPU.
                            let backend = if has_linux_cuda12_runtime() {
                                "cuda"
                            } else if has_linux_vulkan_runtime() {
                                "vulkan"
                            } else {
                                "cpu"
                            };
                            return (backend.to_string(), name, vram);
                        }
                    }
                }
            }
        }
    }

    ("cpu".to_string(), None, None)
}

fn has_linux_cuda12_runtime() -> bool {
    let lib_dirs = [
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/local/cuda/lib64",
        "/usr/local/cuda-12/lib64",
        "/usr/local/cuda-12.8/lib64",
        "/usr/local/cuda-12.6/lib64",
    ];
    let has_cudart = lib_dirs
        .iter()
        .any(|d| Path::new(d).join("libcudart.so.12").exists());
    let has_cublas = lib_dirs
        .iter()
        .any(|d| Path::new(d).join("libcublas.so.12").exists());
    has_cudart && has_cublas
}

fn has_linux_vulkan_runtime() -> bool {
    [
        "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/lib/x86_64-linux-gnu/libvulkan.so.1",
        "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

/// Read-only scan of Ollama's local manifests/blobs directory. Never modifies
/// Ollama's files; verifies each referenced blob starts with `GGUF` magic.
pub(crate) fn scan_ollama_store_at(
    ollama_models_root: &Path,
    max_recommended_bytes: u64,
) -> (Vec<DiscoveredLocalModel>, bool) {
    let manifests_root = ollama_models_root.join("manifests");
    let blobs_root = ollama_models_root.join("blobs");
    if !manifests_root.is_dir() || !blobs_root.is_dir() {
        return (Vec::new(), false);
    }
    let mut found = Vec::new();
    scan_ollama_manifests_recursive(
        &manifests_root,
        &manifests_root,
        &blobs_root,
        max_recommended_bytes,
        MAX_SCAN_DEPTH + 1,
        &mut found,
    );
    (found, true)
}

fn scan_ollama_models_readonly(max_recommended_bytes: u64) -> (Vec<DiscoveredLocalModel>, bool) {
    let Some(home) = dirs::home_dir() else {
        return (Vec::new(), false);
    };
    scan_ollama_store_at(&home.join(".ollama").join("models"), max_recommended_bytes)
}

fn scan_ollama_manifests_recursive(
    root: &Path,
    dir: &Path,
    blobs_root: &Path,
    max_recommended_bytes: u64,
    depth: usize,
    out: &mut Vec<DiscoveredLocalModel>,
) {
    if depth == 0 || out.len() >= MAX_DISCOVERED_MODELS {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_DISCOVERED_MODELS {
            break;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            scan_ollama_manifests_recursive(
                root,
                &path,
                blobs_root,
                max_recommended_bytes,
                depth - 1,
                out,
            );
            continue;
        }
        if !meta.is_file() || meta.len() > 256 * 1024 {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(layers) = manifest.get("layers").and_then(|v| v.as_array()) else {
            continue;
        };
        let Some(model_layer) = layers.iter().find(|layer| {
            layer.get("mediaType").and_then(|v| v.as_str())
                == Some("application/vnd.ollama.image.model")
        }) else {
            continue;
        };
        let Some(digest) = model_layer.get("digest").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(hex_digest) = digest.strip_prefix("sha256:") else {
            continue;
        };
        if hex_digest.len() != 64 || !hex_digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let blob_path = blobs_root.join(format!("sha256-{hex_digest}"));
        let Ok(blob_meta) = fs::symlink_metadata(&blob_path) else {
            continue;
        };
        if blob_meta.file_type().is_symlink() || !blob_meta.is_file() {
            continue;
        }
        if !has_gguf_magic(&blob_path) {
            continue;
        }
        let rel = path.strip_prefix(root).ok();
        let name = rel
            .map(ollama_manifest_rel_to_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("ollama-{}", &hex_digest[..12]));
        let size_bytes = blob_meta.len();
        out.push(DiscoveredLocalModel {
            quantization: infer_quantization_from_name(&name),
            name,
            path: blob_path.display().to_string(),
            size_bytes,
            source: "ollama".to_string(),
            fits_hardware: max_recommended_bytes == 0 || size_bytes <= max_recommended_bytes,
        });
    }
}

fn ollama_manifest_rel_to_name(rel: &Path) -> String {
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        // registry.ollama.ai/library/<model>/<tag>
        [_, ns, model, tag] if ns == "library" => format!("{model}:{tag}"),
        [_, ns, model, tag] => format!("{ns}/{model}:{tag}"),
        [model, tag] => format!("{model}:{tag}"),
        _ => parts.join("/"),
    }
}

fn scan_lmstudio_models_readonly(max_recommended_bytes: u64) -> (Vec<DiscoveredLocalModel>, bool) {
    let Some(home) = dirs::home_dir() else {
        return (Vec::new(), false);
    };
    let candidates = [
        home.join(".lmstudio").join("models"),
        home.join(".cache").join("lm-studio").join("models"),
    ];
    let mut found = Vec::new();
    let mut any_dir = false;
    for dir in candidates {
        if dir.is_dir() {
            any_dir = true;
            scan_gguf_dir_recursive(
                &dir,
                "lmstudio",
                max_recommended_bytes,
                MAX_SCAN_DEPTH,
                &mut found,
            );
        }
    }
    (found, any_dir)
}

fn scan_user_model_dirs_readonly(
    extra_dirs: &[PathBuf],
    max_recommended_bytes: u64,
) -> Vec<DiscoveredLocalModel> {
    let mut dirs_to_scan = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs_to_scan.push(home.join("Models"));
    }
    if let Some(data) = dirs::data_local_dir() {
        dirs_to_scan.push(data.join("lucidd").join("models"));
        dirs_to_scan.push(data.join("lucid").join("models"));
    }
    dirs_to_scan.extend(extra_dirs.iter().cloned());

    let mut found = Vec::new();
    for dir in dirs_to_scan {
        if dir.is_dir() {
            scan_gguf_dir_recursive(
                &dir,
                "local",
                max_recommended_bytes,
                MAX_SCAN_DEPTH,
                &mut found,
            );
        }
    }
    found
}

fn scan_gguf_dir_recursive(
    dir: &Path,
    source: &str,
    max_recommended_bytes: u64,
    depth: usize,
    out: &mut Vec<DiscoveredLocalModel>,
) {
    if depth == 0 || out.len() >= MAX_DISCOVERED_MODELS {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_DISCOVERED_MODELS {
            break;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            // Skip internal content-addressed staging dirs to avoid duplicates.
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            scan_gguf_dir_recursive(&path, source, max_recommended_bytes, depth - 1, out);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("gguf") {
            continue;
        }
        if !has_gguf_magic(&path) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let size_bytes = meta.len();
        out.push(DiscoveredLocalModel {
            quantization: infer_quantization_from_name(&name),
            name,
            path: path.display().to_string(),
            size_bytes,
            source: source.to_string(),
            fits_hardware: max_recommended_bytes == 0 || size_bytes <= max_recommended_bytes,
        });
    }
}

/// Verify the first 4 bytes are `GGUF` so arbitrary files renamed `.gguf` are
/// ignored during discovery.
pub fn has_gguf_magic(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && &magic == GGUF_MAGIC
}

fn infer_quantization_from_name(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    for tag in [
        "Q4_K_M", "Q4_K_S", "Q5_K_M", "Q5_K_S", "Q6_K", "Q8_0", "Q4_0", "Q3_K_M", "Q2_K", "IQ4_XS",
        "F16", "BF16",
    ] {
        if upper.contains(tag) {
            return Some(tag.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn select_pinned_asset_covers_supported_platforms_with_valid_digests() {
        for asset in PINNED_LLAMA_ASSETS {
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(asset
                .download_url()
                .starts_with("https://github.com/ggml-org/llama.cpp/"));
        }
        assert_eq!(
            select_pinned_asset("macos", "aarch64", "metal")
                .unwrap()
                .platform_id,
            "macos-arm64-metal"
        );
        assert_eq!(
            select_pinned_asset("linux", "x86_64", "cuda")
                .unwrap()
                .platform_id,
            "linux-x86_64-cuda"
        );
        assert_eq!(
            select_pinned_asset("linux", "x86_64", "cpu")
                .unwrap()
                .platform_id,
            "linux-x86_64-cpu"
        );
        assert_eq!(
            select_pinned_asset("linux", "aarch64", "cpu")
                .unwrap()
                .platform_id,
            "linux-aarch64-cpu"
        );
    }

    #[test]
    fn validate_archive_entry_path_blocks_traversal_and_absolute_paths() {
        assert!(validate_archive_entry_path("build/bin/llama-server").is_ok());
        assert!(validate_archive_entry_path("../evil").is_err());
        assert!(validate_archive_entry_path("a/../../evil").is_err());
        assert!(validate_archive_entry_path("/etc/passwd").is_err());
    }

    #[test]
    fn scan_ollama_store_reads_manifests_and_verifies_gguf_magic() {
        let dir = TempDir::new().unwrap();
        let manifests = dir
            .path()
            .join("manifests/registry.ollama.ai/library/qwen2.5");
        let blobs = dir.path().join("blobs");
        fs::create_dir_all(&manifests).unwrap();
        fs::create_dir_all(&blobs).unwrap();

        let digest = "a".repeat(64);
        let blob_path = blobs.join(format!("sha256-{digest}"));
        fs::write(&blob_path, b"GGUF\x03\x00\x00\x00test-model-bytes").unwrap();

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{
                "mediaType": "application/vnd.ollama.image.model",
                "digest": format!("sha256:{digest}"),
                "size": 24
            }]
        });
        fs::write(manifests.join("7b-q4_k_m"), manifest.to_string()).unwrap();

        // Also create a non-GGUF file to verify magic filtering.
        let bad_digest = "b".repeat(64);
        fs::write(blobs.join(format!("sha256-{bad_digest}")), b"NOT_GGUF_DATA").unwrap();
        let bad_manifest = serde_json::json!({
            "layers": [{
                "mediaType": "application/vnd.ollama.image.model",
                "digest": format!("sha256:{bad_digest}")
            }]
        });
        fs::write(manifests.join("bad"), bad_manifest.to_string()).unwrap();

        let (models, store_found) = scan_ollama_store_at(dir.path(), 1024 * 1024);
        assert!(store_found);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "qwen2.5:7b-q4_k_m");
        assert_eq!(models[0].source, "ollama");
        assert_eq!(models[0].quantization.as_deref(), Some("Q4_K_M"));
        assert!(models[0].fits_hardware);
    }

    #[test]
    fn scan_gguf_dir_ignores_fake_extension_and_finds_real_gguf() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("real-Q8_0.gguf"),
            b"GGUF\x03\x00\x00\x00bytes",
        )
        .unwrap();
        fs::write(dir.path().join("fake.gguf"), b"hello world").unwrap();
        let mut found = Vec::new();
        scan_gguf_dir_recursive(dir.path(), "local", 1024, 2, &mut found);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "real-Q8_0");
        assert_eq!(found[0].quantization.as_deref(), Some("Q8_0"));
    }
}
