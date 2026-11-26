// SPDX-License-Identifier: AGPL-3.0-or-later

//! LUCID M6 — Model registry.
//!
//! Tracks which models are loaded on **this** node, advertises them onto
//! the Phase DHT (so other peers can discover us as a serving node for
//! those models), and answers "who can serve model X?" queries by reading
//! the DHT back.
//!
//! ## Wire shape
//!
//! Each loaded model is announced as a Kademlia record:
//!
//! ```text
//! key   = b"phase/model/" || model_cid (32 bytes)   // 44 bytes total
//! value = postcard(SignedModelAdvertisement)
//! ```
//!
//! `SignedModelAdvertisement` carries the [`ModelCapabilities`], the
//! advertising peer's Ed25519 public key, and a signature over the
//! canonical form (`postcard(ad)` without the signature field). The schema
//! is tagged with [`ADVERTISEMENT_SCHEMA_VERSION`] so future shapes can
//! be added without breaking old advertisers.
//!
//! ## Trust model
//!
//! The DHT itself is untrusted — any peer can put any record under any
//! key. Trust comes from the Ed25519 signature: a reader resolves a
//! record, verifies the signature against the embedded pubkey, and
//! independently checks that the libp2p `PeerId` derives from that
//! pubkey. Records that fail signature or peer-id binding are discarded.
//!
//! ## Coarse advertisement
//!
//! `ModelCapabilities` describes the **model** — what's loaded, at what
//! quantization, the worker's self-reported parallelism budget. It does
//! **not** include latency, bandwidth, or live load: those live on
//! [`phase_net::PeerCapabilities`] and are gossiped (and bucketed) by
//! the phase-net layer, not duplicated here. See MISSION.md's
//! "gossip-not-telemetry" framing.
//!
//! ## TTL refresh
//!
//! Kademlia records expire — libp2p's default is 36h, but we publish
//! conservatively on a 5-minute cadence so a record never has more than
//! that much staleness for downstream lookups. On `withdraw` (or `Drop`)
//! the refresh task is cancelled. We do **not** publish an explicit
//! tombstone: the registry rebuilds itself on restart (in-memory only),
//! and other peers will let the record expire naturally. A future M-task
//! may add a signed-withdrawal record if "loaded model just vanished"
//! turns into a real UX problem.
//!
//! ## Persistence
//!
//! The set of loaded models is **in-memory**. What persists across
//! restarts is the node's Ed25519 identity (via `phase-identity`), so a
//! restarted node re-advertises under the same pubkey and accumulates
//! the same reputation / discovery linkage. The DHT layer takes care of
//! re-propagating advertisements when the peer comes back online. Alias
//! consumers may additionally opt into a private, atomically replaced replay
//! state file through [`ModelRegistry::new_with_alias_replay_state`].

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use phase_artifact_server::{ArtifactStore, BlobId};
use phase_identity::NodeIdentity;
use phase_net::PeerId;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Types — model identifiers, capability advertisement, and signed envelope.
// ---------------------------------------------------------------------------

/// Wire schema version for [`SignedModelAdvertisement`].
///
/// Bumped when fields are added/removed in a non-additive way **or when the
/// encoding changes** — because the signing payload is encoded and then
/// Ed25519-signed, the encoding is part of the canonical signed bytes.
///
/// Readers must check this before trusting the rest of the payload.
///
/// ## v2 — postcard encoding (SEC-12, RUSTSEC-2025-0141)
///
/// v1 encoded both the `SigningPayload` (signed canonical bytes) and the
/// wire envelope with `bincode 1.x`, which is now unmaintained. v2 switches
/// to `postcard`. Because the signed bytes change, a v1 node and a v2 node
/// would mis-verify each other's advertisements. The network is tiny (v0.1),
/// so this is a deliberate clean break: a v2 reader rejects v1 records on the
/// schema-version check before even reaching signature verification.
pub const ADVERTISEMENT_SCHEMA_VERSION: u32 = 2;

/// DHT key prefix for model advertisements. Final key shape:
/// `b"phase/model/" || model_cid` — exactly 12 + 32 = 44 bytes.
pub const MODEL_KEY_PREFIX: &[u8] = b"phase/model/";

/// Versioned DHT namespace for signed human alias records. The normalized
/// alias is hashed before it becomes a key so attacker-controlled names
/// cannot create oversized or path-shaped Kademlia keys.
pub const ALIAS_KEY_PREFIX: &[u8] = b"phase/model-alias/v1/";

/// Versioned DHT namespace for peers that can serve verified model bytes.
/// This is deliberately distinct from [`MODEL_KEY_PREFIX`]: possessing a
/// blob does not imply that the peer has loaded it for inference.
pub const CONTENT_PROVIDER_KEY_PREFIX: &[u8] = b"phase/content-provider/v1/";

/// Wire version for [`SignedAliasRecord`].
pub const ALIAS_SCHEMA_VERSION: u32 = 1;

/// Wire version for [`SignedContentProviderRecord`].
pub const CONTENT_PROVIDER_SCHEMA_VERSION: u32 = 1;

/// Alias records are deliberately short lived. Publishers may refresh them,
/// but a single signature cannot claim an alias indefinitely.
pub const MAX_ALIAS_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Content-provider claims are short lived and may be republished while the
/// content remains installed. Expiry prevents a one-time claim from becoming
/// a permanent assertion of availability.
pub const MAX_CONTENT_PROVIDER_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Upper bound carried by alias metadata. This is a protocol allocation cap,
/// not a promise that an operator has this much disk available.
pub const MAX_MODEL_SIZE_BYTES: u64 = 1 << 40;

/// How long between TTL refresh publishes. Kademlia's default record
/// lifetime is 36h, but we re-advertise on a much shorter cadence so a
/// reader sees freshness ≤ this interval. 5 minutes matches the design
/// brief and gives quick recovery after a transient outage.
pub const TTL_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Default advertisement lifetime baked into [`ModelCapabilities::valid_until`].
/// Set to `refresh_interval * 3` so a missed refresh doesn't immediately
/// invalidate the record from a consumer's perspective.
pub const ADVERTISEMENT_TTL: Duration = Duration::from_secs(15 * 60);

/// Opaque provider-discovery key. Catalog records themselves are isolated by
/// publisher identity, so publishers cannot overwrite a shared model list.
pub const CATALOG_SERVICE_KEY: &[u8] = b"phase/lucid/catalog/providers/v1";
pub const CATALOG_KEY_PREFIX: &[u8] = b"phase/lucid/catalog/publisher/v1/";
const CATALOG_SCHEMA_VERSION: u32 = 1;
const MAX_CATALOG_MODELS: usize = 64;
const MAX_CATALOG_PUBLISHERS: usize = 16;
const MAX_CATALOG_BYTES: usize = 64 * 1024;
const MAX_CATALOG_TOTAL_BYTES: usize = 1024 * 1024;
const CATALOG_TTL: Duration = Duration::from_secs(5 * 60);
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const CATALOG_QUERY_TIMEOUT: Duration = Duration::from_secs(8);

/// Maximum clock skew accepted from a signed model advertiser. A peer may be
/// slightly ahead, but it cannot pre-sign records arbitrarily far in advance.
pub const MAX_ADVERTISEMENT_FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);

/// Defensive decode limits for one model-CID lookup. `DhtTransport` already
/// owns the returned buffers, so these limits bound validation CPU and any
/// allocations performed by postcard at this layer; transports must also
/// enforce their own network/input limits before constructing the `Vec`.
pub const MAX_ADVERTISEMENT_RECORDS_PER_QUERY: usize = 256;
pub const MAX_ADVERTISEMENT_DECODE_BYTES_PER_QUERY: usize = 1024 * 1024;
pub const MAX_ADVERTISEMENT_RECORD_BYTES: usize = 16 * 1024;

/// Defensive input and replay-state limits for signed alias resolution.
pub const MAX_ALIAS_RECORDS_PER_QUERY: usize = 256;
pub const MAX_ALIAS_DECODE_BYTES_PER_QUERY: usize = 256 * 1024;
pub const MAX_ALIAS_RECORD_BYTES: usize = 4 * 1024;
pub const MAX_TRACKED_ALIAS_RECORDS: usize = 16_384;

/// Bounded durable alias replay-state format. The file contains only the
/// highest accepted sequence and its exact payload fingerprint per
/// `(normalized alias, publisher key)` pair.
pub const ALIAS_REPLAY_STATE_SCHEMA_VERSION: u32 = 1;
pub const MAX_ALIAS_REPLAY_STATE_BYTES: u64 = 8 * 1024 * 1024;

/// Defensive input limits for one content-provider lookup.
pub const MAX_CONTENT_PROVIDER_RECORDS_PER_QUERY: usize = 256;
pub const MAX_CONTENT_PROVIDER_DECODE_BYTES_PER_QUERY: usize = 256 * 1024;
pub const MAX_CONTENT_PROVIDER_RECORD_BYTES: usize = 4 * 1024;
pub const MAX_TRACKED_CONTENT_PROVIDER_RECORDS: usize = 16_384;

/// Hard cap on each class of local content refresh task. Alias and provider
/// tasks have separate maps because installation and serving are independent
/// capabilities, but neither map may grow without bound.
pub const MAX_LOCAL_CONTENT_REFRESH_TASKS: usize = 1_024;

const MAX_CAPABILITY_LABEL_BYTES: usize = 32;

/// Content identifier for a model — the SHA-256 of the underlying weight
/// file (e.g. the GGUF blob). 32 bytes; same hash space the rest of Phase
/// uses for manifest hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelCid(pub [u8; 32]);

impl ModelCid {
    /// Hex-encode for log output and DHT-key debugging.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the canonical lowercase-or-uppercase 64-hex SHA-256 form.
    /// Algorithm prefixes and truncated digests are rejected so a CID has
    /// exactly one binary representation on the wire.
    pub fn from_hex(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("model CID must be exactly 64 hexadecimal characters");
        }
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&value[offset..offset + 2], 16)
                .context("decode model CID hex")?;
        }
        Ok(Self(bytes))
    }

    /// Render the DHT key for this CID:
    /// `b"phase/model/" || cid_bytes` (44 bytes).
    pub fn dht_key(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MODEL_KEY_PREFIX.len() + self.0.len());
        out.extend_from_slice(MODEL_KEY_PREFIX);
        out.extend_from_slice(&self.0);
        out
    }

    /// Development-only deterministic name hash for the explicit EchoWorker.
    ///
    /// This is not a content identifier and is never accepted by production
    /// alias resolution, pulls, llama.cpp, or MLX. It exists only so the
    /// explicitly selected GPU-less echo fixture can participate in relay
    /// integration tests without a model artifact.
    ///
    /// Uses SHA-256 with the domain-separation prefix
    /// `b"phase/model-id-v1:"` so it is stable across Rust versions and
    /// architectures (`DefaultHasher` is not).
    #[doc(hidden)]
    pub fn development_name_hash(model_id: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"phase/model-id-v1:");
        hasher.update(model_id.as_bytes());
        let out = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&out);
        Self(bytes)
    }
}

/// Normalize and validate the human-facing model alias used by the signed
/// name index. v1 intentionally accepts only a conservative ASCII subset;
/// Unicode lookalikes, whitespace, path separators, and shell-shaped leading
/// flags are rejected instead of being normalized ambiguously.
pub fn normalize_model_alias(alias: &str) -> Result<String> {
    if alias.is_empty() || alias.len() > 128 {
        bail!("model alias length must be between 1 and 128 bytes");
    }
    if !alias.is_ascii() {
        bail!("model alias must contain ASCII characters only");
    }
    let normalized = alias.to_ascii_lowercase();
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        bail!("model alias is empty");
    };
    if !first.is_ascii_alphanumeric() {
        bail!("model alias must start with an ASCII letter or digit");
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':')) {
        bail!("model alias contains an unsupported character");
    }
    if normalized.contains("..") {
        bail!("model alias cannot contain '..'");
    }
    Ok(normalized)
}

/// DHT key for a normalized alias. The full signed record repeats the alias,
/// and readers verify that it hashes to the queried key.
pub fn alias_dht_key(alias: &str) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let normalized = normalize_model_alias(alias)?;
    let mut hasher = Sha256::new();
    hasher.update(b"phase:model-alias-key:v1\0");
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut key = Vec::with_capacity(ALIAS_KEY_PREFIX.len() + digest.len());
    key.extend_from_slice(ALIAS_KEY_PREFIX);
    key.extend_from_slice(&digest);
    Ok(key)
}

/// DHT key for providers of one exact immutable content CID.
pub fn content_provider_dht_key(model_cid: &ModelCid) -> Vec<u8> {
    let mut key = Vec::with_capacity(CONTENT_PROVIDER_KEY_PREFIX.len() + model_cid.0.len());
    key.extend_from_slice(CONTENT_PROVIDER_KEY_PREFIX);
    key.extend_from_slice(&model_cid.0);
    key
}

/// Publisher-signed human alias mapping. A mapping identifies immutable
/// content plus the minimum metadata needed to select a compatible backend.
/// It does not claim that the publisher is globally authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRecord {
    pub alias: String,
    pub model_cid: ModelCid,
    pub format: String,
    pub size_bytes: u64,
    pub sequence: u64,
    pub issued_at: u64,
    pub valid_until: u64,
}

