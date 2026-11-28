// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use anyhow::Result as AnyResult;
use async_trait::async_trait;
use phase_identity::NodeIdentity;

use crate::registry::{
    alias_dht_key, content_provider_dht_key, AliasRecord, ContentProviderRecord, DhtTransport,
};

#[derive(Debug, Default)]
struct TestDht {
    records: StdMutex<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
    hanging_keys: StdMutex<HashSet<Vec<u8>>>,
}

impl TestDht {
    fn hang(&self, key: Vec<u8>) {
        self.hanging_keys.lock().unwrap().insert(key);
    }
}

#[async_trait]
impl DhtTransport for TestDht {
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> AnyResult<()> {
        self.records
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(value);
        Ok(())
    }

    async fn get_record(&self, key: Vec<u8>) -> AnyResult<Vec<Vec<u8>>> {
        let should_hang = { self.hanging_keys.lock().unwrap().contains(&key) };
        if should_hang {
            futures::future::pending::<()>().await;
        }
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default())
    }
}

fn resolved(alias: &str, cid: ModelCid, size: u64, format: &str) -> ResolvedAlias {
    let mut record = AliasRecord::new(alias, cid, format, size, 1).unwrap();
    // Ensure separately constructed candidates compare only on the
    // immutable metadata under test, not on wall-clock jitter.
    record.issued_at = 1;
    record.valid_until = 2;
    ResolvedAlias {
        record,
        publisher: PeerId::random(),
    }
}

fn make_test_file_owner_writable(path: &std::path::Path) {
    let metadata = std::fs::metadata(path).unwrap();
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
}

async fn connect_discoveries(client: &Discovery, server: &Discovery) {
    server
        .listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("listen on loopback");
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(address) = server.listen_addrs().await.unwrap().into_iter().next() {
                break address;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loopback listener did not become ready");
    let server_peer = server.local_peer_id();
    client
        .dial_peer(&format!("{address}/p2p/{server_peer}"))
        .await
        .expect("dial loopback content provider");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client
                .reachability_snapshot()
                .await
                .unwrap()
                .connections
                .iter()
                .any(|connection| connection.peer_id == *server_peer)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loopback content connection did not become ready");
}

#[test]
fn config_rejects_unbounded_allocations() {
    assert!(!ContentPlaneConfig::default().publish_provider);
    for config in [
        ContentPlaneConfig {
            max_concurrent_pulls: 0,
            ..Default::default()
        },
        ContentPlaneConfig {
            max_provider_attempts: 65,
            ..Default::default()
        },
        ContentPlaneConfig {
            max_concurrent_serves: 0,
            ..Default::default()
        },
        ContentPlaneConfig {
            max_concurrent_serves: 1,
            max_concurrent_serves_per_peer: 2,
            ..Default::default()
        },
        ContentPlaneConfig {
            max_model_bytes: crate::registry::MAX_MODEL_SIZE_BYTES + 1,
            ..Default::default()
        },
        ContentPlaneConfig {
            max_staging_bytes: 0,
            ..Default::default()
        },
        ContentPlaneConfig {
            pull_deadline: Duration::from_secs(60 * 60 + 1),
            ..Default::default()
        },
    ] {
        assert!(config.validate().is_err());
    }
}

#[test]
fn blob_serve_gate_bounds_global_and_peer_admission_without_peer_maps() {
    let gate = BlobServeGate::new(2, 1);
    let first_peer = PeerId::random();
    let first_stripe = gate.peer_index(&first_peer);
    let second_peer = loop {
        let candidate = PeerId::random();
        if gate.peer_index(&candidate) != first_stripe {
            break candidate;
        }
    };

    let first = gate
        .try_acquire(&first_peer)
        .expect("first stream admitted");
    assert!(gate.try_acquire(&first_peer).is_none());
    let second = gate
        .try_acquire(&second_peer)
        .expect("different peer admitted up to global cap");
    assert!(gate.try_acquire(&PeerId::random()).is_none());
    drop(first);
    assert!(gate.try_acquire(&first_peer).is_some());
    drop(second);
}

#[test]
fn staging_quota_inventories_partials_and_reserves_complete_growth() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(temp.path().join("store")).unwrap();
    let first_blob = BlobId::from_content(b"first partial identity");
    let first_cid = ModelCid::from_hex(first_blob.as_str()).unwrap();
    let first_path = store.prepare_staging_path(&first_blob).unwrap();
    std::fs::write(&first_path, b"1234").unwrap();

    let quota = Arc::new(StagingQuota::from_store(&store, 10).unwrap());
    assert_eq!(quota.state.lock().unwrap().total_accounted, 4);
    let mut first = quota.reserve(first_cid, 8).unwrap();
    first.retain_partial();
    assert_eq!(quota.state.lock().unwrap().total_accounted, 8);

    let second_cid = ModelCid([0xA5; 32]);
    assert!(matches!(
        quota.reserve(second_cid, 3),
        Err(ContentError::Verification(message)) if message.contains("staging quota exceeded")
    ));

    std::fs::remove_file(&first_path).unwrap();
    first.complete();
    assert_eq!(quota.state.lock().unwrap().total_accounted, 0);

    // A reservation made before ArtifactStore returns a staging path is
    // rolled back on early failure/cancellation.
    drop(quota.reserve(second_cid, 10).unwrap());
    assert_eq!(quota.state.lock().unwrap().total_accounted, 0);

    // A daemon that starts over quota may consume an existing full
    // partial without permitting any additional staging growth.
    std::fs::write(&first_path, b"1234").unwrap();
    let over_quota = Arc::new(StagingQuota::from_store(&store, 3).unwrap());
    let existing = over_quota.reserve(first_cid, 4).unwrap();
    assert!(over_quota.reserve(second_cid, 1).is_err());
    std::fs::remove_file(first_path).unwrap();
    existing.complete();
    assert_eq!(over_quota.state.lock().unwrap().total_accounted, 0);
}

