// SPDX-License-Identifier: Apache-2.0

//! Artifact storage and retrieval.
//!
//! Supports two on-disk layouts under the same `artifacts_dir` root:
//!
//! 1. **Channel/arch layout** — `<channel>/<arch>/<filename>`. The legacy
//!    Phase Boot layout, preserved byte-for-byte so existing tooling
//!    (phase-discover, phase-fetch, phase-verify, USB images) keeps working.
//! 2. **Content-addressed layout** — `blobs/<aa>/<full_hex>.bin` where
//!    `<full_hex>` is the lowercase hex SHA-256 of the blob contents and
//!    `<aa>` is the first two hex characters. Added in M6 so the server can
//!    distribute any blob keyed by its hash, independent of channel/arch
//!    semantics.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use tracing::warn;

/// Bounded buffer used while hashing and copying blobs. Blob installation is
/// intentionally independent of artifact size; no whole-file allocation is
/// required by the store.
const BLOB_COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Process-local uniqueness for staging files. create_new(true) remains the
/// authoritative collision guard, including against stale files after a crash.
static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

/// Content-address for a blob. The wire form is the lowercase hex
/// SHA-256 of the blob's contents — no `sha256:` prefix because the
/// algorithm is implicit in the path layout.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(String);

impl BlobId {
    /// Compute the [`BlobId`] of `content` by hashing.
    pub fn from_content(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        Self(hex::encode(hasher.finalize()))
    }

    /// Build a [`BlobId`] from an already-computed hex digest. Returns
    /// `None` if the input is not a 64-character lowercase hex string.
    pub fn from_hex(hex_str: &str) -> Option<Self> {
        if hex_str.len() == 64
            && hex_str
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Some(Self(hex_str.to_string()))
        } else {
            None
        }
    }

    /// View the blob id as its hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The two-character prefix used as the bucket directory name.
    pub fn prefix(&self) -> &str {
        &self.0[..2]
    }

    /// Relative path of this blob under the artifacts root:
    /// `blobs/<aa>/<full_hex>.bin`.
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from("blobs")
            .join(self.prefix())
            .join(format!("{}.bin", self.0))
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Metadata for an artifact on disk.
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// SHA-256 hash in the legacy `"sha256:<hexdigest>"` wire format used by
    /// the channel/arch path. Blob-id-keyed artifacts also populate this
    /// field for consistency.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobFileIdentity {
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
}

impl BlobFileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                len: metadata.len(),
                device: metadata.dev(),
                inode: metadata.ino(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                len: metadata.len(),
            }
        }
    }
}

/// Manages artifact storage and retrieval across both the channel/arch and
/// content-addressed layouts.
#[derive(Debug)]
pub struct ArtifactStore {
    base_dir: PathBuf,
    /// Cache of computed hashes for channel/arch-keyed lookups:
    /// `(channel, arch, name) -> "sha256:<hex>"`.
    hash_cache: RwLock<HashMap<(String, String, String), String>>,
}

impl ArtifactStore {
    /// Create a store rooted at `base_dir`. Creates the directory if needed.
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        if !base_dir.exists() {
            fs::create_dir_all(&base_dir)
                .with_context(|| format!("Failed to create artifacts dir: {:?}", base_dir))?;
        }