impl AliasRecord {
    pub fn new(
        alias: &str,
        model_cid: ModelCid,
        format: impl Into<String>,
        size_bytes: u64,
        sequence: u64,
    ) -> Result<Self> {
        let issued_at = unix_ms_now();
        Ok(Self {
            alias: normalize_model_alias(alias)?,
            model_cid,
            format: format.into(),
            size_bytes,
            sequence,
            issued_at,
            valid_until: issued_at + MAX_ALIAS_TTL.as_millis() as u64,
        })
    }

    fn validate_at(&self, now_ms: u64) -> Result<()> {
        if normalize_model_alias(&self.alias)? != self.alias {
            bail!("alias record is not in canonical normalized form");
        }
        if self.format.is_empty()
            || self.format.len() > 32
            || !self
                .format
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            bail!("alias record has invalid format metadata");
        }
        if self.size_bytes == 0 || self.size_bytes > MAX_MODEL_SIZE_BYTES {
            bail!("alias record size is outside the supported range");
        }
        if self.sequence == 0 {
            bail!("alias record sequence must be non-zero");
        }
        if self.valid_until <= self.issued_at
            || self.valid_until - self.issued_at > MAX_ALIAS_TTL.as_millis() as u64
        {
            bail!("alias record validity window is invalid");
        }
        if self.issued_at > now_ms.saturating_add(Duration::from_secs(5 * 60).as_millis() as u64) {
            bail!("alias record was issued too far in the future");
        }
        if now_ms >= self.valid_until {
            bail!("alias record has expired");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAliasRecord {
    pub schema_version: u32,
    pub record: AliasRecord,
    pub publisher_pubkey: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct AliasSigningPayload<'a> {
    schema_version: u32,
    record: &'a AliasRecord,
    publisher_pubkey: [u8; 32],
}

impl SignedAliasRecord {
    fn canonical_signed_bytes(
        schema_version: u32,
        record: &AliasRecord,
        publisher_pubkey: [u8; 32],
    ) -> Result<Vec<u8>> {
        postcard::to_allocvec(&AliasSigningPayload {
            schema_version,
            record,
            publisher_pubkey,
        })
        .context("serialize alias signing payload")
    }

    pub fn sign(record: AliasRecord, identity: &NodeIdentity) -> Result<Self> {
        record.validate_at(unix_ms_now())?;
        let publisher_pubkey = identity.verifying_key().to_bytes();
        let bytes = Self::canonical_signed_bytes(ALIAS_SCHEMA_VERSION, &record, publisher_pubkey)?;
        Ok(Self {
            schema_version: ALIAS_SCHEMA_VERSION,
            record,
            publisher_pubkey,
            signature: identity.signing_key().sign(&bytes).to_bytes().to_vec(),
        })
    }

    pub fn verify_at(&self, now_ms: u64) -> Result<()> {
        if self.schema_version != ALIAS_SCHEMA_VERSION {
            bail!("unsupported alias schema version: {}", self.schema_version);
        }
        self.record.validate_at(now_ms)?;
        let sig_bytes: &[u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("alias signature has wrong length"))?;
        let bytes =
            Self::canonical_signed_bytes(self.schema_version, &self.record, self.publisher_pubkey)?;
        let key = VerifyingKey::from_bytes(&self.publisher_pubkey)
            .context("decode alias publisher key")?;
        key.verify(&bytes, &Signature::from_bytes(sig_bytes))
            .map_err(|e| anyhow!("alias signature failed to verify: {e}"))
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_allocvec(self).context("serialize signed alias record")?;
        if bytes.len() > MAX_ALIAS_RECORD_BYTES {
            bail!(
                "signed alias record exceeds the {}-byte record limit",
                MAX_ALIAS_RECORD_BYTES
            );
        }
        Ok(bytes)
    }

    pub fn decode_at(bytes: &[u8], now_ms: u64) -> Result<Self> {
        if bytes.len() > MAX_ALIAS_RECORD_BYTES {
            bail!(
                "signed alias record exceeds the {}-byte record limit",
                MAX_ALIAS_RECORD_BYTES
            );
        }
        let (record, remaining): (Self, &[u8]) =
            postcard::take_from_bytes(bytes).context("decode signed alias record")?;
        if !remaining.is_empty() {
            bail!("signed alias record contains trailing bytes");
        }
        record.verify_at(now_ms)?;
        Ok(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAlias {
    pub record: AliasRecord,
    pub publisher: PeerId,
}

type AliasSequenceKey = (String, [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcceptedAliasRecord {
    sequence: u64,
    fingerprint: [u8; 32],
}

type AcceptedAliasRecords = Arc<RwLock<HashMap<AliasSequenceKey, AcceptedAliasRecord>>>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasReplayStateFile {
    schema_version: u32,
    entries: Vec<AliasReplayStateEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasReplayStateEntry {
    alias: String,
    publisher_pubkey: String,
    sequence: u64,
    fingerprint: String,
}

/// Signed assertion that a peer can serve the exact verified bytes identified
/// by `model_cid`. It is a content-plane capability only; it says nothing
/// about whether the peer can execute inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentProviderRecord {
    pub model_cid: ModelCid,
    pub size_bytes: u64,
    pub format: String,
    pub provider_peer_id: String,
    pub sequence: u64,
    pub issued_at: u64,
    pub valid_until: u64,
}

impl ContentProviderRecord {
    pub fn new(
        model_cid: ModelCid,
        size_bytes: u64,
        format: impl Into<String>,
        provider: PeerId,
        sequence: u64,
    ) -> Result<Self> {
        let issued_at = unix_ms_now();
        let record = Self {
            model_cid,
            size_bytes,
            format: format.into(),
            provider_peer_id: provider.to_string(),
            sequence,
            issued_at,
            valid_until: issued_at + MAX_CONTENT_PROVIDER_TTL.as_millis() as u64,
        };
        record.validate_at(issued_at)?;
        Ok(record)
    }

    fn validate_at(&self, now_ms: u64) -> Result<PeerId> {
        if self.model_cid.0.iter().all(|byte| *byte == 0) {
            bail!("content-provider CID cannot be all zeroes");
        }
        if self.size_bytes == 0 || self.size_bytes > MAX_MODEL_SIZE_BYTES {
            bail!("content-provider size is outside the supported range");
        }
        validate_capability_token(&self.format, "content format")?;
        if self.sequence == 0 {
            bail!("content-provider sequence must be non-zero");
        }
        if self.issued_at == 0
            || self.valid_until <= self.issued_at
            || self.valid_until - self.issued_at > MAX_CONTENT_PROVIDER_TTL.as_millis() as u64
        {
            bail!("content-provider validity window is invalid");
        }
        if self.issued_at > now_ms.saturating_add(MAX_ADVERTISEMENT_FUTURE_SKEW.as_millis() as u64)
        {
            bail!("content-provider record was issued too far in the future");
        }
        if now_ms >= self.valid_until {
            bail!("content-provider record has expired");
        }
        let provider: PeerId = self
            .provider_peer_id
            .parse()
            .context("decode content-provider PeerId")?;
        if provider.to_string() != self.provider_peer_id {
            bail!("content-provider PeerId is not in canonical form");
        }
        Ok(provider)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedContentProviderRecord {
    pub schema_version: u32,
    pub record: ContentProviderRecord,
    pub provider_pubkey: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct ContentProviderSigningPayload<'a> {
    schema_version: u32,
    record: &'a ContentProviderRecord,
    provider_pubkey: [u8; 32],
}

impl SignedContentProviderRecord {
    fn canonical_signed_bytes(
        schema_version: u32,
        record: &ContentProviderRecord,
        provider_pubkey: [u8; 32],
    ) -> Result<Vec<u8>> {
        postcard::to_allocvec(&ContentProviderSigningPayload {
            schema_version,
            record,
            provider_pubkey,
        })
        .context("serialize content-provider signing payload")
    }

    pub fn sign(record: ContentProviderRecord, identity: &NodeIdentity) -> Result<Self> {
        let claimed_provider = record.validate_at(unix_ms_now())?;
        let provider_pubkey = identity.verifying_key().to_bytes();
        let derived_provider = peer_id_from_ed25519_pubkey(&provider_pubkey)?;
        if claimed_provider != derived_provider {
            bail!("content-provider PeerId does not match signing key");
        }
        let bytes = Self::canonical_signed_bytes(
            CONTENT_PROVIDER_SCHEMA_VERSION,
            &record,
            provider_pubkey,
        )?;
        Ok(Self {
            schema_version: CONTENT_PROVIDER_SCHEMA_VERSION,
            record,
            provider_pubkey,
            signature: identity.signing_key().sign(&bytes).to_bytes().to_vec(),
        })
    }

    pub fn verify_at(&self, now_ms: u64) -> Result<PeerId> {
        if self.schema_version != CONTENT_PROVIDER_SCHEMA_VERSION {
            bail!(
                "unsupported content-provider schema version: {}",
                self.schema_version
            );
        }
        let claimed_provider = self.record.validate_at(now_ms)?;
        let signature_bytes: &[u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("content-provider signature has wrong length"))?;
        let bytes =
            Self::canonical_signed_bytes(self.schema_version, &self.record, self.provider_pubkey)?;
        let key = VerifyingKey::from_bytes(&self.provider_pubkey)
            .context("decode content-provider public key")?;
        key.verify(&bytes, &Signature::from_bytes(signature_bytes))
            .map_err(|error| anyhow!("content-provider signature failed to verify: {error}"))?;
        let derived_provider = peer_id_from_ed25519_pubkey(&self.provider_pubkey)?;
        if claimed_provider != derived_provider {
            bail!("content-provider PeerId does not match signing key");
        }
        Ok(derived_provider)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_allocvec(self).context("serialize content-provider record")?;
        if bytes.len() > MAX_CONTENT_PROVIDER_RECORD_BYTES {
            bail!(
                "signed content-provider record exceeds the {}-byte record limit",
                MAX_CONTENT_PROVIDER_RECORD_BYTES
            );
        }
        Ok(bytes)
    }

    pub fn decode_at(bytes: &[u8], now_ms: u64) -> Result<Self> {
        if bytes.len() > MAX_CONTENT_PROVIDER_RECORD_BYTES {
            bail!(
                "signed content-provider record exceeds the {}-byte record limit",
                MAX_CONTENT_PROVIDER_RECORD_BYTES
            );
        }
        let (record, remaining): (Self, &[u8]) =
            postcard::take_from_bytes(bytes).context("decode content-provider record")?;
        if !remaining.is_empty() {
            bail!("signed content-provider record contains trailing bytes");
        }
        record.verify_at(now_ms)?;
        Ok(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentProviderCandidate {
    pub provider: PeerId,
    pub record: ContentProviderRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub model_id: String,
    pub model_cid: ModelCid,
    pub format: String,
    pub size_bytes: u64,
    pub installed_at: u64,
}

#[derive(Debug, Clone, Copy)]
struct AcceptedContentProviderRecord {
    sequence: u64,
    fingerprint: [u8; 32],
}

type ContentProviderSequenceKey = (ModelCid, [u8; 32]);
type AcceptedContentProviderRecords =
    Arc<RwLock<HashMap<ContentProviderSequenceKey, AcceptedContentProviderRecord>>>;

/// What a peer claims about a single loaded model.
///
/// Coarse, model-shaped only — see the module docstring on the
/// "gossip-not-telemetry" boundary against `PeerCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Human-readable model identifier, e.g. `"qwen3-next-80b-q4"`. Used
    /// by router code to translate Ollama's `model` field on a chat
    /// request into a [`ModelCid`] for DHT lookup.
    pub model_id: String,

    /// Content identifier for the actual weights blob (SHA-256 of GGUF).
    pub model_cid: ModelCid,

    /// Quantization/format label as the worker reports it, e.g. `"Q4_K_M"`,
    /// `"Q8_0"`, `"F16"`. The signed-advertisement gate accepts a bounded
    /// ASCII protocol token and otherwise preserves its conventional case.
    pub quantization: String,

    /// Maximum context window the loaded model supports, in tokens.
    pub context_length: u32,

    /// Worker's self-reported maximum concurrent inference requests.
    pub max_concurrent: u32,

    /// Backend that loaded the model, e.g. `"llama.cpp"` / `"mlx"`.
    pub backend: String,

    /// Unix millisecond timestamp the advertisement was produced.
    pub advertised_at: u64,

    /// Unix millisecond timestamp after which this advertisement should
    /// be treated as stale. Default is `advertised_at + ADVERTISEMENT_TTL`.
    pub valid_until: u64,
}

impl ModelCapabilities {
    /// Build an advertisement with `advertised_at = now` and
    /// `valid_until = now + ADVERTISEMENT_TTL`. Callers can override
    /// either field before passing to `advertise_loaded`.
    pub fn now(
        model_id: impl Into<String>,
        model_cid: ModelCid,
        quantization: impl Into<String>,
        context_length: u32,
        max_concurrent: u32,
        backend: impl Into<String>,
    ) -> Self {
        let now = unix_ms_now();
        Self {
            model_id: model_id.into(),
            model_cid,
            quantization: quantization.into(),
            context_length,
            max_concurrent,
            backend: backend.into(),
            advertised_at: now,
            valid_until: now + ADVERTISEMENT_TTL.as_millis() as u64,
        }
    }

    fn validate_at(&self, now_ms: u64) -> Result<()> {
        if self.model_cid.0.iter().all(|byte| *byte == 0) {
            bail!("model advertisement CID cannot be all zeroes");
        }

        let normalized_model_id = normalize_model_alias(&self.model_id)
            .context("validate model advertisement model_id")?;
        if normalized_model_id != self.model_id {
            bail!("model advertisement model_id is not in canonical normalized form");
        }

        validate_capability_token(&self.quantization, "quantization/format")?;
        validate_capability_token(&self.backend, "backend")?;
        if self.backend.to_ascii_lowercase() != self.backend {
            bail!("model advertisement backend is not in canonical lowercase form");
        }

        if self.context_length == 0 {
            bail!("model advertisement context length must be non-zero");
        }
        if self.max_concurrent == 0 {
            bail!("model advertisement concurrency capacity must be non-zero");
        }

        if self.advertised_at == 0 || self.valid_until <= self.advertised_at {
            bail!("model advertisement validity window is invalid");
        }
        let ttl_ms = self.valid_until - self.advertised_at;
        if ttl_ms > ADVERTISEMENT_TTL.as_millis() as u64 {
            bail!("model advertisement TTL exceeds the protocol maximum");
        }
        if self.advertised_at
            > now_ms.saturating_add(MAX_ADVERTISEMENT_FUTURE_SKEW.as_millis() as u64)
        {
            bail!("model advertisement was issued too far in the future");
        }
        if now_ms >= self.valid_until {
            bail!("model advertisement has expired");
        }
        Ok(())
    }
}

fn validate_capability_token(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_CAPABILITY_LABEL_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("model advertisement has invalid {field} metadata");
    }
    Ok(())
}

/// Signed envelope around [`ModelCapabilities`]. This is what actually
/// goes onto the wire as the DHT record value.
///
/// Layout (postcard):
/// ```text
/// schema_version: u32
/// caps:           ModelCapabilities
/// pubkey:         [u8; 32]    // Ed25519 verifying key
/// signature:      [u8; 64]    // signature over the canonical form
/// ```
///
/// The "canonical form" signed is postcard-encoded `SigningPayload`
/// (everything except `signature`). This means tampering with **any**
/// field — including `pubkey` — invalidates the signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedModelAdvertisement {
    /// See [`ADVERTISEMENT_SCHEMA_VERSION`].
    pub schema_version: u32,

    /// The advertisement payload.
    pub caps: ModelCapabilities,

    /// Advertiser's Ed25519 public key. The reader independently checks
    /// that the libp2p `PeerId` it learned this record from derives from
    /// this same key — otherwise an attacker could replay an old, valid
    /// advertisement under a different peer-id and look like a serving
    /// node when they aren't.
    pub pubkey: [u8; 32],

    /// Detached Ed25519 signature over `postcard(SigningPayload { .. })`.
    /// Stored as `Vec<u8>` rather than `[u8; 64]` only because serde's
    /// stable surface ships `Deserialize` impls for arrays up to length
    /// 32; a 64-byte array would otherwise need `serde_big_array`. The
    /// length is checked at verify time — anything other than 64 bytes
    /// is rejected.
    pub signature: Vec<u8>,
}

/// Internal helper: the exact byte sequence covered by the signature.
/// Kept private so the only way to compute it is through
/// `SignedModelAdvertisement::canonical_signed_bytes()`.
#[derive(Debug, Serialize)]
struct SigningPayload<'a> {
    schema_version: u32,
    caps: &'a ModelCapabilities,
    pubkey: [u8; 32],
}

impl SignedModelAdvertisement {
    /// Produce the canonical bytes covered by `signature`. Must be
    /// identical on signer and verifier — that's why it's a single
    /// helper rather than open-coded at each site.
    fn canonical_signed_bytes(
        schema_version: u32,
        caps: &ModelCapabilities,
        pubkey: [u8; 32],
    ) -> Result<Vec<u8>> {
        postcard::to_allocvec(&SigningPayload {
            schema_version,
            caps,
            pubkey,
        })
        .context("serialize SigningPayload for advertisement")
    }

    /// Sign a fresh advertisement with the given identity.
    pub fn sign(caps: ModelCapabilities, identity: &NodeIdentity) -> Result<Self> {
        caps.validate_at(unix_ms_now())?;
        let pubkey = identity.verifying_key().to_bytes();
        let bytes = Self::canonical_signed_bytes(ADVERTISEMENT_SCHEMA_VERSION, &caps, pubkey)?;
        let signature = identity.signing_key().sign(&bytes).to_bytes().to_vec();
        Ok(Self {
            schema_version: ADVERTISEMENT_SCHEMA_VERSION,
            caps,
            pubkey,
            signature,
        })
    }

    /// Verify the signature over `caps` + `pubkey` + `schema_version`, then
    /// enforce the capability and timestamp invariants at the current time.
    /// Does **not** check that `pubkey` matches a libp2p `PeerId` — callers
    /// that consume records from the DHT must do that independently (we don't
    /// always have the `PeerId` at the point of verification, e.g. inside a
    /// unit test).
    pub fn verify(&self) -> Result<()> {
        self.verify_at(unix_ms_now())
    }

    /// Verify the exact wire schema, signature, and advertisement semantics at
    /// a caller-supplied time. The explicit time makes boundary tests
    /// deterministic and ensures all records in one DHT lookup share a clock
    /// snapshot.
    pub fn verify_at(&self, now_ms: u64) -> Result<()> {
        if self.schema_version != ADVERTISEMENT_SCHEMA_VERSION {
            bail!(
                "unsupported advertisement schema version: {}",
                self.schema_version
            );
        }
        // Ed25519 signatures are exactly 64 bytes. Anything else is a
        // protocol violation — fail before we hand garbage to the
        // signature library.
        let sig_bytes: &[u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("advertisement signature has wrong length"))?;
        let bytes = Self::canonical_signed_bytes(self.schema_version, &self.caps, self.pubkey)?;
        let vk = VerifyingKey::from_bytes(&self.pubkey).context("decode advertisement pubkey")?;
        let sig = Signature::from_bytes(sig_bytes);
        vk.verify(&bytes, &sig)
            .map_err(|e| anyhow!("advertisement signature failed to verify: {e}"))?;
        self.caps.validate_at(now_ms)?;
        Ok(())
    }

    /// Postcard-encode the full signed envelope for DHT publication.
    pub fn encode(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(self).context("serialize SignedModelAdvertisement")
    }

    /// Decode + verify in one step. Returns the inner advertisement only
    /// after the signature and current semantic checks succeed.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::decode_at(bytes, unix_ms_now())
    }

    /// Decode and verify at a caller-supplied time. The input must contain
    /// exactly one schema-v2 envelope: postcard trailing bytes are rejected.
    pub fn decode_at(bytes: &[u8], now_ms: u64) -> Result<Self> {
        if bytes.len() > MAX_ADVERTISEMENT_RECORD_BYTES {
            bail!(
                "signed advertisement exceeds the {}-byte record limit",
                MAX_ADVERTISEMENT_RECORD_BYTES
            );
        }
        let (ad, remaining): (SignedModelAdvertisement, &[u8]) =
            postcard::take_from_bytes(bytes).context("decode SignedModelAdvertisement")?;
        if !remaining.is_empty() {
            bail!("signed advertisement contains trailing bytes");
        }
        ad.verify_at(now_ms)?;
        Ok(ad)
    }
}

// ---------------------------------------------------------------------------
// DhtTransport — small abstraction over what the registry needs from the DHT.
// ---------------------------------------------------------------------------

/// Minimal DHT surface the registry consumes.
///
/// The trait keeps signed-record validation independent from libp2p event-loop
/// ownership and gives tests a deterministic in-memory transport. Production
/// wiring is [`crate::PhaseNetDhtTransport`], which delegates publication and
/// multi-record lookup to the active [`phase_net::Discovery`] swarm.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates must_use on methods returning an already-must-use boxed Future"
)]
#[async_trait]
pub trait DhtTransport: Send + Sync {
    /// Publish (or refresh) a record under `key`. Idempotent — calling
    /// twice with the same key/value updates the existing record.
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()>;

    /// Look up records under `key`. Returns the raw byte payloads that
    /// other peers have published — the registry decodes and verifies
    /// each one before returning it to a caller.
    async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>>;

    /// Optional opaque provider discovery. Older test transports may opt out;
    /// production Phase transport implements the actual bounded operations.
    async fn start_providing_key(&self, _key: Vec<u8>) -> Result<()> {
        Err(ProviderDiscoveryUnsupported.into())
    }

    async fn get_provider_peers(&self, _key: Vec<u8>) -> Result<Vec<PeerId>> {
        Err(ProviderDiscoveryUnsupported.into())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("this transport does not support provider discovery")]
pub struct ProviderDiscoveryUnsupported;

/// A model discovered through signed, publisher-bound catalog records. Its
/// exact content identity is authoritative; human aliases are display names.
#[derive(Debug, Clone)]
pub struct NetworkModel {
    pub caps: ModelCapabilities,
    pub size_bytes: Option<u64>,
    pub format: Option<String>,
    pub providers: Vec<PeerId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogModel {
    caps: ModelCapabilities,
    size_bytes: Option<u64>,
    format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignedCatalog {
    schema_version: u32,
    issued_at: u64,
    valid_until: u64,
    pubkey: [u8; 32],
    models: Vec<CatalogModel>,
    signature: Vec<u8>,
}

impl SignedCatalog {
    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = b"phase:lucid:model-catalog:v1\0".to_vec();
        bytes.extend(postcard::to_allocvec(&(
            self.schema_version,
            self.issued_at,
            self.valid_until,
            self.pubkey,
            &self.models,
        ))?);
        Ok(bytes)
    }

    fn sign(models: Vec<CatalogModel>, identity: &NodeIdentity) -> Result<Self> {
        let now = unix_ms_now();
        let mut catalog = Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            issued_at: now,
            valid_until: now + CATALOG_TTL.as_millis() as u64,
            pubkey: identity.verifying_key().to_bytes(),
            models,
            signature: Vec::new(),
        };
        catalog.signature = identity
            .signing_key()
            .sign(&catalog.signing_bytes()?)
            .to_bytes()
            .to_vec();
        catalog.validate(peer_id_from_ed25519_pubkey(&catalog.pubkey)?, now)?;
        Ok(catalog)
    }

    fn validate(&self, publisher: PeerId, now: u64) -> Result<()> {
        if self.schema_version != CATALOG_SCHEMA_VERSION
            || self.models.len() > MAX_CATALOG_MODELS
            || self.issued_at == 0
            || self.valid_until <= self.issued_at
            || self.valid_until - self.issued_at > CATALOG_TTL.as_millis() as u64
            || self.issued_at > now.saturating_add(MAX_ADVERTISEMENT_FUTURE_SKEW.as_millis() as u64)
            || now >= self.valid_until
        {
            bail!("invalid or expired model catalog");
        }
        if peer_id_from_ed25519_pubkey(&self.pubkey)? != publisher {
            bail!("catalog publisher does not match discovered peer");
        }
        let signature: &[u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("catalog signature has wrong length"))?;
        VerifyingKey::from_bytes(&self.pubkey)?
            .verify(&self.signing_bytes()?, &Signature::from_bytes(signature))
            .context("verify model catalog signature")?;
        let mut identities = std::collections::HashSet::new();
        for model in &self.models {
            model.caps.validate_at(now)?;
            if !identities.insert(model.caps.model_cid) {
                bail!("catalog repeats an exact model identity");
            }
            if let Some(size) = model.size_bytes {
                if size == 0 || size > MAX_MODEL_SIZE_BYTES {
                    bail!("invalid catalog model size");
                }
            }
            if let Some(format) = &model.format {
                validate_capability_token(format, "catalog format")?;
            }
            if model.size_bytes.is_some() != model.format.is_some() {
                bail!("catalog content metadata must be complete or absent");
            }
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_allocvec(self)?;
        if bytes.len() > MAX_CATALOG_BYTES {
            bail!("model catalog exceeds wire size limit");
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8], publisher: PeerId, now: u64) -> Result<Self> {
        if bytes.len() > MAX_CATALOG_BYTES {
            bail!("model catalog exceeds wire size limit");
        }
        // Reject an attacker-declared model count before deserializing its
        // vector; byte limits alone do not constrain sequence size hints.
        let (_, remaining) = postcard::take_from_bytes::<(u32, u64, u64, [u8; 32])>(bytes)?;
        let (count, _) = postcard::take_from_bytes::<u64>(remaining)?;
        if count > MAX_CATALOG_MODELS as u64 {
            bail!("model catalog exceeds model count limit");
        }
        let (catalog, trailing): (Self, _) = postcard::take_from_bytes(bytes)?;
        if !trailing.is_empty() {
            bail!("model catalog contains trailing bytes");
        }
        catalog.validate(publisher, now)?;
        Ok(catalog)
    }
}

pub fn catalog_publisher_key(publisher: &PeerId) -> Vec<u8> {
    let mut key = CATALOG_KEY_PREFIX.to_vec();
    key.extend(publisher.to_bytes());
    key
}

#[derive(Clone)]
struct CachedCatalogModel {
    publisher: PeerId,
    expires_at: u64,
    model: CatalogModel,
}

#[derive(Default)]
struct NetworkCatalogCache {
    refreshed: Option<tokio::time::Instant>,
    models: Vec<CachedCatalogModel>,
}

async fn publish_model_catalog(
    transport: &dyn DhtTransport,
    identity: &NodeIdentity,
    loaded: &RwLock<HashMap<ModelCid, ModelCapabilities>>,
    installed: &RwLock<HashMap<String, InstalledModel>>,
    publication: &Mutex<()>,
) -> Result<()> {
    let _publication = publication.lock().await;
    // Opt-out is explicit for legacy/test transports and happens before any
    // catalog record is written. Production transport always implements this.
    transport
        .start_providing_key(CATALOG_SERVICE_KEY.to_vec())
        .await?;
    let loaded = loaded.read().await;
    let installed = installed.read().await;
    let now = unix_ms_now();
    let mut capabilities = loaded.values().cloned().collect::<Vec<_>>();
    capabilities.sort_by_key(|caps| caps.model_cid.0);
    let models = capabilities
        .into_iter()
        .take(MAX_CATALOG_MODELS)
        .map(|mut caps| {
            caps.advertised_at = now;
            caps.valid_until = now + ADVERTISEMENT_TTL.as_millis() as u64;
            let metadata = installed
                .values()
                .find(|model| model.model_cid == caps.model_cid);
            CatalogModel {
                caps,
                size_bytes: metadata.map(|model| model.size_bytes),
                format: metadata.map(|model| model.format.clone()),
            }
        })
        .collect();
    drop(installed);
    drop(loaded);
    let snapshot = SignedCatalog::sign(models, identity)?;
    let publisher = peer_id_from_ed25519_pubkey(&snapshot.pubkey)?;
    transport
        .put_record(catalog_publisher_key(&publisher), snapshot.encode()?)
        .await
}

fn merge_catalog_models(records: &[CachedCatalogModel], now: u64) -> Vec<NetworkModel> {
    let mut models: HashMap<ModelCid, NetworkModel> = HashMap::new();
    for entry in records {
        if entry.expires_at <= now || entry.model.caps.validate_at(now).is_err() {
            continue;
        }
        let value = models
            .entry(entry.model.caps.model_cid)
            .or_insert_with(|| NetworkModel {
                caps: entry.model.caps.clone(),
                size_bytes: entry.model.size_bytes,
                format: entry.model.format.clone(),
                providers: Vec::new(),
            });
        if value.size_bytes != entry.model.size_bytes || value.format != entry.model.format {
            // Conflicting self-reported metadata is unknown; never invent a
            // size or silently choose one publisher's content claim.
            value.size_bytes = None;
            value.format = None;
        }
        if !value.providers.contains(&entry.publisher) {
            value.providers.push(entry.publisher);
        }
    }
    let mut models = models.into_values().collect::<Vec<_>>();
    for model in &mut models {
        model.providers.sort_unstable();
    }
    models.sort_by(|left, right| {
        left.caps
            .model_id
            .cmp(&right.caps.model_id)
            .then_with(|| left.caps.model_cid.0.cmp(&right.caps.model_cid.0))
    });
    models
}

// ---------------------------------------------------------------------------
// ModelRegistry — public API.
// ---------------------------------------------------------------------------

/// Tracks locally loaded models, advertises them onto the DHT on a
/// refresh cadence, and answers peer-discovery queries.
///
/// Cheap to clone (everything inside is behind `Arc`), so a router can
/// stash one in its `axum::Extension` and have it be the single source
/// of truth for both `/api/tags` (local) and per-request peer lookups
/// (remote).
pub struct ModelRegistry {
    /// Persistent node identity. Used to sign every advertisement and
    /// to derive the pubkey embedded in the record.
    identity: NodeIdentity,

    /// DHT transport. Behind a trait so tests can substitute an
    /// in-memory recorder and so M5 has a clean wiring point.
    transport: Arc<dyn DhtTransport>,

    /// Locally loaded models. `model_cid` → capabilities. Wrapped in an
    /// `RwLock` because typical access is "read often (find by id),
    /// write rarely (load/unload)".
    loaded: Arc<RwLock<HashMap<ModelCid, ModelCapabilities>>>,

    /// Verified local content indexed by normalized model alias. Installed
    /// bytes are visible to the content plane but are never treated as proof
    /// that an inference worker has loaded the model.
    installed: Arc<RwLock<HashMap<String, InstalledModel>>>,

    /// Serializes alias conflict checks with installed-catalog insertion so
    /// concurrent pulls cannot both pass the check and race to rebind a name.
    install_registration: Arc<Mutex<()>>,

    /// Active TTL refresh task per advertised model. Cancelled on
    /// `withdraw` and on `Drop`.
    refresh_tasks: Arc<Mutex<HashMap<ModelCid, JoinHandle<()>>>>,

    /// One refresh task per locally cataloged alias. Installation starts this
    /// task; removing the catalog entry stops it.
    alias_refresh_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,

    /// Content serving is opt-in, so this map is populated only by
    /// [`Self::publish_installed_content_provider`].
    provider_refresh_tasks: Arc<Mutex<HashMap<ModelCid, JoinHandle<()>>>>,

    /// Last locally issued sequence per alias/provider record. These counters
    /// survive task replacement and withdrawal during the process lifetime so
    /// re-enabling a capability cannot publish a rollback.
    local_alias_sequences: Arc<Mutex<HashMap<String, u64>>>,
    local_provider_sequences: Arc<Mutex<HashMap<ModelCid, u64>>>,

    /// Refresh interval. Overridable for tests so we don't have to wait
    /// 5 real minutes to exercise the refresh path.
    refresh_interval: Duration,

    /// Highest accepted sequence and exact payload fingerprint for each
    /// `(alias, publisher)` pair. The fingerprint makes same-sequence
    /// equivocation visible across independent DHT queries.
    accepted_alias_records: AcceptedAliasRecords,

    /// Optional durable checkpoint for alias replay state. A configured path
    /// is fully validated during construction and atomically replaced before a
    /// newly accepted resolution can be returned.
    alias_replay_state_path: Option<Arc<PathBuf>>,

    /// Serializes alias replay comparison, durable checkpoint replacement, and
    /// the corresponding in-memory commit.
    alias_replay_update: Arc<Mutex<()>>,

    /// Highest accepted provider sequence and payload fingerprint for each
    /// `(content CID, publisher key)`. Keeping the fingerprint detects a peer
    /// reusing one sequence for two different signed claims across lookups.
    accepted_content_provider_records: AcceptedContentProviderRecords,
    catalog_refresh_task: Mutex<Option<JoinHandle<()>>>,
    catalog_publication: Arc<Mutex<()>>,
    catalog_query: Mutex<()>,
    network_catalog: RwLock<NetworkCatalogCache>,
}

impl ModelRegistry {
    async fn refresh_network_catalog(&self) {
        let mut task = self.catalog_refresh_task.lock().await;
        if task.is_none() {
            let transport = self.transport.clone();
            let identity = self.identity.clone();
            let loaded = self.loaded.clone();
            let installed = self.installed.clone();
            let publication = self.catalog_publication.clone();
            *task = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(CATALOG_REFRESH_INTERVAL).await;
                    if let Err(error) = publish_model_catalog(
                        transport.as_ref(),
                        &identity,
                        &loaded,
                        &installed,
                        &publication,
                    )
                    .await
                    {
                        if !error.is::<ProviderDiscoveryUnsupported>() {
                            warn!(%error, "model catalog refresh failed");
                        }
                    }
                }
            }));
        }
        drop(task);
        if let Err(error) = publish_model_catalog(
            self.transport.as_ref(),
            &self.identity,
            &self.loaded,
            &self.installed,
            &self.catalog_publication,
        )
        .await
        {
            if !error.is::<ProviderDiscoveryUnsupported>() {
                warn!(%error, "model catalog publication failed; legacy advertisements remain available");
            }
        }
    }

    /// Discover model catalogs without requiring a model name in advance.
    /// One serialized query per cache period bounds amplification from local
    /// clients polling the Ollama model list. Remote identities stay internal.
    pub async fn discover_network_models(&self) -> Result<Vec<NetworkModel>> {
        {
            let cache = self.network_catalog.read().await;
            if cache
                .refreshed
                .is_some_and(|at| at.elapsed() < CATALOG_REFRESH_INTERVAL)
            {
                return Ok(merge_catalog_models(&cache.models, unix_ms_now()));
            }
        }
        let _query = self.catalog_query.lock().await;
        {
            let cache = self.network_catalog.read().await;
            if cache
                .refreshed
                .is_some_and(|at| at.elapsed() < CATALOG_REFRESH_INTERVAL)
            {
                return Ok(merge_catalog_models(&cache.models, unix_ms_now()));
            }
        }
        let result = tokio::time::timeout(CATALOG_QUERY_TIMEOUT, self.query_network_catalog())
            .await
            .map_err(|_| anyhow!("network model catalog discovery timed out"))
            .and_then(|result| result);
        let mut cache = self.network_catalog.write().await;
        // Failed attempts are also rate limited. Existing still-valid entries
        // remain useful during a transient discovery failure.
        cache.refreshed = Some(tokio::time::Instant::now());
        match result {
            Ok(models) => {
                cache.models = models;
                Ok(merge_catalog_models(&cache.models, unix_ms_now()))
            }
            Err(error) if cache.models.is_empty() => Err(error),
            Err(_) => Ok(merge_catalog_models(&cache.models, unix_ms_now())),
        }
    }

    /// Return only still-valid entries from the last validated discovery.
    pub async fn cached_network_models(&self) -> Vec<NetworkModel> {
        merge_catalog_models(&self.network_catalog.read().await.models, unix_ms_now())
    }

    async fn query_network_catalog(&self) -> Result<Vec<CachedCatalogModel>> {
        let mut publishers = self
            .transport
            .get_provider_peers(CATALOG_SERVICE_KEY.to_vec())
            .await?;
        publishers.sort_unstable();
        publishers.dedup();
        publishers.truncate(MAX_CATALOG_PUBLISHERS);
        let mut total_bytes = 0usize;
        let mut models = Vec::new();
        let now = unix_ms_now();
        for publisher in publishers {
            let records = match self
                .transport
                .get_record(catalog_publisher_key(&publisher))
                .await
            {
                Ok(records) => records,
                Err(error) => {
                    debug!(%error, "catalog publisher lookup failed");
                    continue;
                }
            };
            let mut newest: Option<SignedCatalog> = None;
            for bytes in records {
                let Some(next) = total_bytes.checked_add(bytes.len()) else {
                    break;
                };
                if next > MAX_CATALOG_TOTAL_BYTES {
                    return Ok(models);
                }
                total_bytes = next;
                match SignedCatalog::decode(&bytes, publisher, now) {
                    Ok(record)
                        if newest
                            .as_ref()
                            .is_none_or(|previous| record.issued_at > previous.issued_at) =>
                    {
                        newest = Some(record)
                    }
                    Ok(_) => {}
                    Err(error) => debug!(%error, "discard unverifiable model catalog"),
                }
            }
            if let Some(catalog) = newest {
                models.extend(catalog.models.into_iter().map(|model| CachedCatalogModel {
                    publisher,
                    expires_at: catalog.valid_until,
                    model,
                }));
            }
            if total_bytes >= MAX_CATALOG_TOTAL_BYTES {
                break;
            }
        }
        Ok(models)
    }

    /// Create a registry bound to `identity` and `transport`. The
    /// registry does not start any background work until
    /// [`Self::advertise_loaded`] is called.
    pub fn new(identity: NodeIdentity, transport: Arc<dyn DhtTransport>) -> Self {
        Self {
            identity,
            transport,
            loaded: Arc::new(RwLock::new(HashMap::new())),
            installed: Arc::new(RwLock::new(HashMap::new())),
            install_registration: Arc::new(Mutex::new(())),
            refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            alias_refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            provider_refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            local_alias_sequences: Arc::new(Mutex::new(HashMap::new())),
            local_provider_sequences: Arc::new(Mutex::new(HashMap::new())),
            refresh_interval: TTL_REFRESH_INTERVAL,
            accepted_alias_records: Arc::new(RwLock::new(HashMap::new())),
            alias_replay_state_path: None,
            alias_replay_update: Arc::new(Mutex::new(())),
            accepted_content_provider_records: Arc::new(RwLock::new(HashMap::new())),
            catalog_refresh_task: Mutex::new(None),
            catalog_publication: Arc::new(Mutex::new(())),
            catalog_query: Mutex::new(()),
            network_catalog: RwLock::new(NetworkCatalogCache::default()),
        }
    }

    /// Create a registry with durable signed-alias replay protection.
    ///
    /// If `path` exists, the entire bounded file is decoded and validated
    /// before any loaded state is installed. If it does not exist, a private
    /// empty checkpoint is atomically created so configuration and permission
    /// failures are reported at startup instead of during the first query.
    /// [`Self::new`] remains the explicitly in-memory option.
    pub fn new_with_alias_replay_state(
        identity: NodeIdentity,
        transport: Arc<dyn DhtTransport>,
        path: impl Into<PathBuf>,
    ) -> Result<Self> {
        let path = path.into();
        let loaded = load_alias_replay_state(&path)?;
        let accepted = match loaded {
            Some(accepted) => accepted,
            None => {
                let accepted = HashMap::new();
                write_alias_replay_state(&path, &accepted)?;
                accepted
            }
        };
        let mut registry = Self::new(identity, transport);
        registry.accepted_alias_records = Arc::new(RwLock::new(accepted));
        registry.alias_replay_state_path = Some(Arc::new(path));
        Ok(registry)
    }

    /// Test-only constructor: same as [`Self::new`] but with a
    /// caller-supplied refresh interval. Used by the unit tests to
    /// exercise the refresh path under `tokio::time::pause`.
    #[cfg(test)]
    pub fn with_refresh_interval(
        identity: NodeIdentity,
        transport: Arc<dyn DhtTransport>,
        refresh_interval: Duration,
    ) -> Self {
        Self {
            identity,
            transport,
            loaded: Arc::new(RwLock::new(HashMap::new())),
            installed: Arc::new(RwLock::new(HashMap::new())),
            install_registration: Arc::new(Mutex::new(())),
            refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            alias_refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            provider_refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            local_alias_sequences: Arc::new(Mutex::new(HashMap::new())),
            local_provider_sequences: Arc::new(Mutex::new(HashMap::new())),
            refresh_interval,
            accepted_alias_records: Arc::new(RwLock::new(HashMap::new())),
            alias_replay_state_path: None,
            alias_replay_update: Arc::new(Mutex::new(())),
            accepted_content_provider_records: Arc::new(RwLock::new(HashMap::new())),
            catalog_refresh_task: Mutex::new(None),
            catalog_publication: Arc::new(Mutex::new(())),
            catalog_query: Mutex::new(()),
            network_catalog: RwLock::new(NetworkCatalogCache::default()),
        }
    }

    /// Mark `caps.model_cid` as loaded and start advertising. Returns
    /// when the first publish has completed (so a caller that turns
    /// around and immediately queries the DHT won't race the first put).
    ///
    /// Calling twice for the same `model_cid` replaces the existing
    /// advertisement and restarts the refresh task — fine, since the
    /// new advertisement supersedes the old one anyway.
    pub async fn advertise_loaded(&self, caps: ModelCapabilities) -> Result<()> {
        if self
            .installed
            .read()
            .await
            .get(&caps.model_id)
            .is_some_and(|installed| installed.model_cid != caps.model_cid)
        {
            bail!(
                "loaded model alias '{}' conflicts with installed content",
                caps.model_id
            );
        }
        let cid = caps.model_cid;

        // 1. Sign + publish the initial advertisement.
        let ad = SignedModelAdvertisement::sign(caps.clone(), &self.identity)?;
        let key = cid.dht_key();
        let value = ad.encode()?;
        self.transport
            .put_record(key.clone(), value)
            .await
            .context("initial advertisement put_record")?;

        // 2. Update the in-memory loaded set.
        {
            let mut loaded = self.loaded.write().await;
            loaded.insert(cid, caps.clone());
        }

        // 3. Spawn (or replace) the refresh task. The task owns clones
        //    of the bits it needs — registry doesn't have to live as
        //    long as the task.
        let transport = Arc::clone(&self.transport);
        let identity = self.identity.clone();
        let interval = self.refresh_interval;
        let cid_for_task = cid;
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                // Re-sign with a fresh `advertised_at` so consumers see
                // refresh-rate freshness even if the wire payload is
                // otherwise unchanged.
                let mut refreshed = caps.clone();
                refreshed.advertised_at = unix_ms_now();
                refreshed.valid_until =
                    refreshed.advertised_at + ADVERTISEMENT_TTL.as_millis() as u64;

                let signed = match SignedModelAdvertisement::sign(refreshed, &identity) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            "model registry: sign-on-refresh failed for {}: {e}",
                            cid_for_task.to_hex()
                        );
                        continue;
                    }
                };
                let value = match signed.encode() {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "model registry: encode-on-refresh failed for {}: {e}",
                            cid_for_task.to_hex()
                        );
                        continue;
                    }
                };
                if let Err(e) = transport.put_record(cid_for_task.dht_key(), value).await {
                    // Network blips are expected; log and keep going.
                    debug!(
                        "model registry: refresh put_record failed for {}: {e}",
                        cid_for_task.to_hex()
                    );
                }
            }
        });

        let mut tasks = self.refresh_tasks.lock().await;
        if let Some(prev) = tasks.insert(cid, task) {
            prev.abort();
        }
        drop(tasks);
        self.refresh_network_catalog().await;
        Ok(())
    }

    /// Stop advertising `model_cid`. Cancels the refresh task and drops
    /// the entry from the loaded set.
    ///
    /// We deliberately do **not** publish a "tombstone" record: the
    /// existing advertisement will expire from the DHT on its own, and
    /// other peers will see `valid_until` slip into the past long
    /// before the libp2p TTL fires. A signed-withdrawal record may
    /// arrive in a later milestone if "phantom serving node" complaints
    /// turn out to be a real UX problem.
    pub async fn withdraw(&self, model_cid: &ModelCid) -> Result<()> {
        {
            let mut loaded = self.loaded.write().await;
            loaded.remove(model_cid);
        }
        let mut tasks = self.refresh_tasks.lock().await;
        if let Some(task) = tasks.remove(model_cid) {
            task.abort();
        }
        drop(tasks);
        self.refresh_network_catalog().await;
        Ok(())
    }

    /// All locally loaded models. Used by `/api/tags`. Returns a snapshot
    /// (clone) so the caller doesn't hold the read lock across an await.
    pub fn local_models(&self) -> Vec<ModelCapabilities> {
        // Read lock is held briefly; the registry is structured so this
        // never contends with a refresh task (which only ever writes its
        // own model under the write lock during advertise/withdraw).
        match self.loaded.try_read() {
            Ok(guard) => guard.values().cloned().collect(),
            Err(_) => {
                // A write is in flight — load/unload is rare, so a
                // momentary empty snapshot is acceptable. The caller
                // will see updated state on the next call.
                Vec::new()
            }
        }
    }

    /// Async variant of [`Self::local_models`] that waits for the read
    /// lock rather than returning an empty snapshot. Preferred from
    /// async code paths.
    pub async fn local_models_async(&self) -> Vec<ModelCapabilities> {
        self.loaded.read().await.values().cloned().collect()
    }

    /// Snapshot of content that has been verified and installed locally. This
    /// list is independent of [`Self::local_models`]: a consume-only node may
    /// expose these entries from `/api/tags` without advertising an inference
    /// worker. Results are deterministic by alias then CID.
    pub fn local_installed(&self) -> Vec<InstalledModel> {
        let Ok(installed) = self.installed.try_read() else {
            return Vec::new();
        };
        let mut records = installed.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.model_id
                .cmp(&right.model_id)
                .then_with(|| left.model_cid.0.cmp(&right.model_cid.0))
        });
        records
    }

    /// Async variant of [`Self::local_installed`] that waits for the read lock.
    pub async fn local_installed_async(&self) -> Vec<InstalledModel> {
        let mut records = self
            .installed
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.model_id
                .cmp(&right.model_id)
                .then_with(|| left.model_cid.0.cmp(&right.model_cid.0))
        });
        records
    }

    /// Resolve a request-facing alias to the exact immutable content CID.
    /// Direct CIDs and locally verified aliases take precedence; otherwise a
    /// signed network alias must resolve without publisher conflict. This API
    /// deliberately does not synthesize the legacy name-hash placeholder.
    pub async fn resolve_model_cid(&self, model: &str) -> Result<Option<ModelCid>> {
        if let Ok(cid) = ModelCid::from_hex(model) {
            return Ok(Some(cid));
        }
        let normalized = normalize_model_alias(model)?;
        if let Some(installed) = self.installed.read().await.get(&normalized) {
            return Ok(Some(installed.model_cid));
        }
        if let Some(caps) = self
            .loaded
            .read()
            .await
            .values()
            .find(|caps| caps.model_id == normalized)
        {
            return Ok(Some(caps.model_cid));
        }
        self.resolve_alias_cid(&normalized).await
    }

    /// Import one GGUF into the verified content store and make it eligible
    /// for execution under its immutable SHA-256 CID.
    ///
    /// The source must resolve to a direct child of `source_root`; symlink
    /// escapes and nested paths are rejected. Bytes are streamed through the
    /// existing [`ArtifactStore`] verifier, then atomically hard-linked as
    /// `<verified_model_dir>/<cid>.gguf` so the worker can only open the exact
    /// content named in a signed manifest. The signed alias is published only
    /// after both publication steps succeed.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_verified_gguf(
        &self,
        artifact_store: Arc<ArtifactStore>,
        source_root: PathBuf,
        source_path: PathBuf,
        verified_model_dir: PathBuf,
        alias: &str,
        context_length: u32,
        max_concurrent: u32,
        backend: &str,
    ) -> Result<ModelCapabilities> {
        let normalized_alias = normalize_model_alias(alias)?;
        let (model_cid, size_bytes) = tokio::task::spawn_blocking(move || {
            import_gguf_bytes(
                &artifact_store,
                &source_root,
                &source_path,
                &verified_model_dir,
            )
        })
        .await
        .context("join verified GGUF import task")??;

        self.register_verified_alias(
            &normalized_alias,
            model_cid,
            size_bytes,
            context_length,
            max_concurrent,
            backend,
        )
        .await
    }

    /// Import a verified local GGUF file (including nested LM Studio folders
    /// and Ollama content-addressed `sha256-*` blobs whose header starts with
    /// `GGUF`), verifying non-symlink containment inside `source_root`,
    /// hashing into `artifact_store`, and publishing the signed alias.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_verified_gguf_file(
        &self,
        artifact_store: Arc<ArtifactStore>,
        source_root: PathBuf,
        source_path: PathBuf,
        verified_model_dir: PathBuf,
        alias: &str,
        context_length: u32,
        max_concurrent: u32,
        backend: &str,
    ) -> Result<ModelCapabilities> {
        let normalized_alias = normalize_model_alias(alias)?;
        let (model_cid, size_bytes) = tokio::task::spawn_blocking(move || {
            import_gguf_file_bytes(
                &artifact_store,
                &source_root,
                &source_path,
                &verified_model_dir,
            )
        })
        .await
        .context("join verified GGUF file import task")??;

        self.register_verified_alias(
            &normalized_alias,
            model_cid,
            size_bytes,
            context_length,
            max_concurrent,
            backend,
        )
        .await
    }

    /// Register a blob that a pull coordinator has already downloaded and
    /// committed through [`ArtifactStore::commit_staged_blob`]. The stored
    /// bytes are independently re-hashed before the worker-visible hard link,
    /// and signed alias are published.
    ///
    /// This is deliberately a content-only operation: it does not call
    /// [`Self::advertise_loaded`] and therefore cannot publish a
    /// [`SignedModelAdvertisement`] on a consume-only node.
    pub async fn register_verified_gguf_blob(
        &self,
        artifact_store: Arc<ArtifactStore>,
        verified_model_dir: PathBuf,
        alias: &str,
        model_cid: ModelCid,
        expected_size: u64,
    ) -> Result<InstalledModel> {
        let normalized_alias = normalize_model_alias(alias)?;
        let cid_hex = model_cid.to_hex();
        tokio::task::spawn_blocking(move || {
            let blob_id = BlobId::from_hex(&cid_hex).context("convert model CID to blob ID")?;
            let installed = artifact_store
                .get_blob(&blob_id)?
                .context("verified blob is not installed in the artifact store")?;
            if installed.size_bytes != expected_size {
                bail!(
                    "installed blob size mismatch: expected {expected_size}, got {}",
                    installed.size_bytes
                );
            }
            publish_worker_model_link(
                &installed.path,
                &verified_model_dir,
                model_cid,
                &blob_id,
                expected_size,
            )
        })
        .await
        .context("join verified blob registration task")??;

        self.register_installed_content(&normalized_alias, model_cid, "gguf", expected_size)
            .await
    }

    /// Record verified local content, publish its signed alias, and return the
    /// local catalog entry. No inference or provider advertisement is emitted:
    /// only [`Self::advertise_loaded`] may publish under [`MODEL_KEY_PREFIX`],
    /// while content serving requires a separate explicit call to
    /// [`Self::publish_installed_content_provider`].
    async fn register_installed_content(
        &self,
        alias: &str,
        model_cid: ModelCid,
        format: &str,
        size_bytes: u64,
    ) -> Result<InstalledModel> {
        let _registration = self.install_registration.lock().await;
        let normalized_alias = normalize_model_alias(alias)?;
        if let Some(existing) = self.installed.read().await.get(&normalized_alias) {
            if existing.model_cid != model_cid
                || existing.size_bytes != size_bytes
                || existing.format != format
            {
                bail!(
                    "alias '{}' is already bound to different installed content metadata (CID {}); refusing replacement with {}",
                    normalized_alias,
                    existing.model_cid.to_hex(),
                    model_cid.to_hex()
                );
            }
            return Ok(existing.clone());
        }
        if self.installed.read().await.values().any(|existing| {
            existing.model_cid == model_cid
                && (existing.size_bytes != size_bytes || existing.format != format)
        }) {
            bail!(
                "installed metadata conflicts with an existing record for CID {}",
                model_cid.to_hex()
            );
        }
        if let Some(existing) = self
            .loaded
            .read()
            .await
            .values()
            .find(|caps| caps.model_id == normalized_alias)
        {
            if existing.model_cid != model_cid {
                bail!(
                    "alias '{}' is already loaded from verified CID {}; refusing replacement with {}",
                    normalized_alias,
                    existing.model_cid.to_hex(),
                    model_cid.to_hex()
                );
            }
        }

        let installed_at = unix_ms_now().max(1);
        if model_cid.0.iter().all(|byte| *byte == 0) {
            bail!("installed content CID cannot be all zeroes");
        }
        if size_bytes == 0 || size_bytes > MAX_MODEL_SIZE_BYTES {
            bail!("installed content size is outside the supported range");
        }
        validate_capability_token(format, "content format")?;
        {
            let tasks = self.alias_refresh_tasks.lock().await;
            if tasks.len() >= MAX_LOCAL_CONTENT_REFRESH_TASKS {
                bail!(
                    "installed alias refresh task limit ({MAX_LOCAL_CONTENT_REFRESH_TASKS}) reached"
                );
            }
        }
        let sequence = {
            let mut sequences = self.local_alias_sequences.lock().await;
            next_local_sequence(&mut sequences, normalized_alias.clone())?
        };
        let alias_record =
            AliasRecord::new(&normalized_alias, model_cid, format, size_bytes, sequence)?;

        self.publish_alias(alias_record).await?;

        let installed = InstalledModel {
            model_id: normalized_alias.clone(),
            model_cid,
            format: format.to_string(),
            size_bytes,
            installed_at,
        };
        self.installed
            .write()
            .await
            .insert(normalized_alias, installed.clone());
        self.start_installed_alias_refresh(&installed).await?;
        Ok(installed)
    }

    async fn start_installed_alias_refresh(&self, installed: &InstalledModel) -> Result<()> {
        let alias = installed.model_id.clone();
        let key = alias_dht_key(&alias)?;
        let cid = installed.model_cid;
        let format = installed.format.clone();
        let size_bytes = installed.size_bytes;
        let transport = Arc::clone(&self.transport);
        let identity = self.identity.clone();
        let sequences = Arc::clone(&self.local_alias_sequences);
        let interval = self.refresh_interval;
        let alias_for_task = alias.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let sequence = {
                    let mut sequences = sequences.lock().await;
                    match next_local_sequence(&mut sequences, alias_for_task.clone()) {
                        Ok(sequence) => sequence,
                        Err(error) => {
                            warn!(
                                alias = %alias_for_task,
                                %error,
                                "model registry: alias refresh sequence exhausted"
                            );
                            break;
                        }
                    }
                };
                let record = match AliasRecord::new(
                    &alias_for_task,
                    cid,
                    format.clone(),
                    size_bytes,
                    sequence,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        warn!(
                            alias = %alias_for_task,
                            %error,
                            "model registry: build alias refresh failed"
                        );
                        continue;
                    }
                };
                let value = match SignedAliasRecord::sign(record, &identity)
                    .and_then(|signed| signed.encode())
                {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(
                            alias = %alias_for_task,
                            %error,
                            "model registry: sign alias refresh failed"
                        );
                        continue;
                    }
                };
                if let Err(error) = transport.put_record(key.clone(), value).await {
                    debug!(
                        alias = %alias_for_task,
                        %error,
                        "model registry: alias refresh put_record failed"
                    );
                }
            }
        });

        let mut tasks = self.alias_refresh_tasks.lock().await;
        if tasks.len() >= MAX_LOCAL_CONTENT_REFRESH_TASKS {
            task.abort();
            bail!("installed alias refresh task limit ({MAX_LOCAL_CONTENT_REFRESH_TASKS}) reached");
        }
        if let Some(previous) = tasks.insert(alias, task) {
            previous.abort();
        }
        Ok(())
    }

    async fn register_verified_alias(
        &self,
        normalized_alias: &str,
        model_cid: ModelCid,
        size_bytes: u64,
        context_length: u32,
        max_concurrent: u32,
        backend: &str,
    ) -> Result<ModelCapabilities> {
        self.register_installed_content(normalized_alias, model_cid, "gguf", size_bytes)
            .await?;

        if let Some(existing) = self
            .loaded
            .read()
            .await
            .values()
            .find(|caps| caps.model_id == normalized_alias)
        {
            if existing.model_cid != model_cid {
                bail!(
                    "alias '{}' is already bound locally to verified CID {}; refusing replacement with {}",
                    normalized_alias,
                    existing.model_cid.to_hex(),
                    model_cid.to_hex()
                );
            }
            return Ok(existing.clone());
        }

        let caps = ModelCapabilities::now(
            normalized_alias,
            model_cid,
            "unknown",
            context_length,
            max_concurrent,
            backend,
        );
        self.advertise_loaded(caps.clone()).await?;
        Ok(caps)
    }

    /// Publish a signed human alias after the caller has verified and
    /// atomically installed the referenced content.
    pub async fn publish_alias(&self, record: AliasRecord) -> Result<()> {
        let key = alias_dht_key(&record.alias)?;
        let signed = SignedAliasRecord::sign(record, &self.identity)?;
        self.transport
            .put_record(key, signed.encode()?)
            .await
            .context("publish signed model alias")
    }

    /// Explicitly opt one installed CID into content serving. Callers must only
    /// invoke this after the blob request handler is installed and reachable.
    /// Merely downloading or importing content never calls this method. A
    /// successful call starts (or replaces) the CID's periodic refresh task.
    pub async fn publish_installed_content_provider(&self, model_cid: &ModelCid) -> Result<()> {
        let _registration = self.install_registration.lock().await;
        let installed = self
            .installed
            .read()
            .await
            .values()
            .find(|record| record.model_cid == *model_cid)
            .cloned()
            .context("cannot publish provider claim for content that is not installed")?;

        {
            let mut tasks = self.provider_refresh_tasks.lock().await;
            if !tasks.contains_key(model_cid) && tasks.len() >= MAX_LOCAL_CONTENT_REFRESH_TASKS {
                bail!(
                    "content-provider refresh task limit ({MAX_LOCAL_CONTENT_REFRESH_TASKS}) reached"
                );
            }
            if let Some(previous) = tasks.remove(model_cid) {
                previous.abort();
            }
        }

        let provider = peer_id_from_ed25519_pubkey(&self.identity.verifying_key().to_bytes())?;
        let sequence = {
            let mut sequences = self.local_provider_sequences.lock().await;
            next_local_sequence(&mut sequences, *model_cid)?
        };
        let record = ContentProviderRecord::new(
            installed.model_cid,
            installed.size_bytes,
            installed.format.clone(),
            provider,
            sequence,
        )?;
        let key = content_provider_dht_key(&record.model_cid);
        let signed = SignedContentProviderRecord::sign(record, &self.identity)?;
        self.transport
            .put_record(key.clone(), signed.encode()?)
            .await
            .context("publish signed content-provider record")?;

        let transport = Arc::clone(&self.transport);
        let identity = self.identity.clone();
        let sequences = Arc::clone(&self.local_provider_sequences);
        let interval = self.refresh_interval;
        let cid = *model_cid;
        let format = installed.format;
        let size_bytes = installed.size_bytes;
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let sequence = {
                    let mut sequences = sequences.lock().await;
                    match next_local_sequence(&mut sequences, cid) {
                        Ok(sequence) => sequence,
                        Err(error) => {
                            warn!(
                                cid = %cid.to_hex(),
                                %error,
                                "model registry: provider refresh sequence exhausted"
                            );
                            break;
                        }
                    }
                };
                let record = match ContentProviderRecord::new(
                    cid,
                    size_bytes,
                    format.clone(),
                    provider,
                    sequence,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        warn!(
                            cid = %cid.to_hex(),
                            %error,
                            "model registry: build provider refresh failed"
                        );
                        continue;
                    }
                };
                let value = match SignedContentProviderRecord::sign(record, &identity)
                    .and_then(|signed| signed.encode())
                {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(
                            cid = %cid.to_hex(),
                            %error,
                            "model registry: sign provider refresh failed"
                        );
                        continue;
                    }
                };
                if let Err(error) = transport.put_record(key.clone(), value).await {
                    debug!(
                        cid = %cid.to_hex(),
                        %error,
                        "model registry: provider refresh put_record failed"
                    );
                }
            }
        });

        self.provider_refresh_tasks
            .lock()
            .await
            .insert(*model_cid, task);
        Ok(())
    }

    /// Stop refreshing this node's content-provider claim without removing the
    /// installed catalog entry or affecting inference advertisements. The last
    /// signed claim remains harmlessly discoverable until its short TTL expires.
    pub async fn withdraw_content_provider(&self, model_cid: &ModelCid) -> Result<()> {
        let _registration = self.install_registration.lock().await;
        self.stop_content_provider_refresh(model_cid).await;
        Ok(())
    }

    async fn stop_content_provider_refresh(&self, model_cid: &ModelCid) {
        if let Some(task) = self.provider_refresh_tasks.lock().await.remove(model_cid) {
            task.abort();
        }
    }

    /// Remove one installed alias from the local catalog and stop its alias
    /// refresh. If it was the last alias for the CID, any provider refresh is
    /// also stopped; loaded inference state remains independently controlled by
    /// [`Self::withdraw`].
    pub async fn withdraw_installed_content(&self, alias: &str) -> Result<Option<InstalledModel>> {
        let normalized = normalize_model_alias(alias)?;
        let _registration = self.install_registration.lock().await;
        if let Some(task) = self.alias_refresh_tasks.lock().await.remove(&normalized) {
            task.abort();
        }
        let mut catalog = self.installed.write().await;
        let last_provider_cid = catalog.get(&normalized).and_then(|installed| {
            let alias_count = catalog
                .values()
                .filter(|candidate| candidate.model_cid == installed.model_cid)
                .count();
            (alias_count == 1).then_some(installed.model_cid)
        });
        if let Some(cid) = last_provider_cid {
            self.stop_content_provider_refresh(&cid).await;
        }
        let removed = catalog.remove(&normalized);
        Ok(removed)
    }

    /// Resolve every valid publisher candidate for an alias. Results are
    /// deterministic by publisher PeerId. Conflicting publishers remain
    /// visible to the caller; a publisher that emits two different records at
    /// the same sequence is rejected as equivocation.
    pub async fn resolve_alias(&self, alias: &str) -> Result<Vec<ResolvedAlias>> {
        let normalized = normalize_model_alias(alias)?;
        let raw_records = self
            .transport
            .get_record(alias_dht_key(&normalized)?)
            .await?;
        let now = unix_ms_now();
        let mut decoded_bytes = 0usize;
        let mut by_publisher: HashMap<[u8; 32], SignedAliasRecord> = HashMap::new();

        for (record_index, raw) in raw_records.into_iter().enumerate() {
            if record_index >= MAX_ALIAS_RECORDS_PER_QUERY {
                debug!(
                    alias = %normalized,
                    limit = MAX_ALIAS_RECORDS_PER_QUERY,
                    "registry: alias query record limit reached"
                );
                break;
            }
            if raw.len() > MAX_ALIAS_RECORD_BYTES {
                debug!(
                    alias = %normalized,
                    record_bytes = raw.len(),
                    "registry: drop oversized alias record"
                );
                continue;
            }
            let Some(next_decoded_bytes) = decoded_bytes.checked_add(raw.len()) else {
                debug!(alias = %normalized, "registry: alias query byte count overflow");
                break;
            };
            if next_decoded_bytes > MAX_ALIAS_DECODE_BYTES_PER_QUERY {
                debug!(
                    alias = %normalized,
                    limit = MAX_ALIAS_DECODE_BYTES_PER_QUERY,
                    "registry: alias query decode-byte limit reached"
                );
                break;
            }
            decoded_bytes = next_decoded_bytes;

            let signed = match SignedAliasRecord::decode_at(&raw, now) {
                Ok(record) if record.record.alias == normalized => record,
                Ok(_) => {
                    debug!(alias = %normalized, "registry: drop alias record for a different key");
                    continue;
                }
                Err(error) => {
                    debug!(alias = %normalized, %error, "registry: drop invalid alias record");
                    continue;
                }
            };

            match by_publisher.get(&signed.publisher_pubkey) {
                Some(current) if signed.record.sequence < current.record.sequence => continue,
                Some(current) if signed.record.sequence == current.record.sequence => {
                    if signed.record != current.record {
                        bail!(
                            "alias publisher equivocated at sequence {} for '{}'",
                            signed.record.sequence,
                            normalized
                        );
                    }
                    continue;
                }
                _ => {
                    by_publisher.insert(signed.publisher_pubkey, signed);
                }
            }
        }

        let _update = self.alias_replay_update.lock().await;
        let accepted = self.accepted_alias_records.read().await.clone();
        let new_state_count = by_publisher
            .keys()
            .filter(|pubkey| !accepted.contains_key(&(normalized.clone(), **pubkey)))
            .count();
        if accepted
            .len()
            .checked_add(new_state_count)
            .is_none_or(|count| count > MAX_TRACKED_ALIAS_RECORDS)
        {
            bail!(
                "alias replay state exceeds the {}-record limit",
                MAX_TRACKED_ALIAS_RECORDS
            );
        }

        let mut next_accepted = accepted;
        let mut state_changed = false;
        let mut resolved = Vec::with_capacity(by_publisher.len());
        for (pubkey, signed) in by_publisher {
            let sequence_key = (normalized.clone(), pubkey);
            let fingerprint = alias_record_fingerprint(&signed.record)?;
            if let Some(previous) = next_accepted.get(&sequence_key) {
                if signed.record.sequence < previous.sequence {
                    debug!(
                        alias = %normalized,
                        sequence = signed.record.sequence,
                        "registry: drop rolled-back alias sequence"
                    );
                    continue;
                }
                if signed.record.sequence == previous.sequence
                    && fingerprint != previous.fingerprint
                {
                    bail!(
                        "alias publisher equivocated at sequence {} for '{}'",
                        signed.record.sequence,
                        normalized
                    );
                }
            }

            let next = AcceptedAliasRecord {
                sequence: signed.record.sequence,
                fingerprint,
            };
            state_changed |= next_accepted.get(&sequence_key) != Some(&next);
            next_accepted.insert(sequence_key, next);
            resolved.push(ResolvedAlias {
                record: signed.record,
                publisher: peer_id_from_ed25519_pubkey(&pubkey)?,
            });
        }
        resolved.sort_by_key(|candidate| candidate.publisher.to_string());

        if state_changed {
            if let Some(path) = &self.alias_replay_state_path {
                let path = PathBuf::from(path.as_ref());
                let checkpoint = next_accepted.clone();
                tokio::task::spawn_blocking(move || write_alias_replay_state(&path, &checkpoint))
                    .await
                    .context("join alias replay-state checkpoint task")??;
            }
            *self.accepted_alias_records.write().await = next_accepted;
        }
        Ok(resolved)
    }

    /// Resolve an alias only when all valid publishers agree on the same
    /// immutable CID. Publisher disagreement is surfaced rather than resolved
    /// by DHT arrival order.
    pub async fn resolve_alias_cid(&self, alias: &str) -> Result<Option<ModelCid>> {
        let candidates = self.resolve_alias(alias).await?;
        let Some(first) = candidates.first() else {
            return Ok(None);
        };
        if candidates
            .iter()
            .any(|candidate| candidate.record.model_cid != first.record.model_cid)
        {
            let publishers = candidates
                .iter()
                .map(|candidate| candidate.publisher.to_string())
                .collect::<Vec<_>>()
                .join(",");
            bail!(
                "conflicting signed mappings for alias '{}'; publishers={publishers}",
                normalize_model_alias(alias)?
            );
        }
        Ok(Some(first.record.model_cid))
    }

    /// Find peers that claim they can transfer the exact bytes for
    /// `model_cid`. This reads only the content-provider namespace and never
    /// treats a provider as an inference worker.
    ///
    /// Invalid, expired, oversized, wrong-CID, and rolled-back records are
    /// discarded. Same-sequence conflicting records from one signing key are
    /// surfaced as equivocation. One newest candidate per provider is returned
    /// in deterministic PeerId order.
    pub async fn find_content_providers(
        &self,
        model_cid: &ModelCid,
    ) -> Result<Vec<ContentProviderCandidate>> {
        let raw_records = self
            .transport
            .get_record(content_provider_dht_key(model_cid))
            .await?;
        let now = unix_ms_now();
        let mut decoded_bytes = 0usize;
        let mut by_publisher: HashMap<[u8; 32], SignedContentProviderRecord> = HashMap::new();

        for (record_index, raw) in raw_records.into_iter().enumerate() {
            if record_index >= MAX_CONTENT_PROVIDER_RECORDS_PER_QUERY {
                debug!(
                    cid = %model_cid.to_hex(),
                    limit = MAX_CONTENT_PROVIDER_RECORDS_PER_QUERY,
                    "registry: content-provider query record limit reached"
                );
                break;
            }
            if raw.len() > MAX_CONTENT_PROVIDER_RECORD_BYTES {
                debug!(
                    cid = %model_cid.to_hex(),
                    record_bytes = raw.len(),
                    "registry: drop oversized content-provider record"
                );
                continue;
            }
            let Some(next_decoded_bytes) = decoded_bytes.checked_add(raw.len()) else {
                debug!(cid = %model_cid.to_hex(), "registry: provider query byte count overflow");
                break;
            };
            if next_decoded_bytes > MAX_CONTENT_PROVIDER_DECODE_BYTES_PER_QUERY {
                debug!(
                    cid = %model_cid.to_hex(),
                    limit = MAX_CONTENT_PROVIDER_DECODE_BYTES_PER_QUERY,
                    "registry: provider query decode-byte limit reached"
                );
                break;
            }
            decoded_bytes = next_decoded_bytes;

            let signed = match SignedContentProviderRecord::decode_at(&raw, now) {
                Ok(record) if record.record.model_cid == *model_cid => record,
                Ok(_) => {
                    debug!(
                        cid = %model_cid.to_hex(),
                        "registry: drop content-provider record stored under the wrong CID key"
                    );
                    continue;
                }
                Err(error) => {
                    debug!(
                        cid = %model_cid.to_hex(),
                        %error,
                        "registry: drop invalid content-provider record"
                    );
                    continue;
                }
            };

            match by_publisher.get(&signed.provider_pubkey) {
                Some(current) if signed.record.sequence < current.record.sequence => continue,
                Some(current) if signed.record.sequence == current.record.sequence => {
                    if signed.record != current.record {
                        bail!(
                            "content provider equivocated at sequence {} for CID {}",
                            signed.record.sequence,
                            model_cid.to_hex()
                        );
                    }
                    continue;
                }
                _ => {
                    by_publisher.insert(signed.provider_pubkey, signed);
                }
            }
        }

        let mut accepted = self.accepted_content_provider_records.write().await;
        let new_state_count = by_publisher
            .keys()
            .filter(|pubkey| !accepted.contains_key(&(*model_cid, **pubkey)))
            .count();
        if accepted
            .len()
            .checked_add(new_state_count)
            .is_none_or(|count| count > MAX_TRACKED_CONTENT_PROVIDER_RECORDS)
        {
            bail!(
                "content-provider replay state exceeds the {}-record limit",
                MAX_TRACKED_CONTENT_PROVIDER_RECORDS
            );
        }
        let mut candidates = Vec::with_capacity(by_publisher.len());
        for (pubkey, signed) in by_publisher {
            let fingerprint = content_provider_record_fingerprint(&signed.record)?;
            let state_key = (*model_cid, pubkey);
            if let Some(previous) = accepted.get(&state_key) {
                if signed.record.sequence < previous.sequence {
                    debug!(
                        cid = %model_cid.to_hex(),
                        sequence = signed.record.sequence,
                        "registry: drop rolled-back content-provider sequence"
                    );
                    continue;
                }
                if signed.record.sequence == previous.sequence
                    && fingerprint != previous.fingerprint
                {
                    bail!(
                        "content provider equivocated at sequence {} for CID {}",
                        signed.record.sequence,
                        model_cid.to_hex()
                    );
                }
            }

            accepted.insert(
                state_key,
                AcceptedContentProviderRecord {
                    sequence: signed.record.sequence,
                    fingerprint,
                },
            );
            candidates.push(ContentProviderCandidate {
                provider: peer_id_from_ed25519_pubkey(&pubkey)?,
                record: signed.record,
            });
        }
        candidates.sort_by_key(|candidate| candidate.provider.to_string());
        Ok(candidates)
    }

    /// Find peers advertising `model_cid` on the DHT. Returns one entry
    /// per valid, verified advertisement. Invalid signatures and
    /// unsupported schema versions are dropped silently (logged at
    /// `debug`).
    ///
    /// The `PeerId` is derived from the embedded Ed25519 pubkey — same
    /// derivation libp2p uses, so the returned `PeerId` is dial-able by
    /// any phase-net consumer.
    pub async fn find_peers_for_model(
        &self,
        model_cid: &ModelCid,
    ) -> Result<Vec<(PeerId, ModelCapabilities)>> {
        {
            let now = unix_ms_now();
            let cache = self.network_catalog.read().await;
            let mut candidates = cache
                .models
                .iter()
                .filter(|entry| {
                    entry.expires_at > now
                        && entry.model.caps.model_cid == *model_cid
                        && entry.model.caps.validate_at(now).is_ok()
                })
                .map(|entry| (entry.publisher, entry.model.caps.clone()))
                .collect::<Vec<_>>();
            // Preserve each publisher's own backend/precision/capacity claims
            // for routing and redundant-verification eligibility. The merged
            // model-list display summary is not a peer capability record.
            candidates.sort_by_key(|(peer, _)| *peer);
            if !candidates.is_empty() {
                return Ok(candidates);
            }
        }
        let key = model_cid.dht_key();
        let raw_records = self.transport.get_record(key).await?;
        let mut decoded_bytes = 0usize;
        let mut out =
            Vec::with_capacity(raw_records.len().min(MAX_ADVERTISEMENT_RECORDS_PER_QUERY));
        let now = unix_ms_now();
        for (record_index, record) in raw_records.into_iter().enumerate() {
            if record_index >= MAX_ADVERTISEMENT_RECORDS_PER_QUERY {
                debug!(
                    cid = %model_cid.to_hex(),
                    limit = MAX_ADVERTISEMENT_RECORDS_PER_QUERY,
                    "registry: model query record limit reached"
                );
                break;
            }
            if record.len() > MAX_ADVERTISEMENT_RECORD_BYTES {
                debug!(
                    cid = %model_cid.to_hex(),
                    record_bytes = record.len(),
                    "registry: drop oversized model advertisement"
                );
                continue;
            }
            let Some(next_decoded_bytes) = decoded_bytes.checked_add(record.len()) else {
                debug!(cid = %model_cid.to_hex(), "registry: model query byte count overflow");
                break;
            };
            if next_decoded_bytes > MAX_ADVERTISEMENT_DECODE_BYTES_PER_QUERY {
                debug!(
                    cid = %model_cid.to_hex(),
                    limit = MAX_ADVERTISEMENT_DECODE_BYTES_PER_QUERY,
                    "registry: model query decode-byte limit reached"
                );
                break;
            }
            decoded_bytes = next_decoded_bytes;

            match SignedModelAdvertisement::decode_at(&record, now) {
                Ok(ad) if ad.caps.model_cid != *model_cid => {
                    debug!(
                        queried_cid = %model_cid.to_hex(),
                        advertised_cid = %ad.caps.model_cid.to_hex(),
                        "registry: drop advertisement stored under the wrong CID key"
                    );
                }
                Ok(ad) => match peer_id_from_ed25519_pubkey(&ad.pubkey) {
                    Ok(peer_id) => out.push((peer_id, ad.caps)),
                    Err(e) => {
                        debug!("registry: drop record with bad pubkey: {e}");
                    }
                },
                Err(e) => {
                    debug!("registry: drop unverifiable record: {e}");
                }
            }
        }
        Ok(out)
    }

    /// Lookup by human-readable `model_id` rather than CID. The router
    /// uses this on `/api/chat` because Ollama clients name models by
    /// string, not by hash.
    ///
    /// Resolution order is a direct CID, local verified state, then the signed
    /// network alias index. A signed conflict is returned as an error; there is
    /// no name-derived production fallback.
    pub async fn find_peers_by_model_id(
        &self,
        model_id: &str,
    ) -> Result<Vec<(PeerId, ModelCapabilities)>> {
        let Some(cid) = self.resolve_model_cid(model_id).await? else {
            debug!(model = %model_id, "registry: no verified local or signed alias mapping");
            return Ok(Vec::new());
        };
        self.find_peers_for_model(&cid).await
    }
}