#[tokio::test]
async fn total_deadline_covers_alias_and_provider_dht_queries() {
    let alias_dht = Arc::new(TestDht::default());
    alias_dht.hang(alias_dht_key("hanging-alias").unwrap());
    let alias_registry = ModelRegistry::new(NodeIdentity::generate(), alias_dht);
    let alias_error = resolve_alias_before_deadline(
        &alias_registry,
        "hanging-alias",
        tokio::time::Instant::now() + Duration::from_millis(10),
    )
    .await
    .unwrap_err();
    assert!(matches!(alias_error, ContentError::DeadlineExceeded));

    let provider_dht = Arc::new(TestDht::default());
    let cid = ModelCid([0x51; 32]);
    provider_dht.hang(content_provider_dht_key(&cid));
    let provider_registry = ModelRegistry::new(NodeIdentity::generate(), provider_dht);
    let provider_error = find_providers_before_deadline(
        &provider_registry,
        &cid,
        tokio::time::Instant::now() + Duration::from_millis(10),
    )
    .await
    .unwrap_err();
    assert!(matches!(provider_error, ContentError::DeadlineExceeded));
}

#[test]
fn diagnostics_are_control_safe_and_bounded() {
    let value = bounded_diagnostic(format!("bad\n\u{1b}[31m{}", "x".repeat(1000)));
    assert!(!value.contains('\n'));
    assert!(!value.contains('\u{1b}'));
    assert!(value.chars().count() <= MAX_DIAGNOSTIC_CHARS);
}

#[test]
fn catalog_preflight_rejects_startup_import_conflicts_before_restore() {
    let catalog = vec![VerifiedCatalogEntry {
        alias: "startup-alias".to_string(),
        cid: ModelCid([1; 32]),
        size: 10,
    }];
    let installed = vec![InstalledModel {
        model_id: "startup-alias".to_string(),
        model_cid: ModelCid([2; 32]),
        format: "gguf".to_string(),
        size_bytes: 10,
        installed_at: 1,
    }];
    assert!(preflight_catalog_registration(&catalog, &installed, &[]).is_err());
    assert!(preflight_catalog_registration(&catalog, &[], &[]).is_ok());
}

#[test]
fn signed_alias_metadata_must_agree_exactly() {
    let cid = ModelCid([1; 32]);
    let agreed = vec![
        resolved("model", cid, 42, "gguf"),
        resolved("model", cid, 42, "gguf"),
    ];
    assert_eq!(
        reconcile_alias_records(&agreed).unwrap(),
        ExpectedContent {
            alias: "model".to_string(),
            cid,
            size: 42,
        }
    );

    for conflicting in [
        resolved("model", ModelCid([2; 32]), 42, "gguf"),
        resolved("model", cid, 43, "gguf"),
        resolved("model", cid, 42, "safetensors"),
    ] {
        assert!(matches!(
            reconcile_alias_records(&[agreed[0].clone(), conflicting]),
            Err(ContentError::AliasConflict)
        ));
    }
    assert!(matches!(
        reconcile_alias_records(&[resolved("model", cid, 42, "safetensors")]),
        Err(ContentError::UnsupportedFormat(format)) if format == "safetensors"
    ));
}