        Ok(Self {
            base_dir,
            hash_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Root directory of this store.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    // ------------------------------------------------------------------
    // Channel / arch layout (legacy, byte-identical to pre-M6)
    // ------------------------------------------------------------------

    /// Resolve the on-disk path for an artifact in the channel/arch layout.
    ///
    /// Honours arch aliases (`arm64` <-> `aarch64`, `amd64` <-> `x86_64`)
    /// and well-known filename alternatives (`kernel` -> `vmlinuz`,
    /// `bzImage`, etc.).
    pub fn get_artifact_path(&self, channel: &str, arch: &str, name: &str) -> Option<PathBuf> {
        if !Self::is_valid_name(channel) || !Self::is_valid_name(arch) || !Self::is_valid_name(name)
        {
            warn!(
                "Invalid artifact path components: {}/{}/{}",
                channel, arch, name
            );
            return None;
        }

        let arch_variants = Self::arch_aliases(arch);

        for arch_variant in &arch_variants {
            let path = self.base_dir.join(channel).join(arch_variant).join(name);
            if path.exists() && path.is_file() {
                return Some(path);
            }

            for alt in Self::artifact_alternatives(name) {
                let alt_path = self.base_dir.join(channel).join(arch_variant).join(&alt);
                if alt_path.exists() && alt_path.is_file() {
                    return Some(alt_path);
                }
            }
        }
        None
    }

    /// Look up an artifact in the channel/arch layout, returning metadata
    /// (size + cached hash). Returns `Ok(None)` when nothing exists at any
    /// of the path / alias / alternative combinations.
    pub fn get_artifact(
        &self,
        channel: &str,
        arch: &str,
        name: &str,
    ) -> Result<Option<ArtifactMeta>> {
        let path = match self.get_artifact_path(channel, arch, name) {
            Some(p) => p,
            None => return Ok(None),
        };

        let metadata =
            fs::metadata(&path).with_context(|| format!("Failed to read metadata: {:?}", path))?;

        let hash = self.get_or_compute_hash(channel, arch, name, &path)?;

        Ok(Some(ArtifactMeta {
            name: name.to_string(),
            path,
            size_bytes: metadata.len(),
            hash,
        }))
    }

    /// List every artifact present for a `(channel, arch)` pair. Honours
    /// arch aliases.
    pub fn list_artifacts(&self, channel: &str, arch: &str) -> Result<Vec<ArtifactMeta>> {
        for arch_variant in Self::arch_aliases(arch) {
            let dir = self.base_dir.join(channel).join(arch_variant);
            if dir.exists() {
                let mut artifacts = Vec::new();
                for entry in fs::read_dir(&dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let metadata = fs::metadata(&path)?;
                        let hash = self.get_or_compute_hash(channel, arch_variant, &name, &path)?;

                        artifacts.push(ArtifactMeta {
                            name,
                            path,
                            size_bytes: metadata.len(),
                            hash,
                        });
                    }
                }
                return Ok(artifacts);
            }
        }

        Ok(Vec::new())
    }

    /// List all channel directories under the base dir.
    pub fn list_channels(&self) -> Result<Vec<String>> {
        let mut channels = Vec::new();
        if self.base_dir.exists() {
            for entry in fs::read_dir(&self.base_dir)? {
                let entry = entry?;
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        // Skip the content-addressed bucket — it is not a channel.
                        if name == "blobs" {
                            continue;
                        }
                        channels.push(name.to_string());
                    }
                }
            }
        }
        Ok(channels)
    }

    /// Write `content` into the channel/arch layout at
    /// `<base>/<channel>/<arch>/<filename>`. Creates parent directories as
    /// needed.
    pub fn add_channel_artifact(
        &self,
        channel: &str,
        arch: &str,
        filename: &str,
        content: &[u8],
    ) -> Result<ArtifactMeta> {
        if !Self::is_valid_name(channel)
            || !Self::is_valid_name(arch)
            || !Self::is_valid_name(filename)
        {
            anyhow::bail!(
                "invalid channel/arch/filename: {}/{}/{}",
                channel,
                arch,
                filename
            );
        }
        let dir = self.base_dir.join(channel).join(arch);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create channel artifact dir {:?}", dir))?;
        let path = dir.join(filename);
        fs::write(&path, content).with_context(|| format!("write channel artifact {:?}", path))?;

        let hash = format!("sha256:{}", BlobId::from_content(content).as_str());
        if let Ok(mut cache) = self.hash_cache.write() {
            cache.insert(
                (channel.to_string(), arch.to_string(), filename.to_string()),
                hash.clone(),
            );
        }

        Ok(ArtifactMeta {
            name: filename.to_string(),
            path,
            size_bytes: content.len() as u64,
            hash,
        })
    }

    // ------------------------------------------------------------------
    // Content-addressed (blob-id) layout
    // ------------------------------------------------------------------

    /// Stable staging path for a resumable transfer of id.
    ///
    /// The path is below the same blobs tree as the final artifact, so commit
    /// never crosses filesystems. Call prepare_staging_path before opening it.
    /// A pull coordinator must serialize writers for one CID; final publication
    /// is independently idempotent and concurrent-safe.
    pub fn staging_path(&self, id: &BlobId) -> PathBuf {
        self.staging_dir_for(id)
            .join(format!("{}.part", id.as_str()))
    }

    /// Create the same-store staging directory and return the stable resumable
    /// path for id. Existing partial content is left untouched.
    pub fn prepare_staging_path(&self, id: &BlobId) -> Result<PathBuf> {
        let path = self.staging_path(id);
        let parent = path
            .parent()
            .context("blob staging path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("create blob staging directory {:?}", parent))?;
        Ok(path)
    }

    /// Write content into the blob layout under its verified SHA-256 path.
    /// Existing verified content is reused; conflicting content is rejected.
    pub fn add_blob(&self, content: &[u8]) -> Result<BlobId> {
        let id = BlobId::from_content(content);
        self.install_blob(content, &id, content.len() as u64)?;
        Ok(id)
    }

    /// Stream a blob into a unique same-store staging file, verify its exact
    /// size and SHA-256 CID, then publish it atomically.
    ///
    /// If the verified final blob exists, this returns its metadata without
    /// consuming reader. Concurrent installers converge on the same final path.
    /// Store-owned staging is removed on copy, verification, or publish failure.
    pub fn install_blob<R: Read>(
        &self,
        mut reader: R,
        expected_id: &BlobId,
        expected_size: u64,
    ) -> Result<ArtifactMeta> {
        if let Some(meta) = self.verify_existing_blob(expected_id, expected_size)? {
            return Ok(meta);
        }

        let (staged_path, mut staged_file) = self.create_unique_staging_file(expected_id)?;
        let copy_result = Self::copy_and_hash(&mut reader, &mut staged_file, expected_size)
            .and_then(|(actual_id, actual_size)| {
                Self::verify_blob_identity(expected_id, expected_size, &actual_id, actual_size)?;
                staged_file
                    .sync_all()
                    .with_context(|| format!("sync staged blob {:?}", staged_path))?;
                Ok(())
            });
        if let Err(error) = copy_result {
            drop(staged_file);
            Self::cleanup_staging_file(&staged_path);
            return Err(error);
        }
        let verified_identity = BlobFileIdentity::from_metadata(
            &staged_file
                .metadata()
                .with_context(|| format!("inspect verified staged blob {:?}", staged_path))?,
        );
        drop(staged_file);

        self.publish_verified_staging(&staged_path, expected_id, expected_size, &verified_identity)
    }

    /// Stream a source file through the verified installer. The source is
    /// never moved or removed; only the store-owned staging file is consumed.
    pub fn install_blob_from_path(
        &self,
        source: &Path,
        expected_id: &BlobId,
        expected_size: u64,
    ) -> Result<ArtifactMeta> {
        let source_file = fs::File::open(source)
            .with_context(|| format!("open blob source for import {:?}", source))?;
        self.install_blob(source_file, expected_id, expected_size)
    }

    /// Verify and atomically publish a completed resumable staging file.
    ///
    /// Only a regular file contained by this store's staging directory is
    /// accepted. A failed size/CID check removes the invalid partial. A path
    /// outside the staging directory is rejected without touching it.
    pub fn commit_staged_blob(
        &self,
        staged_path: &Path,
        expected_id: &BlobId,
        expected_size: u64,
    ) -> Result<ArtifactMeta> {
        self.ensure_store_owned_staging_file(staged_path)?;

        let verification = (|| -> Result<BlobFileIdentity> {
            let mut file = fs::File::open(staged_path)
                .with_context(|| format!("open staged blob {:?}", staged_path))?;
            let (actual_id, actual_size) = Self::hash_reader(&mut file)?;
            Self::verify_blob_identity(expected_id, expected_size, &actual_id, actual_size)?;
            file.sync_all()
                .with_context(|| format!("sync staged blob {:?}", staged_path))?;
            Ok(BlobFileIdentity::from_metadata(
                &file
                    .metadata()
                    .with_context(|| format!("inspect verified staged blob {:?}", staged_path))?,
            ))
        })();

        let verified_identity = match verification {
            Ok(identity) => identity,
            Err(error) => {
                Self::cleanup_staging_file(staged_path);
                return Err(error);
            }
        };

        self.publish_verified_staging(staged_path, expected_id, expected_size, &verified_identity)
    }

    /// On-disk path for a content-addressed blob.
    pub fn get_blob_path(&self, id: &BlobId) -> Option<PathBuf> {
        let path = self.base_dir.join(id.relative_path());
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
            Some(path)
        } else {
            None
        }
    }

    /// Metadata for a content-addressed blob.
    pub fn get_blob(&self, id: &BlobId) -> Result<Option<ArtifactMeta>> {
        let path = match self.get_blob_path(id) {
            Some(p) => p,
            None => return Ok(None),
        };
        let metadata =
            fs::metadata(&path).with_context(|| format!("Failed to read metadata: {:?}", path))?;
        Ok(Some(ArtifactMeta {
            name: format!("{}.bin", id.as_str()),
            path,
            size_bytes: metadata.len(),
            hash: format!("sha256:{}", id.as_str()),
        }))
    }

    // ------------------------------------------------------------------
    // Hashing helpers
    // ------------------------------------------------------------------

    /// Streaming SHA-256 of a file. Returns `"sha256:<hexdigest>"`.
    pub fn compute_hash(path: &Path) -> Result<String> {
        let file = fs::File::open(path)
            .with_context(|| format!("Failed to open file for hashing: {:?}", path))?;
        let (id, _) = Self::hash_reader(file)?;
        Ok(format!("sha256:{id}"))
    }

    /// Stream a file once to compute both its canonical CID and exact size.
    pub fn compute_blob_id(path: &Path) -> Result<(BlobId, u64)> {
        let file = fs::File::open(path)
            .with_context(|| format!("Failed to open file for hashing: {:?}", path))?;
        Self::hash_reader(file)
    }

    fn hash_reader<R: Read>(mut reader: R) -> Result<(BlobId, u64)> {
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; BLOB_COPY_BUFFER_BYTES];

        loop {
            let bytes_read = reader
                .read(&mut buffer)
                .context("read blob while hashing")?;
            if bytes_read == 0 {
                break;
            }
            total = total
                .checked_add(bytes_read as u64)
                .context("blob size overflow while hashing")?;
            hasher.update(&buffer[..bytes_read]);
        }

        Ok((BlobId(hex::encode(hasher.finalize())), total))
    }

    fn copy_and_hash<R: Read, W: Write>(
        mut reader: R,
        mut writer: W,
        expected_size: u64,
    ) -> Result<(BlobId, u64)> {
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; BLOB_COPY_BUFFER_BYTES];

        loop {
            let bytes_read = reader
                .read(&mut buffer)
                .context("read blob while staging")?;
            if bytes_read == 0 {
                break;
            }
            total = total
                .checked_add(bytes_read as u64)
                .context("blob size overflow while staging")?;
            if total > expected_size {
                anyhow::bail!(
                    "blob size exceeds expected size: expected {expected_size} bytes, read at least {total}"
                );
            }
            writer
                .write_all(&buffer[..bytes_read])
                .context("write staged blob")?;
            hasher.update(&buffer[..bytes_read]);
        }

        writer.flush().context("flush staged blob")?;
        Ok((BlobId(hex::encode(hasher.finalize())), total))
    }

    fn verify_blob_identity(
        expected_id: &BlobId,
        expected_size: u64,
        actual_id: &BlobId,
        actual_size: u64,
    ) -> Result<()> {
        if actual_size != expected_size {
            anyhow::bail!("blob size mismatch: expected {expected_size} bytes, got {actual_size}");
        }
        if actual_id != expected_id {
            anyhow::bail!(
                "blob CID mismatch: expected {}, got {}",
                expected_id,
                actual_id
            );
        }
        Ok(())
    }

    fn staging_root(&self) -> PathBuf {
        self.base_dir.join("blobs").join(".staging")
    }

    fn staging_dir_for(&self, id: &BlobId) -> PathBuf {
        self.staging_root().join(id.prefix())
    }

    fn create_unique_staging_file(&self, id: &BlobId) -> Result<(PathBuf, fs::File)> {
        let directory = self.staging_dir_for(id);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create blob staging directory {:?}", directory))?;

        for _ in 0..128 {
            let sequence = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let filename = format!("{}.{}.{}.part", id.as_str(), std::process::id(), sequence);
            let path = directory.join(filename);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create unique blob staging file {:?}", path));
                }
            }
        }

        anyhow::bail!("unable to allocate a unique staging file for blob {id}")
    }

    fn ensure_store_owned_staging_file(&self, staged_path: &Path) -> Result<()> {
        let file_type = fs::symlink_metadata(staged_path)
            .with_context(|| format!("inspect staged blob {:?}", staged_path))?
            .file_type();
        if !file_type.is_file() {
            anyhow::bail!("staged blob is not a regular file: {:?}", staged_path);
        }

        fs::create_dir_all(self.staging_root()).context("create blob staging root")?;
        let canonical_root =
            fs::canonicalize(self.staging_root()).context("canonicalize blob staging root")?;
        let canonical_path = fs::canonicalize(staged_path)
            .with_context(|| format!("canonicalize staged blob {:?}", staged_path))?;
        if !canonical_path.starts_with(&canonical_root) {
            anyhow::bail!(
                "staged blob path is outside this ArtifactStore: {:?}",
                staged_path
            );
        }
        Ok(())
    }

    fn verify_existing_blob(
        &self,
        expected_id: &BlobId,
        expected_size: u64,
    ) -> Result<Option<ArtifactMeta>> {
        let path = self.base_dir.join(expected_id.relative_path());
        if !path.exists() {
            return Ok(None);
        }
        let path_metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect existing blob path {:?}", path))?;
        if !path_metadata.file_type().is_file() {
            anyhow::bail!("blob destination is not a regular file: {:?}", path);
        }

        let metadata = path_metadata;
        if metadata.len() != expected_size {
            anyhow::bail!(
                "existing blob size mismatch for {}: expected {} bytes, got {}",
                expected_id,
                expected_size,
                metadata.len()
            );
        }

        let (actual_id, actual_size) = Self::compute_blob_id(&path)?;
        Self::verify_blob_identity(expected_id, expected_size, &actual_id, actual_size)?;
        Ok(Some(Self::blob_meta(expected_id, path, expected_size)))
    }

    fn publish_verified_staging(
        &self,
        staged_path: &Path,
        expected_id: &BlobId,
        expected_size: u64,
        verified_identity: &BlobFileIdentity,
    ) -> Result<ArtifactMeta> {
        let final_path = self.base_dir.join(expected_id.relative_path());
        let final_parent = final_path
            .parent()
            .context("blob final path has no parent directory")?;
        fs::create_dir_all(final_parent)
            .with_context(|| format!("create blob bucket directory {:?}", final_parent))?;

        match self.verify_existing_blob(expected_id, expected_size) {
            Ok(Some(meta)) => {
                Self::cleanup_staging_file(staged_path);
                return Ok(meta);
            }
            Ok(None) => {}
            Err(error) => {
                Self::cleanup_staging_file(staged_path);
                return Err(error);
            }
        }

        match fs::hard_link(staged_path, &final_path) {
            Ok(()) => {
                let post_publish = (|| -> Result<()> {
                    let published_metadata = fs::symlink_metadata(&final_path)
                        .with_context(|| format!("inspect published blob {:?}", final_path))?;
                    let published_identity = BlobFileIdentity::from_metadata(&published_metadata);
                    if !published_metadata.file_type().is_file()
                        || &published_identity != verified_identity
                        || published_metadata.len() != expected_size
                    {
                        anyhow::bail!(
                            "staged blob changed between verification and atomic publication"
                        );
                    }
                    let mut permissions = published_metadata.permissions();
                    permissions.set_readonly(true);
                    fs::set_permissions(&final_path, permissions).with_context(|| {
                        format!("make published blob read-only {:?}", final_path)
                    })?;
                    fs::File::open(&final_path)
                        .and_then(|file| file.sync_all())
                        .with_context(|| format!("sync published blob {:?}", final_path))?;
                    Self::sync_directory(final_parent)
                })();
                if let Err(error) = post_publish {
                    let _ = fs::remove_file(&final_path);
                    Self::cleanup_staging_file(staged_path);
                    return Err(error);
                }
                Self::cleanup_staging_file(staged_path);
                Ok(Self::blob_meta(expected_id, final_path, expected_size))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let result = self.verify_existing_blob(expected_id, expected_size);
                Self::cleanup_staging_file(staged_path);
                match result? {
                    Some(meta) => Ok(meta),
                    None => anyhow::bail!(
                        "blob destination appeared concurrently but is unavailable: {:?}",
                        final_path
                    ),
                }
            }
            Err(error) => {
                Self::cleanup_staging_file(staged_path);
                Err(error).with_context(|| {
                    format!(
                        "atomically publish staged blob {:?} as {:?}",
                        staged_path, final_path
                    )
                })
            }
        }
    }

    fn blob_meta(id: &BlobId, path: PathBuf, size_bytes: u64) -> ArtifactMeta {
        ArtifactMeta {
            name: format!("{}.bin", id.as_str()),
            path,
            size_bytes,
            hash: format!("sha256:{}", id.as_str()),
        }
    }

    fn cleanup_staging_file(path: &Path) {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(path = ?path, error = %error, "failed to remove blob staging file");
            }
        }
    }

    #[cfg(unix)]
    fn sync_directory(path: &Path) -> Result<()> {
        fs::File::open(path)
            .with_context(|| format!("open blob directory for sync {:?}", path))?
            .sync_all()
            .with_context(|| format!("sync blob directory {:?}", path))
    }

    #[cfg(not(unix))]
    fn sync_directory(_path: &Path) -> Result<()> {
        // std does not expose portable directory fsync here. The staged file
        // is synced and hard-link publication remains atomic where supported.
        Ok(())
    }

    fn get_or_compute_hash(
        &self,
        channel: &str,
        arch: &str,
        name: &str,
        path: &Path,
    ) -> Result<String> {
        let key = (channel.to_string(), arch.to_string(), name.to_string());

        if let Ok(cache) = self.hash_cache.read() {
            if let Some(hash) = cache.get(&key) {
                return Ok(hash.clone());
            }
        }

        let hash = Self::compute_hash(path)?;
        if let Ok(mut cache) = self.hash_cache.write() {
            cache.insert(key, hash.clone());
        }

        Ok(hash)
    }

    // ------------------------------------------------------------------
    // Path-component validation
    // ------------------------------------------------------------------

    fn is_valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.contains('\\')
            && !name.contains("..")
            && name != "."
    }

    fn artifact_alternatives(name: &str) -> Vec<String> {
        match name {
            "kernel" => vec![
                "vmlinuz".to_string(),
                "vmlinuz-arm64".to_string(),
                "bzImage".to_string(),
            ],
            "initramfs" => vec![
                "initramfs.img".to_string(),
                "initrd".to_string(),
                "initramfs-arm64.img".to_string(),
            ],
            "rootfs" => vec!["rootfs.img".to_string(), "rootfs.squashfs".to_string()],
            _ => vec![],
        }
    }

    fn arch_aliases(arch: &str) -> Vec<&str> {
        match arch {
            "aarch64" => vec!["aarch64", "arm64"],
            "arm64" => vec!["arm64", "aarch64"],
            "x86_64" => vec!["x86_64", "amd64"],
            "amd64" => vec!["amd64", "x86_64"],
            other => vec![other],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_store() -> (TempDir, ArtifactStore) {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf()).unwrap();
        (temp, store)
    }

    #[test]
    fn test_artifact_store_new() {
        let temp = TempDir::new().unwrap();
        let store = ArtifactStore::new(temp.path().to_path_buf());
        assert!(store.is_ok());
    }

    #[test]
    fn test_get_artifact_not_found() {
        let (_temp, store) = setup_test_store();
        let result = store.get_artifact("stable", "arm64", "kernel").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_artifact_found() {
        let (temp, store) = setup_test_store();

        let artifact_dir = temp.path().join("stable").join("arm64");
        fs::create_dir_all(&artifact_dir).unwrap();
        let kernel_path = artifact_dir.join("kernel");
        fs::write(&kernel_path, b"test kernel content").unwrap();

        let result = store.get_artifact("stable", "arm64", "kernel").unwrap();
        assert!(result.is_some());
        let meta = result.unwrap();
        assert_eq!(meta.name, "kernel");
        assert_eq!(meta.size_bytes, 19);
        assert!(meta.hash.starts_with("sha256:"));
    }

    #[test]
    fn test_path_traversal_prevention() {
        let (_temp, store) = setup_test_store();
        assert!(store
            .get_artifact_path("../etc", "passwd", "file")
            .is_none());
        assert!(store
            .get_artifact_path("stable", "arm64", "../../../etc/passwd")
            .is_none());
    }

    #[test]
    fn test_list_artifacts() {
        let (temp, store) = setup_test_store();

        let artifact_dir = temp.path().join("stable").join("arm64");
        fs::create_dir_all(&artifact_dir).unwrap();
        fs::write(artifact_dir.join("kernel"), b"kernel").unwrap();
        fs::write(artifact_dir.join("initramfs"), b"initramfs").unwrap();

        let artifacts = store.list_artifacts("stable", "arm64").unwrap();
        assert_eq!(artifacts.len(), 2);
    }

    #[test]
    fn test_compute_hash() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("test.bin");
        fs::write(&file_path, b"hello world").unwrap();

        let hash = ArtifactStore::compute_hash(&file_path).unwrap();
        assert!(hash.starts_with("sha256:"));
        assert_eq!(
            hash,
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    // ------------------------------------------------------------------
    // Blob-id layout
    // ------------------------------------------------------------------

    #[test]
    fn test_blob_id_from_content_is_sha256() {
        let id = BlobId::from_content(b"hello world");
        assert_eq!(
            id.as_str(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(id.prefix(), "b9");
    }

    #[test]
    fn test_blob_id_relative_path_layout() {
        let id = BlobId::from_content(b"hello world");
        let rel = id.relative_path();
        let rel_str = rel.to_string_lossy();
        assert!(rel_str.starts_with("blobs/b9/"));
        assert!(rel_str
            .ends_with("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9.bin"));
    }

    #[test]
    fn test_blob_id_from_hex_rejects_invalid() {
        assert!(BlobId::from_hex("not-hex").is_none());
        assert!(BlobId::from_hex("abcdef").is_none()); // too short
        let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(BlobId::from_hex(&digest.to_uppercase()).is_none());
        let valid = BlobId::from_hex(digest).unwrap();
        assert_eq!(valid.prefix(), "b9");
    }

    #[test]
    fn test_add_blob_and_get_blob_roundtrip() {
        let (temp, store) = setup_test_store();
        let id = store.add_blob(b"hello world").unwrap();
        assert_eq!(
            id.as_str(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        // File written under blobs/b9/<hex>.bin
        let blob_path = temp.path().join(id.relative_path());
        assert!(blob_path.exists());

        // Metadata roundtrip
        let meta = store.get_blob(&id).unwrap().unwrap();
        assert_eq!(meta.size_bytes, 11);
        assert_eq!(meta.hash, format!("sha256:{}", id.as_str()));
    }

    #[test]
    fn test_add_blob_is_idempotent_for_same_content() {
        let (_temp, store) = setup_test_store();
        let a = store.add_blob(b"same content").unwrap();
        let b = store.add_blob(b"same content").unwrap();
        assert_eq!(a, b);
    }

    struct ChunkedReader {
        bytes: std::io::Cursor<Vec<u8>>,
        max_chunk: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let limit = output.len().min(self.max_chunk);
            self.bytes.read(&mut output[..limit])
        }
    }

    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _output: &mut [u8]) -> std::io::Result<usize> {
            panic!("idempotent install unexpectedly consumed its reader");
        }
    }

    fn count_files_below(path: &Path) -> usize {
        if !path.exists() {
            return 0;
        }
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|entry| {
                if entry.is_dir() {
                    count_files_below(&entry)
                } else {
                    1
                }
            })
            .sum()
    }

    #[test]
    fn install_blob_streams_verifies_and_atomically_commits() {
        let (temp, store) = setup_test_store();
        let content: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        let expected_id = BlobId::from_content(&content);
        let reader = ChunkedReader {
            bytes: std::io::Cursor::new(content.clone()),
            max_chunk: 17,
        };

        let meta = store
            .install_blob(reader, &expected_id, content.len() as u64)
            .unwrap();

        assert_eq!(meta.path, temp.path().join(expected_id.relative_path()));
        assert_eq!(meta.size_bytes, content.len() as u64);
        assert_eq!(fs::read(&meta.path).unwrap(), content);
        assert_eq!(count_files_below(&store.staging_root()), 0);
    }

    #[test]
    fn install_blob_rejects_wrong_size_and_cleans_staging() {
        let (_temp, store) = setup_test_store();
        let content = b"size-checked-content";
        let expected_id = BlobId::from_content(content);

        let error = store
            .install_blob(content.as_slice(), &expected_id, content.len() as u64 - 1)
            .unwrap_err();

        assert!(error.to_string().contains("exceeds expected size"));
        assert!(store.get_blob_path(&expected_id).is_none());
        assert_eq!(count_files_below(&store.staging_root()), 0);
    }

    #[test]
    fn commit_staged_blob_rejects_wrong_cid_and_removes_partial() {
        let (_temp, store) = setup_test_store();
        let content = b"downloaded bytes";
        let expected_id = BlobId::from_content(b"different bytes");
        let staged_path = store.prepare_staging_path(&expected_id).unwrap();
        fs::write(&staged_path, content).unwrap();

        let error = store
            .commit_staged_blob(&staged_path, &expected_id, content.len() as u64)
            .unwrap_err();

        assert!(error.to_string().contains("CID mismatch"));
        assert!(!staged_path.exists());
        assert!(store.get_blob_path(&expected_id).is_none());
    }

    #[test]
    fn commit_staged_blob_publishes_verified_resumable_file() {
        let (_temp, store) = setup_test_store();
        let content = b"completed resumable download";
        let expected_id = BlobId::from_content(content);
        let staged_path = store.prepare_staging_path(&expected_id).unwrap();
        fs::write(&staged_path, content).unwrap();

        let meta = store
            .commit_staged_blob(&staged_path, &expected_id, content.len() as u64)
            .unwrap();

        assert!(!staged_path.exists());
        assert_eq!(fs::read(&meta.path).unwrap(), content);
        assert_eq!(meta.hash, format!("sha256:{expected_id}"));
        assert!(fs::metadata(&meta.path).unwrap().permissions().readonly());
    }

    #[cfg(unix)]
    #[test]
    fn content_addressed_lookup_and_install_reject_symlink_destinations() {
        use std::os::unix::fs::symlink;

        let (temp, store) = setup_test_store();
        let content = b"never follow a blob symlink";
        let expected_id = BlobId::from_content(content);
        let external = temp.path().join("external-content");
        fs::write(&external, content).unwrap();
        let final_path = temp.path().join(expected_id.relative_path());
        fs::create_dir_all(final_path.parent().unwrap()).unwrap();
        symlink(&external, &final_path).unwrap();

        assert!(store.get_blob(&expected_id).unwrap().is_none());
        let error = store
            .install_blob(content.as_slice(), &expected_id, content.len() as u64)
            .unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert_eq!(fs::read(&external).unwrap(), content);
    }

    #[test]
    fn commit_staged_blob_rejects_external_path_without_removing_it() {
        let (temp, store) = setup_test_store();
        let content = b"outside the store staging tree";
        let expected_id = BlobId::from_content(content);
        let external_path = temp.path().join("external.part");
        fs::write(&external_path, content).unwrap();

        let error = store
            .commit_staged_blob(&external_path, &expected_id, content.len() as u64)
            .unwrap_err();

        assert!(error.to_string().contains("outside this ArtifactStore"));
        assert!(external_path.exists());
        assert!(store.get_blob_path(&expected_id).is_none());
    }

    #[test]
    fn install_blob_existing_verified_content_is_idempotent_without_reading_again() {
        let (_temp, store) = setup_test_store();
        let content = b"already installed";
        let expected_id = store.add_blob(content).unwrap();

        let meta = store
            .install_blob(PanicReader, &expected_id, content.len() as u64)
            .unwrap();

        assert_eq!(meta.size_bytes, content.len() as u64);
        assert_eq!(meta.hash, format!("sha256:{expected_id}"));
    }

    #[test]
    fn install_blob_refuses_corrupt_existing_destination() {
        let (temp, store) = setup_test_store();
        let content = b"expected content";
        let corrupt = b"corrupt-content!";
        assert_eq!(content.len(), corrupt.len());
        let expected_id = BlobId::from_content(content);
        let final_path = temp.path().join(expected_id.relative_path());
        fs::create_dir_all(final_path.parent().unwrap()).unwrap();
        fs::write(&final_path, corrupt).unwrap();

        let error = store
            .install_blob(content.as_slice(), &expected_id, content.len() as u64)
            .unwrap_err();

        assert!(error.to_string().contains("CID mismatch"));
        assert_eq!(fs::read(final_path).unwrap(), corrupt);
        assert_eq!(count_files_below(&store.staging_root()), 0);
    }

    #[test]
    fn concurrent_installers_converge_without_staging_leaks() {
        let temp = TempDir::new().unwrap();
        let store = std::sync::Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
        let content =
            std::sync::Arc::new((0..100_000).map(|i| (i % 239) as u8).collect::<Vec<_>>());
        let expected_id = BlobId::from_content(&content);
        let expected_size = content.len() as u64;

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                let content = std::sync::Arc::clone(&content);
                let expected_id = expected_id.clone();
                std::thread::spawn(move || {
                    store
                        .install_blob(content.as_slice(), &expected_id, expected_size)
                        .unwrap()
                })
            })
            .collect();

        for thread in threads {
            let meta = thread.join().unwrap();
            assert_eq!(meta.size_bytes, expected_size);
        }
        assert_eq!(
            fs::read(store.base_dir().join(expected_id.relative_path())).unwrap(),
            *content
        );
        assert_eq!(count_files_below(&store.staging_root()), 0);
    }

    #[test]
    fn install_blob_from_path_streams_and_preserves_source() {
        let (temp, store) = setup_test_store();
        let source = temp.path().join("source.bin");
        let content = b"file import";
        fs::write(&source, content).unwrap();
        let expected_id = BlobId::from_content(content);

        let meta = store
            .install_blob_from_path(&source, &expected_id, content.len() as u64)
            .unwrap();

        assert_eq!(fs::read(&source).unwrap(), content);
        assert_eq!(fs::read(&meta.path).unwrap(), content);
    }

    #[test]
    fn test_list_channels_excludes_blobs_bucket() {
        let (temp, store) = setup_test_store();
        fs::create_dir_all(temp.path().join("stable").join("x86_64")).unwrap();
        fs::create_dir_all(temp.path().join("blobs").join("ab")).unwrap();
        let channels = store.list_channels().unwrap();
        assert!(channels.contains(&"stable".to_string()));
        assert!(!channels.contains(&"blobs".to_string()));
    }

    #[test]
    fn test_add_channel_artifact_writes_and_caches_hash() {
        let (temp, store) = setup_test_store();
        let meta = store
            .add_channel_artifact("stable", "x86_64", "kernel", b"kernel-bytes")
            .unwrap();
        assert_eq!(meta.size_bytes, 12);
        assert!(meta.hash.starts_with("sha256:"));
        // File should be at <base>/stable/x86_64/kernel
        let path = temp.path().join("stable").join("x86_64").join("kernel");
        assert!(path.exists());
    }
}
