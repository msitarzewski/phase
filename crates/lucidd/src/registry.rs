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
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
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
#[async_trait]
pub trait DhtTransport: Send + Sync {
    /// Publish (or refresh) a record under `key`. Idempotent — calling
    /// twice with the same key/value updates the existing record.
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()>;

    /// Look up records under `key`. Returns the raw byte payloads that
    /// other peers have published — the registry decodes and verifies
    /// each one before returning it to a caller.
    async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>>;
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
}

impl ModelRegistry {
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
fn peer_id_from_ed25519_pubkey(pubkey: &[u8; 32]) -> Result<PeerId> {
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
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// In-memory DHT recorder. Captures every `put_record` and serves
    /// canned `get_record` responses. Replaces `Discovery` in unit tests.
    #[derive(Default)]
    struct MockTransport {
        /// Append-only log of every put. Indexed in test asserts.
        puts: StdMutex<Vec<(Vec<u8>, Vec<u8>)>>,
        /// Canned records returned by `get_record`. Indexed by key.
        store: StdMutex<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
        /// Number of upcoming content-provider puts that should fail before
        /// recording a value.
        fail_provider_puts: StdMutex<usize>,
    }

    impl MockTransport {
        fn put_count(&self) -> usize {
            self.puts.lock().unwrap().len()
        }
        fn last_put(&self) -> Option<(Vec<u8>, Vec<u8>)> {
            self.puts.lock().unwrap().last().cloned()
        }
        fn put_count_with_prefix(&self, prefix: &[u8]) -> usize {
            self.puts
                .lock()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.starts_with(prefix))
                .count()
        }
        fn values_with_prefix(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
            self.puts
                .lock()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.starts_with(prefix))
                .map(|(_, value)| value.clone())
                .collect()
        }
        fn fail_next_provider_puts(&self, count: usize) {
            *self.fail_provider_puts.lock().unwrap() = count;
        }
        fn pending_failed_provider_puts(&self) -> usize {
            *self.fail_provider_puts.lock().unwrap()
        }
        fn install_record(&self, key: Vec<u8>, value: Vec<u8>) {
            self.store
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push(value);
        }
        fn replace_records(&self, key: Vec<u8>, values: Vec<Vec<u8>>) {
            self.store.lock().unwrap().insert(key, values);
        }
    }

    #[async_trait]
    impl DhtTransport for MockTransport {
        async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
            let mut fail_provider_puts = self.fail_provider_puts.lock().unwrap();
            if key.starts_with(CONTENT_PROVIDER_KEY_PREFIX) && *fail_provider_puts > 0 {
                *fail_provider_puts -= 1;
                bail!("injected DHT put failure");
            }
            drop(fail_provider_puts);
            // Also mirror into the store so a subsequent get_record can
            // see what was published (useful for round-trip tests).
            self.store
                .lock()
                .unwrap()
                .entry(key.clone())
                .or_default()
                .push(value.clone());
            self.puts.lock().unwrap().push((key, value));
            Ok(())
        }
        async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
            Ok(self
                .store
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or_default())
        }
    }

    fn sample_caps() -> ModelCapabilities {
        ModelCapabilities::now(
            "qwen3-next-80b-q4",
            ModelCid([7u8; 32]),
            "Q4_K_M",
            32_768,
            4,
            "llama.cpp",
        )
    }

    fn sample_caps_at(now_ms: u64) -> ModelCapabilities {
        let mut caps = sample_caps();
        caps.advertised_at = now_ms;
        caps.valid_until = now_ms + ADVERTISEMENT_TTL.as_millis() as u64;
        caps
    }

    fn sample_provider_record(
        identity: &NodeIdentity,
        cid: ModelCid,
        sequence: u64,
    ) -> ContentProviderRecord {
        let provider = peer_id_from_ed25519_pubkey(&identity.verifying_key().to_bytes()).unwrap();
        ContentProviderRecord::new(cid, 8_192, "gguf", provider, sequence).unwrap()
    }

    fn sign_provider_semantically_unchecked(
        record: ContentProviderRecord,
        identity: &NodeIdentity,
    ) -> SignedContentProviderRecord {
        let provider_pubkey = identity.verifying_key().to_bytes();
        let bytes = SignedContentProviderRecord::canonical_signed_bytes(
            CONTENT_PROVIDER_SCHEMA_VERSION,
            &record,
            provider_pubkey,
        )
        .unwrap();
        SignedContentProviderRecord {
            schema_version: CONTENT_PROVIDER_SCHEMA_VERSION,
            record,
            provider_pubkey,
            signature: identity.signing_key().sign(&bytes).to_bytes().to_vec(),
        }
    }

    /// Construct a correctly signed envelope without calling the production
    /// semantic gate. This models a hostile publisher that intentionally signs
    /// invalid capability metadata, so receiver-side checks are exercised.
    fn sign_semantically_unchecked(
        caps: ModelCapabilities,
        identity: &NodeIdentity,
    ) -> SignedModelAdvertisement {
        let pubkey = identity.verifying_key().to_bytes();
        let bytes = SignedModelAdvertisement::canonical_signed_bytes(
            ADVERTISEMENT_SCHEMA_VERSION,
            &caps,
            pubkey,
        )
        .unwrap();
        SignedModelAdvertisement {
            schema_version: ADVERTISEMENT_SCHEMA_VERSION,
            caps,
            pubkey,
            signature: identity.signing_key().sign(&bytes).to_bytes().to_vec(),
        }
    }

    fn assert_semantically_rejected(caps: ModelCapabilities, now_ms: u64, expected_error: &str) {
        let identity = NodeIdentity::generate();
        let bytes = sign_semantically_unchecked(caps, &identity)
            .encode()
            .unwrap();
        let error = SignedModelAdvertisement::decode_at(&bytes, now_ms)
            .expect_err("signed invalid capabilities must be rejected");
        assert!(
            error.to_string().contains(expected_error),
            "expected error containing {expected_error:?}, got {error}"
        );
    }

    #[tokio::test]
    async fn advertise_emits_exactly_one_put() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity, transport.clone() as _);

        registry.advertise_loaded(sample_caps()).await.unwrap();

        assert_eq!(
            transport.put_count(),
            1,
            "advertise_loaded must publish exactly one record"
        );
        let (key, value) = transport.last_put().expect("a put happened");
        // Key has the right shape: prefix + 32-byte CID.
        assert!(key.starts_with(MODEL_KEY_PREFIX));
        assert_eq!(key.len(), MODEL_KEY_PREFIX.len() + 32);
        // Value decodes and verifies.
        let ad =
            SignedModelAdvertisement::decode(&value).expect("published value must decode + verify");
        assert_eq!(ad.caps.model_id, "qwen3-next-80b-q4");
        assert_eq!(ad.schema_version, ADVERTISEMENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn local_models_reflects_advertise_and_withdraw() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity, transport.clone() as _);

        assert!(registry.local_models_async().await.is_empty());

        let caps = sample_caps();
        let cid = caps.model_cid;
        registry.advertise_loaded(caps).await.unwrap();
        let local = registry.local_models_async().await;
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].model_id, "qwen3-next-80b-q4");

        registry.withdraw(&cid).await.unwrap();
        assert!(registry.local_models_async().await.is_empty());
    }

    /// Wait until `predicate` returns `true`, with the tokio test clock
    /// paused. Each iteration advances time by `step` and then sleeps
    /// for zero duration so the runtime gets a chance to poll spawned
    /// tasks (yielding alone is not enough — the timer wheel only
    /// re-arms when the runtime is actually re-entered).
    async fn wait_for<F: FnMut() -> bool>(
        mut predicate: F,
        step: Duration,
        max_iters: u32,
    ) -> bool {
        for _ in 0..max_iters {
            if predicate() {
                return true;
            }
            tokio::time::advance(step).await;
            // Sleeping for zero duration under a paused clock is the
            // documented way to let other tasks run. `yield_now` alone
            // doesn't pump the timer wheel.
            tokio::time::sleep(Duration::from_millis(0)).await;
        }
        predicate()
    }

    #[tokio::test(start_paused = true)]
    async fn withdraw_stops_refresh_task() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::with_refresh_interval(
            identity,
            transport.clone() as _,
            Duration::from_secs(60),
        );
        let caps = sample_caps();
        let cid = caps.model_cid;
        registry.advertise_loaded(caps).await.unwrap();
        assert_eq!(transport.put_count(), 1);

        // Drive the clock forward until the first refresh has landed.
        let saw_refresh =
            wait_for(|| transport.put_count() >= 2, Duration::from_secs(61), 10).await;
        assert!(
            saw_refresh,
            "expected refresh to publish; got {} puts",
            transport.put_count()
        );
        let after_one_refresh = transport.put_count();

        // Withdraw, then advance well past several more intervals. The
        // put count must not increase further.
        registry.withdraw(&cid).await.unwrap();
        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(60)).await;
            tokio::time::sleep(Duration::from_millis(0)).await;
        }
        assert_eq!(
            transport.put_count(),
            after_one_refresh,
            "withdraw must stop the refresh task"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ttl_refresh_re_publishes() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::with_refresh_interval(
            identity,
            transport.clone() as _,
            Duration::from_secs(60),
        );
        registry.advertise_loaded(sample_caps()).await.unwrap();

        // 1 initial + at least 3 refreshes.
        let reached = wait_for(|| transport.put_count() >= 4, Duration::from_secs(61), 20).await;
        assert!(
            reached,
            "expected >=4 puts (1 initial + 3 refreshes), got {}",
            transport.put_count()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn installed_alias_refreshes_without_provider_opt_in() {
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::with_refresh_interval(
            NodeIdentity::generate(),
            transport.clone() as _,
            Duration::from_secs(60),
        );
        let cid = ModelCid([0x24; 32]);
        registry
            .register_installed_content("refresh-alias", cid, "gguf", 4_096)
            .await
            .unwrap();

        assert_eq!(transport.put_count_with_prefix(ALIAS_KEY_PREFIX), 1);
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            0,
            "installation must not infer content serving"
        );
        let refreshed = wait_for(
            || transport.put_count_with_prefix(ALIAS_KEY_PREFIX) >= 4,
            Duration::from_secs(61),
            20,
        )
        .await;
        assert!(refreshed, "installed alias did not refresh");
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            0,
            "alias refresh must not start provider publication"
        );

        let records = transport
            .values_with_prefix(ALIAS_KEY_PREFIX)
            .into_iter()
            .map(|bytes| SignedAliasRecord::decode_at(&bytes, unix_ms_now()).unwrap())
            .collect::<Vec<_>>();
        assert!(records
            .windows(2)
            .all(|pair| pair[0].record.sequence < pair[1].record.sequence));
        assert!(records.iter().all(|signed| {
            signed.record.alias == "refresh-alias" && signed.record.model_cid == cid
        }));

        registry
            .withdraw_installed_content("refresh-alias")
            .await
            .unwrap();
        let puts_after_withdrawal = transport.put_count_with_prefix(ALIAS_KEY_PREFIX);
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::time::sleep(Duration::ZERO).await;
        }
        assert_eq!(
            transport.put_count_with_prefix(ALIAS_KEY_PREFIX),
            puts_after_withdrawal,
            "catalog withdrawal must stop alias refresh"
        );
        assert!(registry.local_installed_async().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn provider_opt_in_refreshes_retries_and_withdraws_monotonically() {
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::with_refresh_interval(
            NodeIdentity::generate(),
            transport.clone() as _,
            Duration::from_secs(60),
        );
        let cid = ModelCid([0x25; 32]);
        registry
            .register_installed_content("provider-refresh", cid, "gguf", 8_192)
            .await
            .unwrap();

        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::time::sleep(Duration::ZERO).await;
        }
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            0,
            "provider refresh must not exist before explicit opt-in"
        );

        registry
            .publish_installed_content_provider(&cid)
            .await
            .unwrap();
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            1
        );

        transport.fail_next_provider_puts(1);
        let observed_failure = wait_for(
            || transport.pending_failed_provider_puts() == 0,
            Duration::from_secs(61),
            10,
        )
        .await;
        assert!(
            observed_failure,
            "provider refresh did not attempt the injected failure"
        );
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            1,
            "a failed refresh must not be recorded as successful"
        );
        let retried = wait_for(
            || transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX) >= 2,
            Duration::from_secs(61),
            10,
        )
        .await;
        assert!(retried, "provider refresh did not retry after failure");

        let before_replace = transport
            .values_with_prefix(CONTENT_PROVIDER_KEY_PREFIX)
            .last()
            .map(|bytes| {
                SignedContentProviderRecord::decode_at(bytes, unix_ms_now())
                    .unwrap()
                    .record
                    .sequence
            })
            .unwrap();
        registry
            .publish_installed_content_provider(&cid)
            .await
            .unwrap();
        let records = transport
            .values_with_prefix(CONTENT_PROVIDER_KEY_PREFIX)
            .into_iter()
            .map(|bytes| SignedContentProviderRecord::decode_at(&bytes, unix_ms_now()).unwrap())
            .collect::<Vec<_>>();
        assert!(records.last().unwrap().record.sequence > before_replace);
        assert!(records
            .windows(2)
            .all(|pair| pair[0].record.sequence < pair[1].record.sequence));

        registry.withdraw_content_provider(&cid).await.unwrap();
        let puts_after_withdrawal = transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX);
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::time::sleep(Duration::ZERO).await;
        }
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            puts_after_withdrawal,
            "provider withdrawal must stop retries and refreshes"
        );
        assert_eq!(
            registry.local_installed_async().await.len(),
            1,
            "provider withdrawal must not uninstall content"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drop_stops_alias_and_provider_refresh_tasks() {
        let transport = Arc::new(MockTransport::default());
        {
            let registry = ModelRegistry::with_refresh_interval(
                NodeIdentity::generate(),
                transport.clone() as _,
                Duration::from_secs(60),
            );
            let cid = ModelCid([0x26; 32]);
            registry
                .register_installed_content("drop-refresh", cid, "gguf", 8_192)
                .await
                .unwrap();
            registry
                .publish_installed_content_provider(&cid)
                .await
                .unwrap();
            assert_eq!(transport.put_count_with_prefix(ALIAS_KEY_PREFIX), 1);
            assert_eq!(
                transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
                1
            );
        }

        let alias_puts = transport.put_count_with_prefix(ALIAS_KEY_PREFIX);
        let provider_puts = transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX);
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(61)).await;
            tokio::time::sleep(Duration::ZERO).await;
        }
        assert_eq!(
            transport.put_count_with_prefix(ALIAS_KEY_PREFIX),
            alias_puts
        );
        assert_eq!(
            transport.put_count_with_prefix(CONTENT_PROVIDER_KEY_PREFIX),
            provider_puts
        );
    }

    #[test]
    fn signed_advertisement_round_trips_and_verifies() {
        let identity = NodeIdentity::generate();
        let caps = sample_caps();
        let ad = SignedModelAdvertisement::sign(caps.clone(), &identity).unwrap();
        // Pubkey on the envelope matches the identity.
        assert_eq!(ad.pubkey, identity.verifying_key().to_bytes());
        // Encode → decode → verify.
        let bytes = ad.encode().unwrap();
        let back = SignedModelAdvertisement::decode(&bytes).unwrap();
        assert_eq!(back.caps, caps);
        assert_eq!(back.pubkey, identity.verifying_key().to_bytes());
    }

    #[test]
    fn semantic_advertisement_validation_accepts_valid_exact_record() {
        let now = 1_800_000_000_000;
        let identity = NodeIdentity::generate();
        let ad = sign_semantically_unchecked(sample_caps_at(now), &identity);
        let bytes = ad.encode().unwrap();

        let decoded = SignedModelAdvertisement::decode_at(&bytes, now).unwrap();
        assert_eq!(decoded.caps, sample_caps_at(now));

        let mut trailing = bytes;
        trailing.push(0);
        let error = SignedModelAdvertisement::decode_at(&trailing, now)
            .expect_err("trailing bytes must not be accepted as the exact schema");
        assert!(error.to_string().contains("trailing bytes"));
    }

    #[test]
    fn semantic_advertisement_validation_rejects_invalid_time_windows() {
        let now = 1_800_000_000_000;

        let mut expired = sample_caps_at(now);
        expired.advertised_at = now - ADVERTISEMENT_TTL.as_millis() as u64;
        expired.valid_until = now;
        assert_semantically_rejected(expired, now, "expired");

        let mut future = sample_caps_at(now);
        future.advertised_at = now + MAX_ADVERTISEMENT_FUTURE_SKEW.as_millis() as u64 + 1;
        future.valid_until = future.advertised_at + ADVERTISEMENT_TTL.as_millis() as u64;
        assert_semantically_rejected(future, now, "too far in the future");

        let mut overlong = sample_caps_at(now);
        overlong.valid_until = overlong.advertised_at + ADVERTISEMENT_TTL.as_millis() as u64 + 1;
        assert_semantically_rejected(overlong, now, "TTL exceeds");
    }

    #[test]
    fn semantic_advertisement_validation_rejects_zero_capacity_and_context() {
        let now = 1_800_000_000_000;

        let mut zero_capacity = sample_caps_at(now);
        zero_capacity.max_concurrent = 0;
        assert_semantically_rejected(zero_capacity, now, "capacity must be non-zero");

        let mut zero_context = sample_caps_at(now);
        zero_context.context_length = 0;
        assert_semantically_rejected(zero_context, now, "context length must be non-zero");
    }

    #[test]
    fn semantic_advertisement_validation_rejects_noncanonical_metadata() {
        let now = 1_800_000_000_000;

        let mut zero_cid = sample_caps_at(now);
        zero_cid.model_cid = ModelCid([0; 32]);
        assert_semantically_rejected(zero_cid, now, "CID cannot be all zeroes");

        let mut model_id = sample_caps_at(now);
        model_id.model_id = "Qwen3".to_string();
        assert_semantically_rejected(model_id, now, "model_id is not in canonical");

        let mut format = sample_caps_at(now);
        format.quantization = "Q4 K M".to_string();
        assert_semantically_rejected(format, now, "quantization/format");

        let mut backend = sample_caps_at(now);
        backend.backend = "LLAMA.CPP".to_string();
        assert_semantically_rejected(backend, now, "backend is not in canonical lowercase");
    }

    #[test]
    fn tamper_with_caps_breaks_signature() {
        let identity = NodeIdentity::generate();
        let mut ad = SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        // Mutate a field after signing — verification must fail.
        ad.caps.context_length = ad.caps.context_length.wrapping_add(1);
        let err = ad.verify().expect_err("tampered caps must fail verify");
        let msg = format!("{err}");
        assert!(
            msg.contains("signature"),
            "error should mention signature, got: {msg}"
        );
    }

    #[test]
    fn tamper_with_pubkey_breaks_signature() {
        let identity = NodeIdentity::generate();
        let mut ad = SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        // Flip one byte of the embedded pubkey — postcard round-trips it
        // fine, but the signature was bound to the original pubkey.
        ad.pubkey[0] ^= 0x01;
        let err = ad.verify().expect_err("tampered pubkey must fail verify");
        let msg = format!("{err}");
        assert!(
            msg.contains("signature") || msg.contains("decode"),
            "error should mention signature or decode, got: {msg}"
        );
    }

    #[test]
    fn tamper_with_signature_bytes_breaks_verify() {
        let identity = NodeIdentity::generate();
        let mut ad = SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        ad.signature[0] ^= 0xff;
        assert!(ad.verify().is_err());
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let identity = NodeIdentity::generate();
        let mut ad = SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        ad.schema_version = ADVERTISEMENT_SCHEMA_VERSION + 1;
        let err = ad.verify().expect_err("bumped schema must fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("schema") || msg.contains("signature"),
            "error should mention schema or signature, got: {msg}"
        );
    }

    #[tokio::test]
    async fn find_peers_returns_round_trip_record() {
        // Self-publish, then look up; this exercises the put → get →
        // decode → derive-peer-id path end-to-end against the mock.
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity.clone(), transport.clone() as _);
        let caps = sample_caps();
        let cid = caps.model_cid;
        registry.advertise_loaded(caps.clone()).await.unwrap();

        let peers = registry.find_peers_for_model(&cid).await.unwrap();
        assert_eq!(peers.len(), 1, "should find the record we just published");
        assert_eq!(peers[0].1.model_id, caps.model_id);
        // PeerId derives from the same pubkey we signed with — sanity-
        // check by re-deriving and comparing.
        let expected = peer_id_from_ed25519_pubkey(&identity.verifying_key().to_bytes()).unwrap();
        assert_eq!(peers[0].0, expected);
    }

    #[tokio::test]
    async fn find_peers_drops_unverifiable_records() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity, transport.clone() as _);
        let cid = ModelCid([9u8; 32]);

        // Install a garbage record under the key — it must be filtered
        // out, not returned to the caller.
        transport.install_record(cid.dht_key(), b"not-a-valid-postcard-record".to_vec());

        let peers = registry.find_peers_for_model(&cid).await.unwrap();
        assert!(peers.is_empty(), "garbage records must be discarded");
    }

    #[tokio::test]
    async fn find_peers_rejects_valid_advertisement_under_wrong_cid_key() {
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let advertised_cid = ModelCid([9u8; 32]);
        let queried_cid = ModelCid([8u8; 32]);
        let mut caps = sample_caps();
        caps.model_cid = advertised_cid;
        let record = SignedModelAdvertisement::sign(caps, &publisher)
            .unwrap()
            .encode()
            .unwrap();
        transport.install_record(queried_cid.dht_key(), record);

        let peers = registry.find_peers_for_model(&queried_cid).await.unwrap();
        assert!(
            peers.is_empty(),
            "a valid signature must not override the queried CID binding"
        );
    }

    #[tokio::test]
    async fn find_peers_bounds_records_processed_per_query() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let cid = ModelCid([9u8; 32]);
        let mut records = vec![b"invalid".to_vec(); MAX_ADVERTISEMENT_RECORDS_PER_QUERY];
        let mut caps = sample_caps();
        caps.model_cid = cid;
        records.push(
            SignedModelAdvertisement::sign(caps, &identity)
                .unwrap()
                .encode()
                .unwrap(),
        );
        transport.replace_records(cid.dht_key(), records);

        let peers = registry.find_peers_for_model(&cid).await.unwrap();
        assert!(
            peers.is_empty(),
            "records beyond the per-query processing cap must not be decoded"
        );
    }

    #[tokio::test]
    async fn find_peers_bounds_total_decoded_bytes_per_query() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let cid = ModelCid([9u8; 32]);
        let invalid_record_bytes = 8 * 1024;
        let records_at_byte_limit = MAX_ADVERTISEMENT_DECODE_BYTES_PER_QUERY / invalid_record_bytes;
        assert!(records_at_byte_limit < MAX_ADVERTISEMENT_RECORDS_PER_QUERY);
        let mut records = vec![vec![0; invalid_record_bytes]; records_at_byte_limit];
        let mut caps = sample_caps();
        caps.model_cid = cid;
        records.push(
            SignedModelAdvertisement::sign(caps, &identity)
                .unwrap()
                .encode()
                .unwrap(),
        );
        transport.replace_records(cid.dht_key(), records);

        let peers = registry.find_peers_for_model(&cid).await.unwrap();
        assert!(
            peers.is_empty(),
            "records beyond the aggregate decode-byte cap must not be decoded"
        );
    }

    #[tokio::test]
    async fn find_peers_by_model_id_uses_local_name_map() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity, transport.clone() as _);
        let caps = sample_caps();
        registry.advertise_loaded(caps.clone()).await.unwrap();

        let peers = registry
            .find_peers_by_model_id(&caps.model_id)
            .await
            .unwrap();
        assert_eq!(peers.len(), 1);
        // Unknown model id → empty result, not an error.
        let none = registry
            .find_peers_by_model_id("no-such-model")
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn model_cid_dht_key_layout() {
        let cid = ModelCid([0xab; 32]);
        let key = cid.dht_key();
        assert_eq!(key.len(), 44);
        assert_eq!(&key[..12], b"phase/model/");
        assert_eq!(&key[12..], &[0xab; 32]);
    }

    #[test]
    fn model_cid_hex_round_trip_is_strict() {
        let cid = ModelCid([0x5a; 32]);
        assert_eq!(ModelCid::from_hex(&cid.to_hex()).unwrap(), cid);
        assert_eq!(
            ModelCid::from_hex(&cid.to_hex().to_ascii_uppercase()).unwrap(),
            cid
        );
        assert!(ModelCid::from_hex("abc").is_err());
        assert!(ModelCid::from_hex(&"z".repeat(64)).is_err());
    }

    #[test]
    fn alias_normalization_rejects_ambiguous_or_path_shaped_names() {
        assert_eq!(
            normalize_model_alias("Qwen3:Q4_K_M").unwrap(),
            "qwen3:q4_k_m"
        );
        for invalid in [
            "",
            "-flag",
            "../model",
            "model/path",
            "model name",
            "ｍodel",
        ] {
            assert!(
                normalize_model_alias(invalid).is_err(),
                "alias should be rejected: {invalid:?}"
            );
        }
        let key = alias_dht_key("QWEN3").unwrap();
        assert_eq!(key, alias_dht_key("qwen3").unwrap());
        assert_eq!(key.len(), ALIAS_KEY_PREFIX.len() + 32);
    }

    #[test]
    fn signed_alias_binds_metadata_and_expiry() {
        let identity = NodeIdentity::generate();
        let record = AliasRecord::new("qwen3", ModelCid([3; 32]), "gguf", 4096, 1).unwrap();
        let signed = SignedAliasRecord::sign(record, &identity).unwrap();
        let bytes = signed.encode().unwrap();
        let decoded = SignedAliasRecord::decode_at(&bytes, unix_ms_now()).unwrap();
        assert_eq!(decoded.record.model_cid, ModelCid([3; 32]));
        assert!(decoded.verify_at(decoded.record.valid_until).is_err());

        let mut tampered = decoded;
        tampered.record.size_bytes += 1;
        assert!(tampered.verify_at(unix_ms_now()).is_err());

        let mut trailing = bytes;
        trailing.push(0);
        assert!(SignedAliasRecord::decode_at(&trailing, unix_ms_now()).is_err());
    }

    fn signed_alias_bytes(
        identity: &NodeIdentity,
        alias: &str,
        cid: ModelCid,
        sequence: u64,
    ) -> Vec<u8> {
        SignedAliasRecord::sign(
            AliasRecord::new(alias, cid, "gguf", 8_192, sequence).unwrap(),
            identity,
        )
        .unwrap()
        .encode()
        .unwrap()
    }

    #[tokio::test]
    async fn alias_query_caps_records_bytes_and_individual_payloads_before_decode() {
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let key = alias_dht_key("bounded").unwrap();
        let valid = signed_alias_bytes(&publisher, "bounded", ModelCid([0x41; 32]), 1);

        let mut record_flood = vec![vec![0]; MAX_ALIAS_RECORDS_PER_QUERY];
        record_flood.push(valid.clone());
        transport.replace_records(key.clone(), record_flood);
        assert!(registry.resolve_alias("bounded").await.unwrap().is_empty());

        let records_before_byte_limit = MAX_ALIAS_DECODE_BYTES_PER_QUERY / 2_048;
        let mut byte_flood = vec![vec![0; 2_048]; records_before_byte_limit + 1];
        byte_flood.push(valid.clone());
        transport.replace_records(key.clone(), byte_flood);
        assert!(registry.resolve_alias("bounded").await.unwrap().is_empty());

        transport.replace_records(key, vec![vec![0; MAX_ALIAS_RECORD_BYTES + 1], valid]);
        assert_eq!(registry.resolve_alias("bounded").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn alias_replay_state_has_a_hard_entry_cap() {
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        {
            let mut accepted = registry.accepted_alias_records.write().await;
            for index in 0..MAX_TRACKED_ALIAS_RECORDS {
                accepted.insert(
                    (format!("cap-{index}"), [index as u8; 32]),
                    AcceptedAliasRecord {
                        sequence: 1,
                        fingerprint: [index.wrapping_add(1) as u8; 32],
                    },
                );
            }
        }
        transport.replace_records(
            alias_dht_key("overflow").unwrap(),
            vec![signed_alias_bytes(
                &publisher,
                "overflow",
                ModelCid([0x42; 32]),
                1,
            )],
        );

        let error = registry
            .resolve_alias("overflow")
            .await
            .expect_err("a new publisher must not grow replay state beyond its cap");
        assert!(error.to_string().contains("replay state exceeds"));
        assert_eq!(
            registry.accepted_alias_records.read().await.len(),
            MAX_TRACKED_ALIAS_RECORDS
        );
    }

    #[tokio::test]
    async fn durable_alias_replay_rejects_rollback_and_equivocation_after_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alias-replay.json");
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let key = alias_dht_key("durable").unwrap();
        transport.replace_records(
            key.clone(),
            vec![signed_alias_bytes(
                &publisher,
                "durable",
                ModelCid([0x51; 32]),
                2,
            )],
        );
        let registry = ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();
        assert_eq!(registry.resolve_alias("durable").await.unwrap().len(), 1);
        drop(registry);

        let reopened = ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();
        transport.replace_records(
            key.clone(),
            vec![signed_alias_bytes(
                &publisher,
                "durable",
                ModelCid([0x50; 32]),
                1,
            )],
        );
        assert!(reopened.resolve_alias("durable").await.unwrap().is_empty());

        transport.replace_records(
            key,
            vec![signed_alias_bytes(
                &publisher,
                "durable",
                ModelCid([0x52; 32]),
                2,
            )],
        );
        let error = reopened
            .resolve_alias("durable")
            .await
            .expect_err("same-sequence equivocation must survive restart");
        assert!(error.to_string().contains("equivocated"));
    }

    #[tokio::test]
    async fn alias_replay_checkpoint_is_atomic_and_committed_before_resolution_returns() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state").join("alias-replay.json");
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let key = alias_dht_key("atomic").unwrap();
        let registry = ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();

        transport.replace_records(
            key.clone(),
            vec![signed_alias_bytes(
                &publisher,
                "atomic",
                ModelCid([0x61; 32]),
                1,
            )],
        );
        assert_eq!(registry.resolve_alias("atomic").await.unwrap().len(), 1);
        let first = load_alias_replay_state(&path).unwrap().unwrap();
        assert_eq!(first.values().next().unwrap().sequence, 1);

        transport.replace_records(
            key,
            vec![signed_alias_bytes(
                &publisher,
                "atomic",
                ModelCid([0x62; 32]),
                2,
            )],
        );
        assert_eq!(registry.resolve_alias("atomic").await.unwrap().len(), 1);
        let second = load_alias_replay_state(&path).unwrap().unwrap();
        assert_eq!(second.values().next().unwrap().sequence, 2);
        assert_ne!(
            first.values().next().unwrap().fingerprint,
            second.values().next().unwrap().fingerprint
        );
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "atomic replacement must not leave temporary files"
        );
    }

    #[tokio::test]
    async fn failed_alias_replay_checkpoint_does_not_expose_in_memory_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("state");
        let path = parent.join("alias-replay.json");
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&parent).unwrap();
        fs::write(&parent, b"not-a-directory").unwrap();
        transport.replace_records(
            alias_dht_key("persist-fail").unwrap(),
            vec![signed_alias_bytes(
                &publisher,
                "persist-fail",
                ModelCid([0x63; 32]),
                1,
            )],
        );

        assert!(registry.resolve_alias("persist-fail").await.is_err());
        assert!(registry.accepted_alias_records.read().await.is_empty());
    }

    #[test]
    fn alias_replay_load_rejects_corruption_unknown_fields_and_partial_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alias-replay.json");
        let transport = Arc::new(MockTransport::default());
        ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();

        fs::write(
            &path,
            br#"{"schema_version":1,"entries":[],"unknown":true}"#,
        )
        .unwrap();
        assert!(ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .is_err());

        let publisher = NodeIdentity::generate().verifying_key().to_bytes();
        let entries = serde_json::json!({
            "schema_version": ALIAS_REPLAY_STATE_SCHEMA_VERSION,
            "entries": [
                {
                    "alias": "valid",
                    "publisher_pubkey": encode_hex_32(&publisher),
                    "sequence": 1,
                    "fingerprint": encode_hex_32(&[1; 32])
                },
                {
                    "alias": "../invalid",
                    "publisher_pubkey": encode_hex_32(&publisher),
                    "sequence": 2,
                    "fingerprint": encode_hex_32(&[2; 32])
                }
            ]
        });
        fs::write(&path, serde_json::to_vec(&entries).unwrap()).unwrap();
        assert!(load_alias_replay_state(&path).is_err());
    }

    #[test]
    fn alias_replay_load_enforces_file_and_entry_caps_before_install() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alias-replay.json");
        let transport = Arc::new(MockTransport::default());
        ModelRegistry::new_with_alias_replay_state(NodeIdentity::generate(), transport, &path)
            .unwrap();

        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_ALIAS_REPLAY_STATE_BYTES + 1)
            .unwrap();
        assert!(load_alias_replay_state(&path).is_err());

        let publisher = NodeIdentity::generate().verifying_key().to_bytes();
        let entries = (0..=MAX_TRACKED_ALIAS_RECORDS)
            .map(|index| AliasReplayStateEntry {
                alias: format!("bounded-{index}"),
                publisher_pubkey: encode_hex_32(&publisher),
                sequence: 1,
                fingerprint: encode_hex_32(&[index as u8; 32]),
            })
            .collect();
        let bytes = serde_json::to_vec(&AliasReplayStateFile {
            schema_version: ALIAS_REPLAY_STATE_SCHEMA_VERSION,
            entries,
        })
        .unwrap();
        assert!(bytes.len() as u64 <= MAX_ALIAS_REPLAY_STATE_BYTES);
        fs::write(&path, bytes).unwrap();
        assert!(load_alias_replay_state(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn alias_replay_state_is_private_and_rejects_unsafe_paths() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("alias-replay.json");
        let transport = Arc::new(MockTransport::default());
        ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport.clone() as _,
            &path,
        )
        .is_err());

        let target = temp.path().join("target.json");
        fs::write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("alias-replay-link.json");
        symlink(&target, &link).unwrap();
        assert!(ModelRegistry::new_with_alias_replay_state(
            NodeIdentity::generate(),
            transport as _,
            &link,
        )
        .is_err());
    }

    #[tokio::test]
    async fn signed_alias_drives_name_to_content_cid_lookup() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let publisher = ModelRegistry::new(identity, transport.clone() as _);
        let cid = ModelCid([0x44; 32]);
        publisher
            .publish_alias(AliasRecord::new("remote-model", cid, "gguf", 8192, 1).unwrap())
            .await
            .unwrap();
        publisher
            .advertise_loaded(ModelCapabilities::now(
                "remote-model",
                cid,
                "Q4_K_M",
                8192,
                1,
                "llama.cpp",
            ))
            .await
            .unwrap();

        let consumer = ModelRegistry::new(NodeIdentity::generate(), transport as _);
        let resolved = consumer.resolve_alias("REMOTE-MODEL").await.unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].record.model_cid, cid);
        let peers = consumer
            .find_peers_by_model_id("remote-model")
            .await
            .unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].1.model_cid, cid);
    }

    #[tokio::test]
    async fn conflicting_publishers_are_not_selected_by_arrival_order() {
        let transport = Arc::new(MockTransport::default());
        let first = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let second = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        first
            .publish_alias(
                AliasRecord::new("conflict", ModelCid([1; 32]), "gguf", 1024, 1).unwrap(),
            )
            .await
            .unwrap();
        second
            .publish_alias(
                AliasRecord::new("conflict", ModelCid([2; 32]), "gguf", 1024, 1).unwrap(),
            )
            .await
            .unwrap();

        let consumer = ModelRegistry::new(NodeIdentity::generate(), transport as _);
        let error = consumer
            .resolve_alias_cid("conflict")
            .await
            .expect_err("conflicting signed mappings must remain visible");
        assert!(error.to_string().contains("conflicting signed mappings"));
    }

    #[tokio::test]
    async fn lower_sequence_replay_is_rejected_after_newer_record() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let consumer = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let key = alias_dht_key("rollback").unwrap();

        let newer = SignedAliasRecord::sign(
            AliasRecord::new("rollback", ModelCid([9; 32]), "gguf", 1024, 2).unwrap(),
            &identity,
        )
        .unwrap()
        .encode()
        .unwrap();
        transport.replace_records(key.clone(), vec![newer]);
        assert_eq!(consumer.resolve_alias("rollback").await.unwrap().len(), 1);

        let older = SignedAliasRecord::sign(
            AliasRecord::new("rollback", ModelCid([8; 32]), "gguf", 1024, 1).unwrap(),
            &identity,
        )
        .unwrap()
        .encode()
        .unwrap();
        transport.replace_records(key, vec![older]);
        assert!(consumer.resolve_alias("rollback").await.unwrap().is_empty());
    }

    #[test]
    fn content_provider_key_and_signed_payload_bind_exact_metadata() {
        let identity = NodeIdentity::generate();
        let cid = ModelCid([0x31; 32]);
        let record = sample_provider_record(&identity, cid, 17);
        let signed = SignedContentProviderRecord::sign(record.clone(), &identity).unwrap();
        let bytes = signed.encode().unwrap();
        let decoded = SignedContentProviderRecord::decode_at(&bytes, unix_ms_now()).unwrap();

        assert_eq!(decoded.record.model_cid, cid);
        assert_eq!(decoded.record.size_bytes, 8_192);
        assert_eq!(decoded.record.format, "gguf");
        assert_eq!(decoded.record.provider_peer_id, record.provider_peer_id);
        assert_eq!(decoded.record.sequence, 17);
        assert_eq!(
            decoded.record.valid_until - decoded.record.issued_at,
            MAX_CONTENT_PROVIDER_TTL.as_millis() as u64
        );
        assert!(decoded.verify_at(decoded.record.valid_until).is_err());

        let key = content_provider_dht_key(&cid);
        assert!(key.starts_with(CONTENT_PROVIDER_KEY_PREFIX));
        assert_eq!(&key[CONTENT_PROVIDER_KEY_PREFIX.len()..], cid.0.as_slice());
        assert_ne!(key, cid.dht_key());

        let mut tampered = decoded;
        tampered.record.size_bytes += 1;
        assert!(tampered.verify_at(unix_ms_now()).is_err());
    }

    #[tokio::test]
    async fn provider_query_filters_invalid_records_and_is_deterministic() {
        let first = NodeIdentity::generate();
        let second = NodeIdentity::generate();
        let wrong_identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let cid = ModelCid([0x41; 32]);
        let key = content_provider_dht_key(&cid);

        let valid_first =
            SignedContentProviderRecord::sign(sample_provider_record(&first, cid, 2), &first)
                .unwrap()
                .encode()
                .unwrap();
        let valid_second =
            SignedContentProviderRecord::sign(sample_provider_record(&second, cid, 3), &second)
                .unwrap()
                .encode()
                .unwrap();

        let wrong_cid = SignedContentProviderRecord::sign(
            sample_provider_record(&first, ModelCid([0x42; 32]), 4),
            &first,
        )
        .unwrap()
        .encode()
        .unwrap();

        let now = unix_ms_now();
        let mut expired = sample_provider_record(&first, cid, 5);
        expired.issued_at = now.saturating_sub(2_000);
        expired.valid_until = now.saturating_sub(1_000);
        let expired = sign_provider_semantically_unchecked(expired, &first)
            .encode()
            .unwrap();

        let mut impossible_size = sample_provider_record(&first, cid, 6);
        impossible_size.size_bytes = MAX_MODEL_SIZE_BYTES + 1;
        let impossible_size = sign_provider_semantically_unchecked(impossible_size, &first)
            .encode()
            .unwrap();

        let mut wrong_provider = sample_provider_record(&first, cid, 7);
        wrong_provider.provider_peer_id =
            peer_id_from_ed25519_pubkey(&wrong_identity.verifying_key().to_bytes())
                .unwrap()
                .to_string();
        let wrong_provider = sign_provider_semantically_unchecked(wrong_provider, &first)
            .encode()
            .unwrap();

        transport.replace_records(
            key,
            vec![
                valid_second,
                b"malformed".to_vec(),
                wrong_cid,
                expired,
                impossible_size,
                wrong_provider,
                valid_first,
            ],
        );

        let candidates = registry.find_content_providers(&cid).await.unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .windows(2)
            .all(|pair| pair[0].provider.to_string() < pair[1].provider.to_string()));
        assert!(candidates.iter().all(|candidate| {
            candidate.record.model_cid == cid
                && candidate.record.size_bytes == 8_192
                && candidate.record.format == "gguf"
                && candidate.provider.to_string() == candidate.record.provider_peer_id
        }));
    }

    #[tokio::test]
    async fn provider_query_rejects_equivocation_and_rollback() {
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let cid = ModelCid([0x51; 32]);
        let key = content_provider_dht_key(&cid);

        let newer_record = sample_provider_record(&publisher, cid, 10);
        let newer = SignedContentProviderRecord::sign(newer_record.clone(), &publisher)
            .unwrap()
            .encode()
            .unwrap();
        transport.replace_records(key.clone(), vec![newer]);
        assert_eq!(
            registry.find_content_providers(&cid).await.unwrap().len(),
            1
        );

        let older = SignedContentProviderRecord::sign(
            sample_provider_record(&publisher, cid, 9),
            &publisher,
        )
        .unwrap()
        .encode()
        .unwrap();
        transport.replace_records(key.clone(), vec![older]);
        assert!(registry
            .find_content_providers(&cid)
            .await
            .unwrap()
            .is_empty());

        let mut changed = newer_record;
        changed.size_bytes += 1;
        let changed = SignedContentProviderRecord::sign(changed, &publisher)
            .unwrap()
            .encode()
            .unwrap();
        transport.replace_records(key.clone(), vec![changed]);
        let error = registry
            .find_content_providers(&cid)
            .await
            .expect_err("same sequence with changed metadata must be equivocation");
        assert!(error.to_string().contains("equivocated"));

        let first = SignedContentProviderRecord::sign(
            sample_provider_record(&publisher, cid, 11),
            &publisher,
        )
        .unwrap();
        let mut conflicting_record = first.record.clone();
        conflicting_record.format = "safetensors".to_string();
        let conflicting =
            SignedContentProviderRecord::sign(conflicting_record, &publisher).unwrap();
        transport.replace_records(
            key,
            vec![first.encode().unwrap(), conflicting.encode().unwrap()],
        );
        let error = registry
            .find_content_providers(&cid)
            .await
            .expect_err("same-query equivocation must fail closed");
        assert!(error.to_string().contains("equivocated"));
    }

    #[tokio::test]
    async fn provider_query_bounds_record_count_and_decode_bytes() {
        let publisher = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
        let cid = ModelCid([0x61; 32]);
        let key = content_provider_dht_key(&cid);
        let valid = SignedContentProviderRecord::sign(
            sample_provider_record(&publisher, cid, 1),
            &publisher,
        )
        .unwrap()
        .encode()
        .unwrap();

        let mut count_limited = vec![b"bad".to_vec(); MAX_CONTENT_PROVIDER_RECORDS_PER_QUERY];
        count_limited.push(valid.clone());
        transport.replace_records(key.clone(), count_limited);
        assert!(registry
            .find_content_providers(&cid)
            .await
            .unwrap()
            .is_empty());

        let invalid_record_size = MAX_CONTENT_PROVIDER_RECORD_BYTES;
        let records_at_byte_limit =
            MAX_CONTENT_PROVIDER_DECODE_BYTES_PER_QUERY / invalid_record_size;
        assert!(records_at_byte_limit < MAX_CONTENT_PROVIDER_RECORDS_PER_QUERY);
        let mut byte_limited = vec![vec![0; invalid_record_size]; records_at_byte_limit];
        byte_limited.push(valid);
        transport.replace_records(key, byte_limited);
        assert!(registry
            .find_content_providers(&cid)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn installed_loaded_and_content_serving_are_independent_states() {
        let identity = NodeIdentity::generate();
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(identity, transport.clone() as _);
        let cid = ModelCid([0x71; 32]);

        assert!(registry
            .publish_installed_content_provider(&cid)
            .await
            .is_err());
        assert_eq!(transport.put_count(), 0);

        let installed = registry
            .register_installed_content("installed-only", cid, "gguf", 4_096)
            .await
            .unwrap();
        assert_eq!(
            registry.local_installed_async().await,
            vec![installed.clone()]
        );
        assert!(registry.local_models_async().await.is_empty());
        assert!(registry
            .find_content_providers(&cid)
            .await
            .unwrap()
            .is_empty());
        assert!(transport
            .puts
            .lock()
            .unwrap()
            .iter()
            .all(|(key, _)| !key.starts_with(MODEL_KEY_PREFIX)
                && !key.starts_with(CONTENT_PROVIDER_KEY_PREFIX)));

        registry
            .publish_installed_content_provider(&cid)
            .await
            .unwrap();
        assert_eq!(
            registry.find_content_providers(&cid).await.unwrap().len(),
            1
        );
        assert!(registry.local_models_async().await.is_empty());
        assert!(transport
            .puts
            .lock()
            .unwrap()
            .iter()
            .all(|(key, _)| !key.starts_with(MODEL_KEY_PREFIX)));

        registry
            .advertise_loaded(ModelCapabilities::now(
                installed.model_id,
                cid,
                "Q4_K_M",
                8_192,
                1,
                "llama.cpp",
            ))
            .await
            .unwrap();
        assert_eq!(registry.local_models_async().await.len(), 1);
        assert_eq!(registry.local_installed_async().await.len(), 1);
    }

    #[tokio::test]
    async fn verified_gguf_import_binds_alias_worker_path_and_blob_cid() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("source");
        let artifact_root = temp.path().join("artifacts");
        let verified_root = artifact_root.join("verified-models");
        fs::create_dir_all(&source_root).unwrap();
        let source_path = source_root.join("tiny.gguf");
        let bytes = b"GGUF-test-content-that-is-hashed";
        fs::write(&source_path, bytes).unwrap();

        let store = Arc::new(ArtifactStore::new(artifact_root).unwrap());
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport as _);
        let caps = registry
            .import_verified_gguf(
                store.clone(),
                source_root,
                source_path,
                verified_root.clone(),
                "TINY",
                4096,
                1,
                "llama.cpp",
            )
            .await
            .unwrap();

        let expected_blob = BlobId::from_content(bytes);
        assert_eq!(caps.model_id, "tiny");
        assert_eq!(caps.model_cid.to_hex(), expected_blob.as_str());
        assert_eq!(
            fs::read(verified_root.join(format!("{}.gguf", caps.model_cid.to_hex()))).unwrap(),
            bytes
        );
        assert!(store.get_blob(&expected_blob).unwrap().is_some());
        assert_eq!(
            registry.resolve_model_cid("tiny").await.unwrap(),
            Some(caps.model_cid)
        );
    }

    #[tokio::test]
    async fn verified_gguf_import_refuses_silent_alias_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("source");
        let artifact_root = temp.path().join("artifacts");
        let verified_root = artifact_root.join("verified-models");
        fs::create_dir_all(&source_root).unwrap();
        let first_path = source_root.join("first.gguf");
        let second_path = source_root.join("second.gguf");
        fs::write(&first_path, b"first verified model").unwrap();
        fs::write(&second_path, b"different verified model").unwrap();

        let store = Arc::new(ArtifactStore::new(artifact_root).unwrap());
        let transport = Arc::new(MockTransport::default());
        let registry = ModelRegistry::new(NodeIdentity::generate(), transport as _);
        registry
            .import_verified_gguf(
                store.clone(),
                source_root.clone(),
                first_path,
                verified_root.clone(),
                "stable-name",
                4096,
                1,
                "llama.cpp",
            )
            .await
            .unwrap();

        let error = registry
            .import_verified_gguf(
                store,
                source_root,
                second_path,
                verified_root,
                "stable-name",
                4096,
                1,
                "llama.cpp",
            )
            .await
            .expect_err("an existing alias must not be silently rebound");
        assert!(error.to_string().contains("refusing replacement"));
    }

    #[test]
    fn verified_gguf_import_rejects_conflicting_worker_file() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("source");
        let artifact_root = temp.path().join("artifacts");
        let verified_root = artifact_root.join("verified-models");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&verified_root).unwrap();
        let source_path = source_root.join("tiny.gguf");
        fs::write(&source_path, b"expected model bytes").unwrap();
        let expected_blob = BlobId::from_content(b"expected model bytes");
        fs::write(
            verified_root.join(format!("{}.gguf", expected_blob.as_str())),
            b"attacker bytes",
        )
        .unwrap();

        let store = ArtifactStore::new(artifact_root).unwrap();
        let error = import_gguf_bytes(&store, &source_root, &source_path, &verified_root)
            .expect_err("conflicting worker file must be rejected");
        assert!(error.to_string().contains("verified worker model mismatch"));
    }
}
