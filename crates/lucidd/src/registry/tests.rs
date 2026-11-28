// SPDX-License-Identifier: AGPL-3.0-or-later

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
    catalog_providers: StdMutex<Option<Vec<PeerId>>>,
    catalog_queries: std::sync::atomic::AtomicUsize,
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

    async fn start_providing_key(&self, _key: Vec<u8>) -> Result<()> {
        if self.catalog_providers.lock().unwrap().is_some() {
            Ok(())
        } else {
            Err(ProviderDiscoveryUnsupported.into())
        }
    }

    async fn get_provider_peers(&self, _key: Vec<u8>) -> Result<Vec<PeerId>> {
        self.catalog_queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.catalog_providers
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ProviderDiscoveryUnsupported.into())
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

#[test]
fn catalog_enforces_signature_publisher_expiry_and_decode_bounds() {
    let identity = NodeIdentity::generate();
    let peer = peer_id_from_ed25519_pubkey(&identity.verifying_key().to_bytes()).unwrap();
    let model = CatalogModel {
        caps: sample_caps(),
        size_bytes: None,
        format: None,
    };
    let signed = SignedCatalog::sign(vec![model.clone()], &identity).unwrap();
    let encoded = signed.encode().unwrap();
    assert!(SignedCatalog::decode(&encoded, peer, unix_ms_now()).is_ok());
    assert!(SignedCatalog::decode(&encoded, PeerId::random(), unix_ms_now()).is_err());
    assert!(SignedCatalog::decode(&encoded, peer, signed.valid_until).is_err());
    let mut forged = signed.clone();
    forged.models[0].caps.context_length += 1;
    assert!(SignedCatalog::decode(&forged.encode().unwrap(), peer, unix_ms_now()).is_err());
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(SignedCatalog::decode(&trailing, peer, unix_ms_now()).is_err());
    assert!(SignedCatalog::decode(&vec![0; MAX_CATALOG_BYTES + 1], peer, unix_ms_now()).is_err());
    assert!(SignedCatalog::sign(vec![model.clone(); MAX_CATALOG_MODELS + 1], &identity).is_err());
    let mut declared = postcard::to_allocvec(&(
        CATALOG_SCHEMA_VERSION,
        signed.issued_at,
        signed.valid_until,
        signed.pubkey,
    ))
    .unwrap();
    declared.extend(postcard::to_allocvec(&((MAX_CATALOG_MODELS + 1) as u64)).unwrap());
    assert!(SignedCatalog::decode(&declared, peer, unix_ms_now())
        .unwrap_err()
        .to_string()
        .contains("count"));
    let mut bad_metadata = model.clone();
    bad_metadata.size_bytes = Some(0);
    bad_metadata.format = Some("gguf".into());
    assert!(SignedCatalog::sign(vec![bad_metadata], &identity).is_err());
    let mut bad_caps = model;
    bad_caps.caps.model_id = "NOT-CANONICAL".into();
    assert!(SignedCatalog::sign(vec![bad_caps], &identity).is_err());
}

#[tokio::test]
async fn catalog_discovers_without_names_preserves_conflicts_and_coalesces_polling() {
    let transport = Arc::new(MockTransport::default());
    let identities = (0..3).map(|_| NodeIdentity::generate()).collect::<Vec<_>>();
    let peers = identities
        .iter()
        .map(|id| peer_id_from_ed25519_pubkey(&id.verifying_key().to_bytes()).unwrap())
        .collect::<Vec<_>>();
    *transport.catalog_providers.lock().unwrap() = Some(peers.clone());
    for (i, identity) in identities.iter().enumerate() {
        let mut caps = sample_caps();
        caps.model_id = "shared-name".into();
        caps.model_cid = ModelCid([if i < 2 { 1 } else { 2 }; 32]);
        caps.max_concurrent = i as u32 + 1;
        let snapshot = SignedCatalog::sign(
            vec![CatalogModel {
                caps,
                size_bytes: None,
                format: None,
            }],
            identity,
        )
        .unwrap();
        transport.install_record(catalog_publisher_key(&peers[i]), snapshot.encode().unwrap());
    }
    let registry = Arc::new(ModelRegistry::new(
        NodeIdentity::generate(),
        transport.clone(),
    ));
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            registry.discover_network_models().await.unwrap()
        }));
    }
    for task in tasks {
        let models = task.await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .all(|model| model.caps.model_id == "shared-name"
                && model.size_bytes.is_none()
                && model.format.is_none()));
        let first = models
            .iter()
            .find(|model| model.caps.model_cid == ModelCid([1; 32]))
            .unwrap();
        assert_eq!(first.providers.len(), 2);
    }
    assert_eq!(
        transport
            .catalog_queries
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let peers = registry
        .find_peers_for_model(&ModelCid([1; 32]))
        .await
        .unwrap();
    let mut capacities = peers
        .iter()
        .map(|(_, caps)| caps.max_concurrent)
        .collect::<Vec<_>>();
    capacities.sort_unstable();
    assert_eq!(
        capacities,
        vec![1, 2],
        "routing must preserve each publisher's own capability claims"
    );
    assert!(
        registry
            .resolve_model_cid("shared-name")
            .await
            .unwrap()
            .is_none(),
        "catalog must not silently choose a conflicting alias"
    );
}

