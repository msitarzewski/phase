// SPDX-License-Identifier: AGPL-3.0-or-later

//! Real multi-peer libp2p integration tests for Tier-1 Trusted-Circle
//! `ShardWorker` execution over `/phase/shard-stage/1.0.0`.
//!
//! Spawns actual `phase_net::Discovery` swarms on loopback TCP and exercises:
//! 1. Three-node heterogeneous pipeline partitioning (64 GiB + 24 GiB + 12 GiB)
//!    with Stage 0 local + Stages 1 & 2 remote over libp2p, verifying exact
//!    activation handoff chaining and composite Ed25519 receipt verification.
//! 2. Default-deny rejection of an uninvited peer attempting to invoke a stage.
//! 3. Operator `manual_pause` policy rejection on a participating node.
//! 4. Fail-closed rejection when a stage fails mid-pipeline.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use async_trait::async_trait;
use lucidd::policy::PolicyEngine;
use lucidd::shard_plan::{plan_pipeline_shards, ModelShardSpec, PeerShardCapacity, ShardTrustTier};
use lucidd::shard_worker::{
    execute_sharded_pipeline, make_inbound_shard_stage_handler, ShardStageContext,
    ShardStageExecutor,
};
use phase_identity::NodeIdentity;
use phase_net::{Discovery, DiscoveryConfig, Multiaddr, PeerCapabilities};

const GIB: u64 = 1024 * 1024 * 1024;

/// Deterministic test executor that appends stage metadata to the incoming
/// activation buffer so tests can assert exact stage ordering and handoff
/// integrity across real libp2p substreams.
struct DeterministicStageExecutor {
    node_label: String,
    fail_on_stage: Option<u32>,
}

#[async_trait]
impl ShardStageExecutor for DeterministicStageExecutor {
    async fn execute_stage(
        &self,
        ctx: &ShardStageContext,
        input_payload: &[u8],
    ) -> Result<Vec<u8>> {
        if self.fail_on_stage == Some(ctx.stage_index) {
            bail!("simulated GPU OOM on stage {}", ctx.stage_index);
        }
        let mut out = input_payload.to_vec();
        out.extend_from_slice(
            format!(
                "|{}[s{}:{}..{}]",
                self.node_label, ctx.stage_index, ctx.start_layer, ctx.end_layer
            )
            .as_bytes(),
        );
        Ok(out)
    }
}