fn import_gguf_bytes(
    artifact_store: &ArtifactStore,
    source_root: &Path,
    source_path: &Path,
    verified_model_dir: &Path,
) -> Result<(ModelCid, u64)> {
    let canonical_root = source_root
        .canonicalize()
        .with_context(|| format!("canonicalize source model directory {source_root:?}"))?;
    let canonical_source = source_path
        .canonicalize()
        .with_context(|| format!("canonicalize source GGUF {source_path:?}"))?;
    if canonical_source.parent() != Some(canonical_root.as_path()) {
        bail!("source GGUF must be a direct child of the configured model directory");
    }
    if canonical_source
        .extension()
        .and_then(|value| value.to_str())
        != Some("gguf")
    {
        bail!("source model must have the .gguf extension");
    }
    let metadata = canonical_source
        .metadata()
        .with_context(|| format!("inspect source GGUF {canonical_source:?}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("source GGUF must be a non-empty regular file");
    }
    if metadata.len() > MAX_MODEL_SIZE_BYTES {
        bail!("source GGUF exceeds the supported model size");
    }

    let (blob_id, size_bytes) = ArtifactStore::compute_blob_id(&canonical_source)?;
    let installed =
        artifact_store.install_blob_from_path(&canonical_source, &blob_id, size_bytes)?;
    let model_cid = ModelCid::from_hex(blob_id.as_str())?;

    publish_worker_model_link(
        &installed.path,
        verified_model_dir,
        model_cid,
        &blob_id,
        size_bytes,
    )?;
    Ok((model_cid, size_bytes))
}

fn import_gguf_file_bytes(
    artifact_store: &ArtifactStore,
    source_root: &Path,
    source_path: &Path,
    verified_model_dir: &Path,
) -> Result<(ModelCid, u64)> {
    let canonical_root = source_root
        .canonicalize()
        .with_context(|| format!("canonicalize source root {source_root:?}"))?;
    let link_meta = std::fs::symlink_metadata(source_path)
        .with_context(|| format!("inspect source GGUF {source_path:?}"))?;
    if link_meta.file_type().is_symlink() {
        bail!("source GGUF must not be a symlink");
    }
    let canonical_source = source_path
        .canonicalize()
        .with_context(|| format!("canonicalize source GGUF {source_path:?}"))?;
    if !canonical_source.starts_with(&canonical_root) {
        bail!("source GGUF escapes its approved model root");
    }
    let metadata = canonical_source
        .metadata()
        .with_context(|| format!("inspect source GGUF {canonical_source:?}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("source GGUF must be a non-empty regular file");
    }
    if metadata.len() > MAX_MODEL_SIZE_BYTES {
        bail!("source GGUF exceeds the supported model size");
    }
    let has_gguf_ext = canonical_source
        .extension()
        .and_then(|value| value.to_str())
        == Some("gguf");
    if !has_gguf_ext && !crate::runtime_manager::has_gguf_magic(&canonical_source) {
        bail!("source model must have .gguf extension or valid GGUF header");
    }

    let (blob_id, size_bytes) = ArtifactStore::compute_blob_id(&canonical_source)?;
    let installed =
        artifact_store.install_blob_from_path(&canonical_source, &blob_id, size_bytes)?;
    let model_cid = ModelCid::from_hex(blob_id.as_str())?;

    publish_worker_model_link(
        &installed.path,
        verified_model_dir,
        model_cid,
        &blob_id,
        size_bytes,
    )?;
    Ok((model_cid, size_bytes))
}

fn publish_worker_model_link(
    installed_path: &Path,
    verified_model_dir: &Path,
    model_cid: ModelCid,
    blob_id: &BlobId,
    size_bytes: u64,
) -> Result<()> {
    fs::create_dir_all(verified_model_dir)
        .with_context(|| format!("create verified model directory {verified_model_dir:?}"))?;
    let worker_path = verified_model_dir.join(format!("{}.gguf", model_cid.to_hex()));
    match fs::hard_link(installed_path, &worker_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_worker_model_link(&worker_path, blob_id, size_bytes)?;
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "publish verified worker model {:?} from {:?}",
                    worker_path, installed_path
                )
            });
        }
    }
    verify_worker_model_link(&worker_path, blob_id, size_bytes)
}