#[test]
fn exact_cid_and_publisher_pins_narrow_conflicts_without_choosing_arbitrarily() {
    let first_cid = ModelCid([0x31; 32]);
    let second_cid = ModelCid([0x32; 32]);
    let first = resolved("pinned", first_cid, 42, "gguf");
    let second = resolved("pinned", second_cid, 84, "gguf");

    let by_cid = select_alias_records(
        vec![first.clone(), second.clone()],
        PullSelection {
            exact_cid: Some(second_cid),
            publisher: None,
        },
    )
    .unwrap();
    assert_eq!(by_cid, vec![second.clone()]);
    assert_eq!(reconcile_alias_records(&by_cid).unwrap().cid, second_cid);

    let by_publisher = select_alias_records(
        vec![first.clone(), second.clone()],
        PullSelection {
            exact_cid: None,
            publisher: Some(first.publisher),
        },
    )
    .unwrap();
    assert_eq!(by_publisher, vec![first.clone()]);

    assert!(matches!(
        select_alias_records(
            vec![first, second],
            PullSelection {
                exact_cid: Some(ModelCid([0xff; 32])),
                publisher: None,
            },
        ),
        Err(ContentError::PinMismatch)
    ));
}

#[test]
fn only_exact_signed_content_providers_are_selected_and_deduplicated() {
    let first = PeerId::random();
    let second = PeerId::random();
    let cid = ModelCid([3; 32]);
    let expected = ExpectedContent {
        alias: "model".to_string(),
        cid,
        size: 42,
    };
    let candidate = |provider, size, format: &str| ContentProviderCandidate {
        provider,
        record: ContentProviderRecord::new(cid, size, format, provider, 1).unwrap(),
    };
    let selected = select_content_providers(
        vec![
            candidate(first, 42, "gguf"),
            candidate(second, 42, "gguf"),
            candidate(first, 42, "gguf"),
            candidate(PeerId::random(), 43, "gguf"),
            candidate(PeerId::random(), 42, "safetensors"),
        ],
        &expected,
    );
    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&first));
    assert!(selected.contains(&second));

    let attempts = provider_attempt_plan(&selected, 5);
    assert_eq!(attempts.len(), 5);
    assert_eq!(attempts[0], attempts[2]);
    assert_eq!(attempts[1], attempts[3]);
    assert_eq!(attempts[0], attempts[4]);
}

#[test]
fn transfer_state_rejects_wrong_total_offset_chunks_and_eof() {
    let mut wrong_total = TransferState::new(10, 3);
    assert!(wrong_total
        .apply(BlobStreamFrameKind::Accepted {
            total_size: 11,
            offset: 3,
        })
        .is_err());

    let mut wrong_resume = TransferState::new(10, 3);
    assert!(wrong_resume
        .apply(BlobStreamFrameKind::Accepted {
            total_size: 10,
            offset: 2,
        })
        .is_err());

    let mut state = TransferState::new(10, 3);
    assert_eq!(
        state
            .apply(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 3,
            })
            .unwrap(),
        TransferAction::Accepted
    );
    for invalid in [
        BlobStreamFrameKind::Chunk {
            offset: 2,
            bytes: vec![1],
        },
        BlobStreamFrameKind::Chunk {
            offset: 3,
            bytes: Vec::new(),
        },
        BlobStreamFrameKind::Chunk {
            offset: 3,
            bytes: vec![1; BLOB_STREAM_MAX_CHUNK_BYTES + 1],
        },
        BlobStreamFrameKind::Chunk {
            offset: 3,
            bytes: vec![1; 8],
        },
        BlobStreamFrameKind::Eof { offset: 3 },
    ] {
        let mut candidate = TransferState::new(10, 3);
        candidate
            .apply(BlobStreamFrameKind::Accepted {
                total_size: 10,
                offset: 3,
            })
            .unwrap();
        assert!(candidate.apply(invalid).is_err());
    }
}

