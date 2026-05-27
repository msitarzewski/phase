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
//! value = bincode(SignedModelAdvertisement)
//! ```
//!
//! `SignedModelAdvertisement` carries the [`ModelCapabilities`], the
//! advertising peer's Ed25519 public key, and a signature over the
//! canonical form (`bincode(ad)` without the signature field). The schema
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
//! re-propagating advertisements when the peer comes back online.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
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
/// Bumped when fields are added/removed in a non-additive way. Readers
/// must check this before trusting the rest of the payload.
pub const ADVERTISEMENT_SCHEMA_VERSION: u32 = 1;

/// DHT key prefix for model advertisements. Final key shape:
/// `b"phase/model/" || model_cid` — exactly 12 + 32 = 44 bytes.
pub const MODEL_KEY_PREFIX: &[u8] = b"phase/model/";

/// How long between TTL refresh publishes. Kademlia's default record
/// lifetime is 36h, but we re-advertise on a much shorter cadence so a
/// reader sees freshness ≤ this interval. 5 minutes matches the design
/// brief and gives quick recovery after a transient outage.
pub const TTL_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Default advertisement lifetime baked into [`ModelCapabilities::valid_until`].
/// Set to `refresh_interval * 3` so a missed refresh doesn't immediately
/// invalidate the record from a consumer's perspective.
pub const ADVERTISEMENT_TTL: Duration = Duration::from_secs(15 * 60);

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

    /// Render the DHT key for this CID:
    /// `b"phase/model/" || cid_bytes` (44 bytes).
    pub fn dht_key(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MODEL_KEY_PREFIX.len() + self.0.len());
        out.extend_from_slice(MODEL_KEY_PREFIX);
        out.extend_from_slice(&self.0);
        out
    }
}

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

    /// Quantization label as the worker reports it, e.g. `"Q4_K_M"`,
    /// `"Q8_0"`, `"F16"`. Not validated here — just propagated.
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
}

/// Signed envelope around [`ModelCapabilities`]. This is what actually
/// goes onto the wire as the DHT record value.
///
/// Layout (bincode):
/// ```text
/// schema_version: u32
/// caps:           ModelCapabilities
/// pubkey:         [u8; 32]    // Ed25519 verifying key
/// signature:      [u8; 64]    // signature over the canonical form
/// ```
///
/// The "canonical form" signed is bincode-encoded `SigningPayload`
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

    /// Detached Ed25519 signature over `bincode(SigningPayload { .. })`.
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
        bincode::serialize(&SigningPayload {
            schema_version,
            caps,
            pubkey,
        })
        .context("serialize SigningPayload for advertisement")
    }

    /// Sign a fresh advertisement with the given identity.
    pub fn sign(caps: ModelCapabilities, identity: &NodeIdentity) -> Result<Self> {
        let pubkey = identity.verifying_key().to_bytes();
        let bytes =
            Self::canonical_signed_bytes(ADVERTISEMENT_SCHEMA_VERSION, &caps, pubkey)?;
        let signature = identity.signing_key().sign(&bytes).to_bytes().to_vec();
        Ok(Self {
            schema_version: ADVERTISEMENT_SCHEMA_VERSION,
            caps,
            pubkey,
            signature,
        })
    }

    /// Verify the signature over `caps` + `pubkey` + `schema_version`.
    /// Does **not** check that `pubkey` matches a libp2p `PeerId` —
    /// callers that consume records from the DHT must do that
    /// independently (we don't always have the `PeerId` at the point of
    /// verification, e.g. inside a unit test).
    pub fn verify(&self) -> Result<()> {
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
        let bytes = Self::canonical_signed_bytes(
            self.schema_version,
            &self.caps,
            self.pubkey,
        )?;
        let vk = VerifyingKey::from_bytes(&self.pubkey)
            .context("decode advertisement pubkey")?;
        let sig = Signature::from_bytes(sig_bytes);
        vk.verify(&bytes, &sig)
            .map_err(|e| anyhow!("advertisement signature failed to verify: {e}"))?;
        Ok(())
    }

    /// Bincode-encode the full signed envelope for DHT publication.
    pub fn encode(&self) -> Result<Vec<u8>> {
        bincode::serialize(self).context("serialize SignedModelAdvertisement")
    }

    /// Decode + verify in one step. Returns the inner advertisement only
    /// after the signature checks out.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let ad: SignedModelAdvertisement =
            bincode::deserialize(bytes).context("decode SignedModelAdvertisement")?;
        ad.verify()?;
        Ok(ad)
    }
}

// ---------------------------------------------------------------------------
// DhtTransport — small abstraction over what the registry needs from the DHT.
// ---------------------------------------------------------------------------