fn verify_worker_model_link(path: &Path, expected_id: &BlobId, expected_size: u64) -> Result<()> {
    let (actual_id, actual_size) = ArtifactStore::compute_blob_id(path)?;
    if actual_size != expected_size || &actual_id != expected_id {
        bail!(
            "verified worker model mismatch at {:?}: expected {} bytes / {}, got {} bytes / {}",
            path,
            expected_size,
            expected_id,
            actual_size,
            actual_id
        );
    }
    Ok(())
}

impl Drop for ModelRegistry {
    fn drop(&mut self) {
        if let Ok(mut task) = self.catalog_refresh_task.try_lock() {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
        // Best-effort: abort all refresh tasks. We can't `await` the
        // mutex in `Drop`, but `try_lock` is sufficient here — if the
        // mutex is contended at drop time, the tokio runtime is going
        // away anyway and the tasks will be torn down with it.
        if let Ok(mut tasks) = self.refresh_tasks.try_lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        if let Ok(mut tasks) = self.alias_refresh_tasks.try_lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
        if let Ok(mut tasks) = self.provider_refresh_tasks.try_lock() {
            for (_, task) in tasks.drain() {
                task.abort();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_local_sequence<K>(sequences: &mut HashMap<K, u64>, key: K) -> Result<u64>
where
    K: Eq + std::hash::Hash,
{
    use std::collections::hash_map::Entry;

    let floor = unix_ms_now().max(1);
    match sequences.entry(key) {
        Entry::Occupied(mut entry) => {
            let next = entry
                .get()
                .checked_add(1)
                .context("local publication sequence exhausted")?
                .max(floor);
            entry.insert(next);
            Ok(next)
        }
        Entry::Vacant(entry) => {
            entry.insert(floor);
            Ok(floor)
        }
    }
}

fn content_provider_record_fingerprint(record: &ContentProviderRecord) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let bytes = postcard::to_allocvec(record).context("serialize content-provider fingerprint")?;
    let digest = Sha256::digest(bytes);
    let mut fingerprint = [0u8; 32];
    fingerprint.copy_from_slice(&digest);
    Ok(fingerprint)
}

fn alias_record_fingerprint(record: &AliasRecord) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let bytes = postcard::to_allocvec(record).context("serialize alias replay fingerprint")?;
    let mut hasher = Sha256::new();
    hasher.update(b"phase:alias-replay-fingerprint:v1\0");
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

fn load_alias_replay_state(
    path: &Path,
) -> Result<Option<HashMap<AliasSequenceKey, AcceptedAliasRecord>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => bail!("alias replay-state path is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect alias replay-state path"),
    };
    validate_alias_replay_permissions(&metadata)?;
    if metadata.len() > MAX_ALIAS_REPLAY_STATE_BYTES {
        bail!("alias replay-state file exceeds its byte limit");
    }

    let mut file = fs::File::open(path).context("open alias replay-state file")?;
    let opened_metadata = file
        .metadata()
        .context("inspect opened alias replay-state file")?;
    if !opened_metadata.file_type().is_file() || opened_metadata.len() != metadata.len() {
        bail!("alias replay-state file changed while opening");
    }
    validate_alias_replay_permissions(&opened_metadata)?;
    if !fs::symlink_metadata(path)
        .context("reinspect alias replay-state path")?
        .file_type()
        .is_file()
    {
        bail!("alias replay-state path changed while opening");
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_ALIAS_REPLAY_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read alias replay-state file")?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_ALIAS_REPLAY_STATE_BYTES {
        bail!("alias replay-state file changed or exceeded its byte limit while reading");
    }
    let state: AliasReplayStateFile =
        serde_json::from_slice(&bytes).context("decode alias replay-state file")?;
    if state.schema_version != ALIAS_REPLAY_STATE_SCHEMA_VERSION {
        bail!(
            "unsupported alias replay-state schema {}",
            state.schema_version
        );
    }
    if state.entries.len() > MAX_TRACKED_ALIAS_RECORDS {
        bail!("alias replay-state file exceeds its entry limit");
    }

    let mut accepted = HashMap::with_capacity(state.entries.len());
    for entry in state.entries {
        let normalized =
            normalize_model_alias(&entry.alias).context("validate alias replay-state alias")?;
        if normalized != entry.alias {
            bail!("alias replay-state alias is not in canonical normalized form");
        }
        if entry.sequence == 0 {
            bail!("alias replay-state sequence must be non-zero");
        }
        let publisher_pubkey =
            parse_canonical_hex_32("alias replay-state publisher key", &entry.publisher_pubkey)?;
        VerifyingKey::from_bytes(&publisher_pubkey)
            .context("decode alias replay-state publisher key")?;
        peer_id_from_ed25519_pubkey(&publisher_pubkey)
            .context("derive alias replay-state publisher PeerId")?;
        let fingerprint =
            parse_canonical_hex_32("alias replay-state fingerprint", &entry.fingerprint)?;
        if accepted
            .insert(
                (normalized, publisher_pubkey),
                AcceptedAliasRecord {
                    sequence: entry.sequence,
                    fingerprint,
                },
            )
            .is_some()
        {
            bail!("alias replay-state file contains a duplicate alias/publisher pair");
        }
    }
    Ok(Some(accepted))
}

fn write_alias_replay_state(
    path: &Path,
    accepted: &HashMap<AliasSequenceKey, AcceptedAliasRecord>,
) -> Result<()> {
    if accepted.len() > MAX_TRACKED_ALIAS_RECORDS {
        bail!("alias replay state exceeds its entry limit");
    }
    let parent = alias_replay_parent(path);
    ensure_alias_replay_parent(parent)?;
    reject_non_regular_alias_replay_path(path, true)?;

    let mut entries = accepted
        .iter()
        .map(
            |((alias, publisher_pubkey), record)| AliasReplayStateEntry {
                alias: alias.clone(),
                publisher_pubkey: encode_hex_32(publisher_pubkey),
                sequence: record.sequence,
                fingerprint: encode_hex_32(&record.fingerprint),
            },
        )
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.alias
            .cmp(&right.alias)
            .then_with(|| left.publisher_pubkey.cmp(&right.publisher_pubkey))
    });
    let bytes = serde_json::to_vec(&AliasReplayStateFile {
        schema_version: ALIAS_REPLAY_STATE_SCHEMA_VERSION,
        entries,
    })
    .context("serialize alias replay-state file")?;
    if bytes.len() as u64 > MAX_ALIAS_REPLAY_STATE_BYTES {
        bail!("alias replay-state file exceeds its byte limit");
    }