#[test]
fn transfer_state_accepts_exact_resumed_sequence() {
    let mut state = TransferState::new(10, 3);
    state
        .apply(BlobStreamFrameKind::Accepted {
            total_size: 10,
            offset: 3,
        })
        .unwrap();
    assert_eq!(
        state
            .apply(BlobStreamFrameKind::Chunk {
                offset: 3,
                bytes: vec![4, 5, 6],
            })
            .unwrap(),
        TransferAction::Chunk(vec![4, 5, 6])
    );
    state
        .apply(BlobStreamFrameKind::Chunk {
            offset: 6,
            bytes: vec![7, 8, 9, 10],
        })
        .unwrap();
    assert_eq!(
        state
            .apply(BlobStreamFrameKind::Eof { offset: 10 })
            .unwrap(),
        TransferAction::Complete
    );
}

#[tokio::test]
async fn inbound_handler_serves_exact_ordered_resume_and_eof() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let content = vec![7_u8; BLOB_STREAM_MAX_CHUNK_BYTES + 17];
    let blob_id = store.add_blob(&content).unwrap();
    let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
    let request = BlobStreamRequest {
        schema_version: BLOB_STREAM_SCHEMA_VERSION,
        content_id: cid.0,
        offset: 13,
        deadline_unix_ms: unix_ms() + 10_000,
        idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
        metadata: Vec::new(),
    };
    let (tx, mut rx) = mpsc::channel(4);
    let task = tokio::spawn(serve_blob(store, content.len() as u64, request, tx));
    let mut frames = Vec::new();
    while let Some(frame) = rx.recv().await {
        frames.push(frame);
    }
    task.await.unwrap();

    assert!(matches!(
        frames.first().map(|frame| &frame.kind),
        Some(BlobStreamFrameKind::Accepted { total_size, offset })
            if *total_size == content.len() as u64 && *offset == 13
    ));
    let mut cursor = 13_u64;
    let mut received = Vec::new();
    for frame in &frames[1..frames.len() - 1] {
        let BlobStreamFrameKind::Chunk { offset, bytes } = &frame.kind else {
            panic!("expected only chunks between Accepted and EOF");
        };
        assert_eq!(*offset, cursor);
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= BLOB_STREAM_MAX_CHUNK_BYTES);
        cursor += bytes.len() as u64;
        received.extend_from_slice(bytes);
    }
    assert_eq!(received, content[13..]);
    assert!(matches!(
        frames.last().map(|frame| &frame.kind),
        Some(BlobStreamFrameKind::Eof { offset }) if *offset == content.len() as u64
    ));
}

#[tokio::test]
async fn inbound_handler_never_serves_uninstalled_artifact_store_blobs() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let blob_id = store.add_blob(b"unrelated artifact").unwrap();
    let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
    let identity = NodeIdentity::generate();
    let registry = Arc::new(ModelRegistry::new(
        identity.clone(),
        Arc::new(TestDht::default()),
    ));
    let network = Arc::new(
        Discovery::new(phase_net::DiscoveryConfig {
            identity: Some(identity),
            ..Default::default()
        })
        .unwrap(),
    );
    let plane = ContentPlane::new(
        network,
        registry,
        store,
        temp.path().join("verified"),
        ContentPlaneConfig::default(),
    )
    .unwrap();
    let handler = plane.blob_stream_handler();
    let (tx, mut rx) = mpsc::channel(1);
    handler(
        PeerId::random(),
        BlobStreamRequest {
            schema_version: BLOB_STREAM_SCHEMA_VERSION,
            content_id: cid.0,
            offset: 0,
            deadline_unix_ms: unix_ms() + 10_000,
            idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
            metadata: Vec::new(),
        },
        tx,
    )
    .await;
    assert!(matches!(
        rx.recv().await.map(|frame| frame.kind),
        Some(BlobStreamFrameKind::Rejected { reason }) if reason == "content unavailable"
    ));
}

#[tokio::test]
async fn inbound_rejects_metadata_bad_offset_and_receiver_loss() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let blob_id = store.add_blob(b"content").unwrap();
    let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();

    for (offset, metadata) in [(0, vec![1]), (8, Vec::new())] {
        let (tx, mut rx) = mpsc::channel(1);
        serve_blob(
            store.clone(),
            7,
            BlobStreamRequest {
                schema_version: BLOB_STREAM_SCHEMA_VERSION,
                content_id: cid.0,
                offset,
                deadline_unix_ms: unix_ms() + 10_000,
                idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
                metadata,
            },
            tx,
        )
        .await;
        assert!(matches!(
            rx.recv().await.map(|frame| frame.kind),
            Some(BlobStreamFrameKind::Rejected { .. })
        ));
    }

    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    tokio::time::timeout(
        Duration::from_secs(1),
        serve_blob(
            store,
            7,
            BlobStreamRequest {
                schema_version: BLOB_STREAM_SCHEMA_VERSION,
                content_id: cid.0,
                offset: 0,
                deadline_unix_ms: unix_ms() + 10_000,
                idle_timeout_ms: BLOB_STREAM_DEFAULT_IDLE_TIMEOUT_MS,
                metadata: Vec::new(),
            },
            tx,
        ),
    )
    .await
    .expect("receiver loss must stop the serving task");
}