#[tokio::test]
async fn catalog_bounds_publisher_fanout_and_expires_cached_entries() {
    let transport = Arc::new(MockTransport::default());
    let identities = (0..MAX_CATALOG_PUBLISHERS * 2)
        .map(|_| NodeIdentity::generate())
        .collect::<Vec<_>>();
    let peers = identities
        .iter()
        .map(|identity| peer_id_from_ed25519_pubkey(&identity.verifying_key().to_bytes()).unwrap())
        .collect::<Vec<_>>();
    *transport.catalog_providers.lock().unwrap() = Some(peers.clone());
    for (index, identity) in identities.iter().enumerate() {
        let mut caps = sample_caps();
        caps.model_cid = ModelCid([index as u8 + 1; 32]);
        let record = SignedCatalog::sign(
            vec![CatalogModel {
                caps,
                size_bytes: None,
                format: None,
            }],
            identity,
        )
        .unwrap();
        transport.install_record(
            catalog_publisher_key(&peers[index]),
            record.encode().unwrap(),
        );
    }
    let registry = ModelRegistry::new(NodeIdentity::generate(), transport);
    assert_eq!(
        registry.discover_network_models().await.unwrap().len(),
        MAX_CATALOG_PUBLISHERS
    );
    for entry in &mut registry.network_catalog.write().await.models {
        entry.expires_at = unix_ms_now();
    }
    assert!(registry.cached_network_models().await.is_empty());
    assert!(
        registry.discover_network_models().await.unwrap().is_empty(),
        "fresh cache must still filter expired claims"
    );
}