    let temporary_path = alias_replay_temporary_path(path);
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary_path)
            .with_context(|| format!("create alias replay-state file {temporary_path:?}"))?;
        file.write_all(&bytes)
            .context("write alias replay-state file")?;
        file.sync_all().context("sync alias replay-state file")?;
        validate_alias_replay_permissions(
            &file
                .metadata()
                .context("inspect alias replay-state temporary file")?,
        )?;
        drop(file);

        reject_non_regular_alias_replay_path(path, true)?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("publish alias replay-state file {path:?}"))?;
        sync_alias_replay_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn alias_replay_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_alias_replay_parent(parent: &Path) -> Result<()> {
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => bail!("alias replay-state parent is not a directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect alias replay-state parent"),
    }
    fs::create_dir_all(parent)
        .with_context(|| format!("create alias replay-state directory {parent:?}"))?;
    let metadata = fs::symlink_metadata(parent).context("inspect created replay-state parent")?;
    if !metadata.file_type().is_dir() {
        bail!("alias replay-state parent is not a directory");
    }
    Ok(())
}

fn reject_non_regular_alias_replay_path(path: &Path, absence_allowed: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            validate_alias_replay_permissions(&metadata)
        }
        Ok(_) => bail!("alias replay-state path is not a regular file"),
        Err(error) if absence_allowed && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("inspect alias replay-state path"),
    }
}