#[tokio::test]
async fn stable_partial_resumes_and_corrupt_full_partial_allows_clean_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let content = b"verified provider fallback";
    let blob_id = BlobId::from_content(content);
    let (path, initial) = prepare_staging(store.clone(), blob_id.clone(), content.len() as u64)
        .await
        .unwrap();
    assert_eq!(initial, 0);
    tokio::fs::write(&path, &content[..8]).await.unwrap();
    let (same_path, resumed) =
        prepare_staging(store.clone(), blob_id.clone(), content.len() as u64)
            .await
            .unwrap();
    assert_eq!(same_path, path);
    assert_eq!(resumed, 8);
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap();
    file.write_all(&content[8..]).await.unwrap();
    file.sync_all().await.unwrap();
    drop(file);
    commit_staging(
        store.clone(),
        path.clone(),
        blob_id.clone(),
        content.len() as u64,
    )
    .await
    .unwrap();
    assert!(store.get_blob(&blob_id).unwrap().is_some());

    let other = b"correct after corrupt provider";
    let other_id = BlobId::from_content(other);
    let (other_path, _) = prepare_staging(store.clone(), other_id.clone(), other.len() as u64)
        .await
        .unwrap();
    tokio::fs::write(&other_path, vec![0_u8; other.len()])
        .await
        .unwrap();
    assert!(commit_staging(
        store.clone(),
        other_path.clone(),
        other_id.clone(),
        other.len() as u64,
    )
    .await
    .is_err());
    let (_, reset) = prepare_staging(store.clone(), other_id.clone(), other.len() as u64)
        .await
        .unwrap();
    assert_eq!(reset, 0, "bad full content must not poison fallback");
    tokio::fs::write(&other_path, other).await.unwrap();
    commit_staging(store, other_path, other_id, other.len() as u64)
        .await
        .unwrap();
}

#[tokio::test]
async fn progress_disconnect_cancels_and_same_cid_lock_serializes() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    assert!(matches!(
        emit_progress(
            &Some(tx),
            PullProgress::Resolving {
                alias: "model".to_string()
            }
        )
        .await,
        Err(ContentError::Cancelled)
    ));

    let locks = Arc::new(
        (0..CID_LOCK_STRIPES)
            .map(|_| Mutex::new(()))
            .collect::<Vec<_>>(),
    );
    let cid = ModelCid([11; 32]);
    let index = cid_lock_index(&cid);
    let first = locks[index].lock().await;
    let second_locks = locks.clone();
    let waiter = tokio::spawn(async move {
        let _guard = second_locks[index].lock().await;
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!waiter.is_finished());
    drop(first);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("same-CID waiter should proceed after release")
        .unwrap();
}

#[tokio::test]
async fn installed_catalog_persists_union_and_restores_verified_content_silently() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let verified_dir = temp.path().join("verified");
    let dht = Arc::new(TestDht::default());
    let registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht.clone()));
    let plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        registry.clone(),
        store.clone(),
        verified_dir.clone(),
        ContentPlaneConfig::default(),
    )
    .unwrap();

    let first_bytes = b"first durable consume-only model";
    let first_blob = store.add_blob(first_bytes).unwrap();
    let first_cid = ModelCid::from_hex(first_blob.as_str()).unwrap();
    plane
        .register_installed("durable-first", first_cid, first_bytes.len() as u64, &None)
        .await
        .unwrap();

    let imported_bytes = b"startup imported durable model";
    let imported_blob = store.add_blob(imported_bytes).unwrap();
    let imported_cid = ModelCid::from_hex(imported_blob.as_str()).unwrap();
    registry
        .register_verified_gguf_blob(
            store.clone(),
            verified_dir.clone(),
            "durable-import",
            imported_cid,
            imported_bytes.len() as u64,
        )
        .await
        .unwrap();
    plane.persist_installed_catalog().await.unwrap();

    let catalog_path = verified_dir.join(CONTENT_CATALOG_FILE);
    assert!(std::fs::metadata(&catalog_path).unwrap().len() <= MAX_CONTENT_CATALOG_BYTES);
    assert!(!verified_dir.join(CONTENT_CATALOG_TEMP_FILE).exists());

    let restored_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
    let restored_plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        restored_registry.clone(),
        store,
        verified_dir.clone(),
        ContentPlaneConfig::default(),
    )
    .unwrap();
    let restored = restored_plane.restore_installed_catalog().await.unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(restored_registry.local_installed_async().await.len(), 2);
    assert!(restored_registry.local_models_async().await.is_empty());
    assert!(restored_registry
        .find_content_providers(&first_cid)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read(verified_dir.join(format!("{}.gguf", first_cid.to_hex()))).unwrap(),
        first_bytes
    );
    assert_eq!(
        std::fs::read(verified_dir.join(format!("{}.gguf", imported_cid.to_hex()))).unwrap(),
        imported_bytes
    );
}