/// Minimal DHT surface the registry consumes.
///
/// Why a trait? `phase_net::Discovery::publish_kad_record` takes
/// `&mut self`, which means a registry that wants to share access to one
/// `Discovery` across an HTTP handler **and** a background refresh task
/// would need its own `Arc<Mutex<Discovery>>` wrapper. Instead, we put
/// that synchronization concern behind a trait the registry doesn't have
/// to know about. The real wiring — building a `DhtTransport` that
/// drives `Discovery` — is LUCID M5's router work.
///
/// `get_record` is not yet exposed by phase-net's public API. The trait
/// declares it so the registry can be wired against a real DHT lookup
/// when M5 lands; until then implementations may return `Ok(vec![])`.
/// This is documented in the module-level "Flag for M5" note in
/// [`crate::registry`].
#[async_trait]
pub trait DhtTransport: Send + Sync {
    /// Publish (or refresh) a record under `key`. Idempotent — calling
    /// twice with the same key/value updates the existing record.
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()>;

    /// Look up records under `key`. Returns the raw byte payloads that
    /// other peers have published — the registry decodes and verifies
    /// each one before returning it to a caller.
    ///
    /// **M5 gap**: phase-net does not yet expose a record-lookup
    /// primitive. Implementations backed by `Discovery` should currently
    /// return `Ok(vec![])`; flag this for M5 to wire.
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

    /// Active TTL refresh task per advertised model. Cancelled on
    /// `withdraw` and on `Drop`.
    refresh_tasks: Arc<Mutex<HashMap<ModelCid, JoinHandle<()>>>>,

    /// Refresh interval. Overridable for tests so we don't have to wait
    /// 5 real minutes to exercise the refresh path.
    refresh_interval: Duration,
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
            refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            refresh_interval: TTL_REFRESH_INTERVAL,
        }
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
            refresh_tasks: Arc::new(Mutex::new(HashMap::new())),
            refresh_interval,
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

                let signed = match SignedModelAdvertisement::sign(refreshed, &identity)
                {
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
                if let Err(e) =
                    transport.put_record(cid_for_task.dht_key(), value).await
                {
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
        let mut out = Vec::with_capacity(raw_records.len());
        for record in raw_records {
            match SignedModelAdvertisement::decode(&record) {
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
    /// Implementation: check the local loaded set for a matching id; if
    /// found, look up by its CID. There is currently no cross-peer
    /// `name → cid` index, so a router that wants to route to peers for
    /// a model **it doesn't have loaded** needs an out-of-band mapping
    /// (e.g. an Ollama-style model name registry). That index is M5's
    /// problem; today this method returns an empty `Vec` if the model
    /// is not locally loaded.
    pub async fn find_peers_by_model_id(
        &self,
        model_id: &str,
    ) -> Result<Vec<(PeerId, ModelCapabilities)>> {
        let cid_opt = {
            let loaded = self.loaded.read().await;
            loaded
                .values()
                .find(|c| c.model_id == model_id)
                .map(|c| c.model_cid)
        };
        match cid_opt {
            Some(cid) => self.find_peers_for_model(&cid).await,
            None => Ok(Vec::new()),
        }
    }
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
    }

    impl MockTransport {
        fn put_count(&self) -> usize {
            self.puts.lock().unwrap().len()
        }
        fn last_put(&self) -> Option<(Vec<u8>, Vec<u8>)> {
            self.puts.lock().unwrap().last().cloned()
        }
        fn install_record(&self, key: Vec<u8>, value: Vec<u8>) {
            self.store
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push(value);
        }
    }

    #[async_trait]
    impl DhtTransport for MockTransport {
        async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
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
        let ad = SignedModelAdvertisement::decode(&value)
            .expect("published value must decode + verify");
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
        let saw_refresh = wait_for(
            || transport.put_count() >= 2,
            Duration::from_secs(61),
            10,
        )
        .await;
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
        let reached = wait_for(
            || transport.put_count() >= 4,
            Duration::from_secs(61),
            20,
        )
        .await;
        assert!(
            reached,
            "expected >=4 puts (1 initial + 3 refreshes), got {}",
            transport.put_count()
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
    fn tamper_with_caps_breaks_signature() {
        let identity = NodeIdentity::generate();
        let mut ad =
            SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
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
        let mut ad =
            SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        // Flip one byte of the embedded pubkey — bincode round-trips it
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
        let mut ad =
            SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
        ad.signature[0] ^= 0xff;
        assert!(ad.verify().is_err());
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let identity = NodeIdentity::generate();
        let mut ad =
            SignedModelAdvertisement::sign(sample_caps(), &identity).unwrap();
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
        let expected = peer_id_from_ed25519_pubkey(
            &identity.verifying_key().to_bytes(),
        )
        .unwrap();
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
        transport.install_record(cid.dht_key(), b"not-a-valid-bincode-record".to_vec());

        let peers = registry.find_peers_for_model(&cid).await.unwrap();
        assert!(peers.is_empty(), "garbage records must be discarded");
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
}