#[tokio::test]
async fn catalog_publication_uses_real_installed_metadata_and_withdraws() {
    let transport = Arc::new(MockTransport::default());
    let identity = NodeIdentity::generate();
    let peer = peer_id_from_ed25519_pubkey(&identity.verifying_key().to_bytes()).unwrap();
    *transport.catalog_providers.lock().unwrap() = Some(vec![peer]);
    let registry = ModelRegistry::new(identity, transport.clone());
    let caps = sample_caps();
    registry.advertise_loaded(caps.clone()).await.unwrap();
    let snapshots = transport.values_with_prefix(CATALOG_KEY_PREFIX);
    let snapshot = SignedCatalog::decode(snapshots.last().unwrap(), peer, unix_ms_now()).unwrap();
    assert!(snapshot.models[0].size_bytes.is_none());
    registry.installed.write().await.insert(
        caps.model_id.clone(),
        InstalledModel {
            model_id: caps.model_id.clone(),
            model_cid: caps.model_cid,
            size_bytes: 8192,
            format: "gguf".into(),
            installed_at: unix_ms_now(),
        },
    );
    registry.refresh_network_catalog().await;
    let snapshots = transport.values_with_prefix(CATALOG_KEY_PREFIX);
    let snapshot = SignedCatalog::decode(snapshots.last().unwrap(), peer, unix_ms_now()).unwrap();
    assert_eq!(snapshot.models[0].size_bytes, Some(8192));
    assert_eq!(snapshot.models[0].format.as_deref(), Some("gguf"));
    registry.withdraw(&caps.model_cid).await.unwrap();
    let snapshots = transport.values_with_prefix(CATALOG_KEY_PREFIX);
    assert!(
        SignedCatalog::decode(snapshots.last().unwrap(), peer, unix_ms_now())
            .unwrap()
            .models
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_discovery_between_real_nodes_needs_no_model_name() {
    use phase_net::{Discovery, DiscoveryConfig, ReachabilityConfig};
    let identity = NodeIdentity::generate();
    let serving = Arc::new(
        Discovery::new(DiscoveryConfig {
            identity: Some(identity.clone()),
            ..DiscoveryConfig::default()
        })
        .unwrap(),
    );
    serving.listen("/ip4/127.0.0.1/tcp/0").await.unwrap();
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(addr) = serving
                .listen_addrs()
                .await
                .unwrap()
                .into_iter()
                .find(|addr| addr.to_string().starts_with("/ip4/127.0.0.1/"))
            {
                break addr;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let client = Arc::new(
        Discovery::new_with_reachability(
            DiscoveryConfig {
                bootstrap_peers: vec![format!("{address}/p2p/{}", serving.local_peer_id())],
                ..DiscoveryConfig::default()
            },
            ReachabilityConfig {
                outbound_only: true,
                ..ReachabilityConfig::default()
            },
        )
        .unwrap(),
    );
    let publisher = ModelRegistry::new(
        identity,
        Arc::new(crate::PhaseNetDhtTransport::new(serving)),
    );
    publisher.advertise_loaded(sample_caps()).await.unwrap();
    let consumer = ModelRegistry::new(
        NodeIdentity::generate(),
        Arc::new(crate::PhaseNetDhtTransport::new(client)),
    );
    let models = consumer.discover_network_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].caps.model_cid, ModelCid([7; 32]));
    assert_eq!(models[0].providers.len(), 1);
    assert!(models[0].size_bytes.is_none());
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
async fn wait_for<F: FnMut() -> bool>(mut predicate: F, step: Duration, max_iters: u32) -> bool {
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
    let saw_refresh = wait_for(|| transport.put_count() >= 2, Duration::from_secs(61), 10).await;
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
async fn find_peers_for_model_caches_verified_dht_results() {
    let provider = NodeIdentity::generate();
    let transport = Arc::new(MockTransport::default());
    let registry = ModelRegistry::new(NodeIdentity::generate(), transport.clone() as _);
    let cid = ModelCid([0x7b; 32]);
    let mut caps = sample_caps();
    caps.model_cid = cid;
    transport.replace_records(
        cid.dht_key(),
        vec![SignedModelAdvertisement::sign(caps, &provider)
            .unwrap()
            .encode()
            .unwrap()],
    );

    let first = registry.find_peers_for_model(&cid).await.unwrap();
    assert_eq!(first.len(), 1);

    // Clear the backing DHT transport; a back-to-back lookup within the short
    // cache TTL must still return the previously verified peer without stalling
    // on another full WAN DHT sweep.
    transport.replace_records(cid.dht_key(), Vec::new());
    let second = registry.find_peers_for_model(&cid).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].0, first[0].0);
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
    ModelRegistry::new_with_alias_replay_state(NodeIdentity::generate(), transport, &path).unwrap();

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
        .publish_alias(AliasRecord::new("conflict", ModelCid([1; 32]), "gguf", 1024, 1).unwrap())
        .await
        .unwrap();
    second
        .publish_alias(AliasRecord::new("conflict", ModelCid([2; 32]), "gguf", 1024, 1).unwrap())
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

    let older =
        SignedContentProviderRecord::sign(sample_provider_record(&publisher, cid, 9), &publisher)
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

    let first =
        SignedContentProviderRecord::sign(sample_provider_record(&publisher, cid, 11), &publisher)
            .unwrap();
    let mut conflicting_record = first.record.clone();
    conflicting_record.format = "safetensors".to_string();
    let conflicting = SignedContentProviderRecord::sign(conflicting_record, &publisher).unwrap();
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
    let valid =
        SignedContentProviderRecord::sign(sample_provider_record(&publisher, cid, 1), &publisher)
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
    let records_at_byte_limit = MAX_CONTENT_PROVIDER_DECODE_BYTES_PER_QUERY / invalid_record_size;
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