async fn start_loopback_node(identity: NodeIdentity) -> (Arc<Discovery>, Multiaddr) {
    let disc = Discovery::new(DiscoveryConfig {
        identity: Some(identity),
        capabilities: PeerCapabilities::default(),
        listen_addr: "/ip4/127.0.0.1/tcp/0".to_string(),
        bootstrap_peers: Vec::new(),
    })
    .expect("create discovery");
    disc.listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("listen loopback");

    let mut addr = None;
    for _ in 0..40 {
        let addrs = disc.listen_addrs().await.expect("listen_addrs");
        if let Some(first) = addrs.into_iter().next() {
            addr = Some(first);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let listen_addr = addr.expect("bound loopback multiaddr");
    let full: Multiaddr = format!("{listen_addr}/p2p/{}", disc.local_peer_id())
        .parse()
        .expect("peer multiaddr");
    (Arc::new(disc), full)
}

async fn wait_until_connected(disc: &Discovery, target: &str) {
    for _ in 0..80 {
        if let Ok(snap) = disc.reachability_snapshot().await {
            if snap
                .connections
                .iter()
                .any(|c| c.peer_id.to_string() == target)
            {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for libp2p connection to {target}");
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sample_70b_spec() -> ModelShardSpec {
    ModelShardSpec {
        model_cid_hex: "c".repeat(64),
        model_alias: "llama3.3:70b-q4_k_m".to_string(),
        total_layers: 80,
        per_layer_bytes: 500 * 1024 * 1024,
        embedding_bytes: GIB,
        lm_head_bytes: GIB,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_trusted_circle_pipeline_executes_and_enforces_authz_and_policy() {
    // 1. Provision 3 trusted-circle nodes (M5 Max coordinator, M1 Max, 5060 Ti).
    let id_coord = NodeIdentity::generate();
    let id_m1 = NodeIdentity::generate();
    let id_5060 = NodeIdentity::generate();
    let coord_pubkey_hex = hex::encode(id_coord.peer_id_bytes());

    let (disc_m1, addr_m1) = start_loopback_node(id_m1.clone()).await;
    let (disc_5060, addr_5060) = start_loopback_node(id_5060.clone()).await;

    let temp = tempfile::tempdir().expect("tempdir");

    // Coordinator bootstraps directly to both trusted-circle peers.
    let disc_coord = Arc::new(
        Discovery::new(DiscoveryConfig {
            identity: Some(id_coord.clone()),
            capabilities: PeerCapabilities::default(),
            listen_addr: "/ip4/127.0.0.1/tcp/0".to_string(),
            bootstrap_peers: vec![addr_m1.to_string(), addr_5060.to_string()],
        })
        .expect("create coordinator discovery"),
    );
    disc_coord
        .listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("listen coordinator");

    wait_until_connected(&disc_coord, &disc_m1.local_peer_id().to_string()).await;
    wait_until_connected(&disc_coord, &disc_5060.local_peer_id().to_string()).await;

    // Default-deny policy explicitly allowlisting ONLY the coordinator key.
    let m1_policy = Arc::new(
        PolicyEngine::load_or_default(Some(temp.path().join("m1-policy.toml")))
            .await
            .expect("load m1 policy"),
    );
    m1_policy
        .update_config(|cfg| {
            cfg.allow_unauthenticated_jobs = false;
            cfg.authorized_submitters = vec![coord_pubkey_hex.clone()];
            cfg.auto_pause_on_battery = false;
            cfg.auto_pause_on_thermal_threshold_c = None;
        })
        .expect("update m1 policy");

    let d5060_policy = Arc::new(
        PolicyEngine::load_or_default(Some(temp.path().join("d5060-policy.toml")))
            .await
            .expect("load 5060 policy"),
    );
    d5060_policy
        .update_config(|cfg| {
            cfg.allow_unauthenticated_jobs = false;
            cfg.authorized_submitters = vec![coord_pubkey_hex.clone()];
            cfg.auto_pause_on_battery = false;
            cfg.auto_pause_on_thermal_threshold_c = None;
        })
        .expect("update 5060 policy");

    disc_m1
        .set_shard_stage_handler(Some(make_inbound_shard_stage_handler(
            Arc::new(id_m1.clone()),
            *disc_m1.local_peer_id(),
            m1_policy.clone(),
            Arc::new(DeterministicStageExecutor {
                node_label: "m1max".to_string(),
                fail_on_stage: None,
            }),
        )))
        .expect("install m1 shard handler");

    disc_5060
        .set_shard_stage_handler(Some(make_inbound_shard_stage_handler(
            Arc::new(id_5060.clone()),
            *disc_5060.local_peer_id(),
            d5060_policy.clone(),
            Arc::new(DeterministicStageExecutor {
                node_label: "5060ti".to_string(),
                fail_on_stage: None,
            }),
        )))
        .expect("install 5060ti shard handler");

    let peers = vec![
        PeerShardCapacity {
            peer_id: disc_coord.local_peer_id().to_string(),
            verifying_key_hex: hex::encode(id_coord.peer_id_bytes()),
            available_memory_bytes: 64 * GIB,
            backend: "metal".to_string(),
        },
        PeerShardCapacity {
            peer_id: disc_m1.local_peer_id().to_string(),
            verifying_key_hex: hex::encode(id_m1.peer_id_bytes()),
            available_memory_bytes: 24 * GIB,
            backend: "metal".to_string(),
        },
        PeerShardCapacity {
            peer_id: disc_5060.local_peer_id().to_string(),
            verifying_key_hex: hex::encode(id_5060.peer_id_bytes()),
            available_memory_bytes: 12 * GIB,
            backend: "cuda".to_string(),
        },
    ];

    let signed_plan = plan_pipeline_shards(
        &id_coord,
        &sample_70b_spec(),
        &peers,
        ShardTrustTier::TrustedCircle,
        unix_ms(),
        60_000,
    )
    .expect("plan 70B across 3 heterogeneous peers");

    let local_exec = DeterministicStageExecutor {
        node_label: "m5max".to_string(),
        fail_on_stage: None,
    };

    // 2. Execute end-to-end across all 3 peers and verify the composite receipt chain.
    let output = execute_sharded_pipeline(
        &disc_coord,
        &id_coord,
        &signed_plan,
        "job-trusted-70b-1",
        b"PROMPT:hello",
        Some(&local_exec),
        Duration::from_secs(10),
    )
    .await
    .expect("3-peer sharded pipeline must succeed");

    assert_eq!(output.receipts.len(), 3);
    assert_eq!(output.verification.stage_count, 3);
    assert_eq!(output.verification.trust_label, "trusted-circle-sharded");
    let final_str = String::from_utf8(output.output_payload).unwrap();
    assert!(final_str.starts_with("PROMPT:hello|m5max[s0:0.."));
    assert!(final_str.contains("|m1max[s1:"));
    assert!(final_str.ends_with("..80]"));

    // 3. Operator manual_pause on M1 Max must reject inbound shard stages cleanly.
    m1_policy
        .update_config(|cfg| cfg.manual_pause = true)
        .unwrap();
    let paused_err = execute_sharded_pipeline(
        &disc_coord,
        &id_coord,
        &signed_plan,
        "job-paused-1",
        b"PROMPT:paused",
        Some(&local_exec),
        Duration::from_secs(10),
    )
    .await
    .expect_err("paused peer must reject shard stage");
    assert!(
        paused_err.to_string().contains("paused"),
        "expected policy pause error, got: {paused_err}"
    );
    m1_policy
        .update_config(|cfg| cfg.manual_pause = false)
        .unwrap();

    // 4. Uninvited intruder peer must be rejected fail-closed by default-deny authz.
    let id_intruder = NodeIdentity::generate();
    let disc_intruder = Arc::new(
        Discovery::new(DiscoveryConfig {
            identity: Some(id_intruder.clone()),
            capabilities: PeerCapabilities::default(),
            listen_addr: "/ip4/127.0.0.1/tcp/0".to_string(),
            bootstrap_peers: vec![addr_m1.to_string()],
        })
        .expect("create intruder discovery"),
    );
    disc_intruder
        .listen("/ip4/127.0.0.1/tcp/0")
        .await
        .expect("listen intruder");
    wait_until_connected(&disc_intruder, &disc_m1.local_peer_id().to_string()).await;

    let intruder_peers = vec![
        PeerShardCapacity {
            peer_id: disc_intruder.local_peer_id().to_string(),
            verifying_key_hex: hex::encode(id_intruder.peer_id_bytes()),
            available_memory_bytes: 64 * GIB,
            backend: "metal".to_string(),
        },
        PeerShardCapacity {
            peer_id: disc_m1.local_peer_id().to_string(),
            verifying_key_hex: hex::encode(id_m1.peer_id_bytes()),
            available_memory_bytes: 32 * GIB,
            backend: "metal".to_string(),
        },
    ];
    let intruder_plan = plan_pipeline_shards(
        &id_intruder,
        &sample_70b_spec(),
        &intruder_peers,
        ShardTrustTier::TrustedCircle,
        unix_ms(),
        60_000,
    )
    .expect("intruder can sign its own plan");

    let unauth_err = execute_sharded_pipeline(
        &disc_intruder,
        &id_intruder,
        &intruder_plan,
        "job-intruder-1",
        b"PROMPT:steal-gpu",
        Some(&local_exec),
        Duration::from_secs(10),
    )
    .await
    .expect_err("uninvited peer must be rejected");
    assert!(
        unauth_err.to_string().contains("not authorized"),
        "expected authz rejection, got: {unauth_err}"
    );

    // 5. Mid-pipeline stage failure must fail closed without returning a verified result.
    disc_5060
        .set_shard_stage_handler(Some(make_inbound_shard_stage_handler(
            Arc::new(id_5060.clone()),
            *disc_5060.local_peer_id(),
            d5060_policy,
            Arc::new(DeterministicStageExecutor {
                node_label: "5060ti".to_string(),
                fail_on_stage: Some(2),
            }),
        )))
        .expect("install failing stage 2 handler");

    let fail_err = execute_sharded_pipeline(
        &disc_coord,
        &id_coord,
        &signed_plan,
        "job-oom-1",
        b"PROMPT:oom",
        Some(&local_exec),
        Duration::from_secs(10),
    )
    .await
    .expect_err("stage 2 failure must abort pipeline");
    assert!(
        fail_err.to_string().contains("simulated GPU OOM"),
        "expected stage failure detail, got: {fail_err}"
    );
}