#[tokio::test]
async fn verified_gguf_activates_only_when_a_local_llama_worker_is_configured() {
    let temp = tempfile::tempdir().unwrap();
    let dht = Arc::new(TestDht::default());
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let verified_dir = temp.path().join("verified");
    let bytes = b"verified local activation model";
    let blob = store.add_blob(bytes).unwrap();
    let cid = ModelCid::from_hex(blob.as_str()).unwrap();

    let cache_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht.clone()));
    let cache_plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        cache_registry.clone(),
        store.clone(),
        verified_dir.clone(),
        ContentPlaneConfig::default(),
    )
    .unwrap();
    cache_plane
        .register_installed("activation-test", cid, bytes.len() as u64, &None)
        .await
        .unwrap();
    assert!(cache_registry.local_models_async().await.is_empty());

    let worker_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
    let worker_plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        worker_registry.clone(),
        store,
        verified_dir,
        ContentPlaneConfig {
            local_gguf_activation: Some(LocalGgufActivation {
                context_length: 16_384,
                max_concurrent: 1,
                backend: "llama.cpp".to_string(),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let restored = worker_plane.restore_installed_catalog().await.unwrap();
    assert_eq!(restored.len(), 1);
    let loaded = worker_registry.local_models_async().await;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].model_id, "activation-test");
    assert_eq!(loaded[0].model_cid, cid);
    assert_eq!(loaded[0].backend, "llama.cpp");
    assert_eq!(loaded[0].context_length, 16_384);
}

#[tokio::test]
async fn catalog_failure_rolls_back_installed_and_loaded_registry_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let verified_dir = temp.path().join("verified");
    std::fs::create_dir_all(verified_dir.join(CONTENT_CATALOG_FILE)).unwrap();
    let bytes = b"rollback on catalog failure";
    let blob = store.add_blob(bytes).unwrap();
    let cid = ModelCid::from_hex(blob.as_str()).unwrap();
    let registry = Arc::new(ModelRegistry::new(
        NodeIdentity::generate(),
        Arc::new(TestDht::default()),
    ));
    let plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        registry.clone(),
        store,
        verified_dir,
        ContentPlaneConfig {
            local_gguf_activation: Some(LocalGgufActivation {
                context_length: 8_192,
                max_concurrent: 1,
                backend: "llama.cpp".to_string(),
            }),
            ..Default::default()
        },
    )
    .unwrap();

    assert!(plane
        .register_installed("rollback-test", cid, bytes.len() as u64, &None)
        .await
        .is_err());
    assert!(registry.local_installed_async().await.is_empty());
    assert!(registry.local_models_async().await.is_empty());
}