#[cfg(unix)]
fn validate_alias_replay_permissions(metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!("alias replay-state file permissions must not grant group or other access");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_alias_replay_permissions(_metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_alias_replay_directory(directory: &Path) -> Result<()> {
    fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .context("sync alias replay-state directory")
}

#[cfg(not(unix))]
fn sync_alias_replay_directory(_directory: &Path) -> Result<()> {
    Ok(())
}

fn alias_replay_temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "alias-replay-state".into());
    name.push(format!(".tmp-{}", uuid::Uuid::new_v4()));
    path.with_file_name(name)
}

fn parse_canonical_hex_32(field: &str, value: &str) -> Result<[u8; 32]> {
    if value != value.to_ascii_lowercase() {
        bail!("{field} is not canonical lowercase hexadecimal");
    }
    ModelCid::from_hex(value)
        .map(|cid| cid.0)
        .with_context(|| format!("parse {field}"))
}

fn encode_hex_32(value: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in value {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

/// Derive a libp2p `PeerId` from a raw Ed25519 public key (32 bytes).
///
/// libp2p's `PublicKey::try_decode_protobuf` would want a protobuf-wrapped
/// representation; here we build the public key directly through the
/// `ed25519` submodule, which accepts the raw 32 bytes. This matches the
/// derivation `phase_net::Discovery` uses on the inbound side.
pub(crate) fn peer_id_from_ed25519_pubkey(pubkey: &[u8; 32]) -> Result<PeerId> {
    use phase_net::libp2p_identity::{ed25519, PublicKey};
    let ed = ed25519::PublicKey::try_from_bytes(pubkey)
        .map_err(|e| anyhow!("decode ed25519 pubkey: {e}"))?;
    let pk: PublicKey = ed.into();
    Ok(PeerId::from(pk))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