#[tokio::test]
async fn corrupt_or_unverifiable_catalog_fails_before_exposing_any_entry() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let verified_dir = temp.path().join("verified");
    std::fs::create_dir_all(&verified_dir).unwrap();
    let good_bytes = b"valid but must not be partially restored";
    let good_blob = store.add_blob(good_bytes).unwrap();
    let good_cid = ModelCid::from_hex(good_blob.as_str()).unwrap();
    let absent_cid = ModelCid([0x93; 32]);
    let untrusted = ContentCatalog {
        schema_version: CONTENT_CATALOG_SCHEMA_VERSION,
        entries: vec![
            ContentCatalogEntry {
                alias: "good-entry".to_string(),
                cid: good_cid.to_hex(),
                size: good_bytes.len() as u64,
                format: "gguf".to_string(),
            },
            ContentCatalogEntry {
                alias: "absent-entry".to_string(),
                cid: absent_cid.to_hex(),
                size: 1,
                format: "gguf".to_string(),
            },
        ],
    };
    std::fs::write(
        verified_dir.join(CONTENT_CATALOG_FILE),
        serde_json::to_vec(&untrusted).unwrap(),
    )
    .unwrap();

    let registry = Arc::new(ModelRegistry::new(
        NodeIdentity::generate(),
        Arc::new(TestDht::default()),
    ));
    let plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        registry.clone(),
        store,
        verified_dir.clone(),
        ContentPlaneConfig::default(),
    )
    .unwrap();
    assert!(plane.restore_installed_catalog().await.is_err());
    assert!(registry.local_installed_async().await.is_empty());
    assert!(!verified_dir
        .join(format!("{}.gguf", good_cid.to_hex()))
        .exists());

    std::fs::write(
        verified_dir.join(CONTENT_CATALOG_FILE),
        b"{\"schema_version\":1,",
    )
    .unwrap();
    assert!(plane.restore_installed_catalog().await.is_err());
    assert!(registry.local_installed_async().await.is_empty());
}

#[tokio::test]
async fn catalog_restore_rehashes_and_rejects_same_size_blob_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let verified_dir = temp.path().join("verified");
    let bytes = b"catalog-bound content";
    let blob_id = store.add_blob(bytes).unwrap();
    let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
    write_catalog(
        &verified_dir,
        &[InstalledModel {
            model_id: "tamper-test".to_string(),
            model_cid: cid,
            format: "gguf".to_string(),
            size_bytes: bytes.len() as u64,
            installed_at: 1,
        }],
    )
    .unwrap();
    let stored_path = store.get_blob_path(&blob_id).unwrap();
    make_test_file_owner_writable(&stored_path);
    std::fs::write(stored_path, vec![0xA5; bytes.len()]).unwrap();

    let registry = Arc::new(ModelRegistry::new(
        NodeIdentity::generate(),
        Arc::new(TestDht::default()),
    ));
    let plane = ContentPlane::new(
        Arc::new(Discovery::new(Default::default()).unwrap()),
        registry.clone(),
        store,
        verified_dir.clone(),
        ContentPlaneConfig::default(),
    )
    .unwrap();
    assert!(plane.restore_installed_catalog().await.is_err());
    assert!(registry.local_installed_async().await.is_empty());
    assert!(!verified_dir.join(format!("{}.gguf", cid.to_hex())).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_discovery_nodes_resolve_signed_content_resume_and_reject_tampering() {
    let temp = tempfile::tempdir().unwrap();
    let dht = Arc::new(TestDht::default());
    let provider_identity = NodeIdentity::generate();
    let consumer_identity = NodeIdentity::generate();
    let provider_network = Arc::new(
        Discovery::new(phase_net::DiscoveryConfig {
            identity: Some(provider_identity.clone()),
            ..Default::default()
        })
        .unwrap(),
    );
    let consumer_network = Arc::new(
        Discovery::new(phase_net::DiscoveryConfig {
            identity: Some(consumer_identity.clone()),
            ..Default::default()
        })
        .unwrap(),
    );
    connect_discoveries(&consumer_network, &provider_network).await;

    let provider_store =
        Arc::new(ArtifactStore::new(temp.path().join("provider-store")).expect("provider store"));
    let provider_verified = temp.path().join("provider-verified");
    let provider_registry = Arc::new(ModelRegistry::new(provider_identity, dht.clone()));
    let provider_plane = ContentPlane::new(
        provider_network.clone(),
        provider_registry,
        provider_store.clone(),
        provider_verified,
        ContentPlaneConfig {
            publish_provider: true,
            pull_deadline: Duration::from_secs(10),
            ..Default::default()
        },
    )
    .unwrap();
    provider_network
        .set_blob_stream_handler(Some(provider_plane.blob_stream_handler()))
        .unwrap();

    let content = (0..(BLOB_STREAM_MAX_CHUNK_BYTES + 137))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let provider_blob = provider_store.add_blob(&content).unwrap();
    let cid = ModelCid::from_hex(provider_blob.as_str()).unwrap();
    provider_plane
        .register_installed("remote-exact", cid, content.len() as u64, &None)
        .await
        .unwrap();

    let consumer_store = Arc::new(ArtifactStore::new(temp.path().join("consumer-store")).unwrap());
    let consumer_verified = temp.path().join("consumer-verified");
    let consumer_registry = Arc::new(ModelRegistry::new(consumer_identity, dht.clone()));
    let consumer_plane = ContentPlane::new(
        consumer_network.clone(),
        consumer_registry.clone(),
        consumer_store.clone(),
        consumer_verified.clone(),
        ContentPlaneConfig {
            max_provider_attempts: 1,
            pull_deadline: Duration::from_secs(10),
            ..Default::default()
        },
    )
    .unwrap();
    let resume_offset = 777usize;
    let staging = consumer_store.prepare_staging_path(&provider_blob).unwrap();
    std::fs::write(&staging, &content[..resume_offset]).unwrap();
    let (progress_tx, mut progress_rx) = mpsc::channel(32);
    let installed = consumer_plane
        .pull("remote-exact", Some(progress_tx))
        .await
        .unwrap();
    assert_eq!(installed.model_cid, cid);
    let progress = std::iter::from_fn(|| progress_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(progress.iter().any(|event| matches!(
        event,
        PullProgress::Downloading { completed, total, .. }
            if *completed == resume_offset as u64 && *total == content.len() as u64
    )));
    let installed_blob = consumer_store.get_blob(&provider_blob).unwrap().unwrap();
    assert_eq!(std::fs::read(installed_blob.path).unwrap(), content);
    assert_eq!(
        std::fs::read(consumer_verified.join(format!("{}.gguf", cid.to_hex()))).unwrap(),
        content
    );
    assert_eq!(consumer_registry.local_installed_async().await.len(), 1);
    assert!(consumer_registry.local_models_async().await.is_empty());

    // A serving store that changes after its signed advertisement can
    // still send bytes, but the consumer's exact CID commit must reject
    // them before installation or catalog publication.
    let provider_path = provider_store.get_blob_path(&provider_blob).unwrap();
    make_test_file_owner_writable(&provider_path);
    std::fs::write(provider_path, vec![0xFF; content.len()]).unwrap();
    let tampered_store = Arc::new(ArtifactStore::new(temp.path().join("tampered-store")).unwrap());
    let tampered_verified = temp.path().join("tampered-verified");
    let tampered_registry = Arc::new(ModelRegistry::new(NodeIdentity::generate(), dht));
    let tampered_plane = ContentPlane::new(
        consumer_network,
        tampered_registry.clone(),
        tampered_store.clone(),
        tampered_verified.clone(),
        ContentPlaneConfig {
            max_provider_attempts: 1,
            pull_deadline: Duration::from_secs(10),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tampered_plane.pull("remote-exact", None).await,
        Err(ContentError::ProvidersExhausted { attempts: 1, .. })
    ));
    assert!(tampered_store.get_blob(&provider_blob).unwrap().is_none());
    assert!(tampered_registry.local_installed_async().await.is_empty());
    assert!(!tampered_verified
        .join(format!("{}.gguf", cid.to_hex()))
        .exists());
    assert!(!tampered_verified.join(CONTENT_CATALOG_FILE).exists());
}

#[tokio::test]
async fn committed_blob_registration_is_exact_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path().join("store")).unwrap());
    let content = b"registered exact model";
    let blob_id = store.add_blob(content).unwrap();
    let cid = ModelCid::from_hex(blob_id.as_str()).unwrap();
    let registry = ModelRegistry::new(NodeIdentity::generate(), Arc::new(TestDht::default()));
    let verified_dir = temp.path().join("verified");

    let first = registry
        .register_verified_gguf_blob(
            store.clone(),
            verified_dir.clone(),
            "exact-model",
            cid,
            content.len() as u64,
        )
        .await
        .unwrap();
    let second = registry
        .register_verified_gguf_blob(
            store,
            verified_dir.clone(),
            "exact-model",
            cid,
            content.len() as u64,
        )
        .await
        .unwrap();
    assert_eq!(first.model_cid, cid);
    assert_eq!(second.model_cid, cid);
    assert_eq!(registry.local_installed_async().await.len(), 1);
    assert!(registry.local_models_async().await.is_empty());
    assert!(registry
        .find_content_providers(&cid)
        .await
        .unwrap()
        .is_empty());
    registry
        .publish_installed_content_provider(&cid)
        .await
        .unwrap();
    let providers = registry.find_content_providers(&cid).await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].record.model_cid, cid);
    assert_eq!(providers[0].record.size_bytes, content.len() as u64);
    assert_eq!(
        tokio::fs::read(verified_dir.join(format!("{}.gguf", cid.to_hex())))
            .await
            .unwrap(),
        content
    );
}
