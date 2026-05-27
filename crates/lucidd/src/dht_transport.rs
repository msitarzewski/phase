// SPDX-License-Identifier: AGPL-3.0-or-later

//! `phase_net::Discovery`-backed [`DhtTransport`] implementation.
//!
//! Bridges the in-crate [`crate::registry::DhtTransport`] trait — which the
//! [`crate::ModelRegistry`] consumes for `put_record` / `get_record` — onto
//! the real Kademlia DHT exposed by `phase_net::Discovery`.
//!
//! The registry was deliberately written against a trait so the M6 work
//! could ship and be tested before the DHT lookup primitive existed in
//! phase-net. M5 added [`Discovery::get_kad_record`]; this module is the
//! last mile that wires the two.
//!
//! Cheap to clone — internally `Arc<Discovery>`, so a single router can
//! hand identical transports to the registry, the router itself, and any
//! HTTP handler that wants direct DHT access.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use phase_net::Discovery;

use crate::registry::DhtTransport;

/// Real [`DhtTransport`] backed by phase-net.
#[derive(Clone)]
pub struct PhaseNetDhtTransport {
    discovery: Arc<Discovery>,
}

impl PhaseNetDhtTransport {
    pub fn new(discovery: Arc<Discovery>) -> Self {
        Self { discovery }
    }

    /// Borrow the underlying [`Discovery`] handle. Useful for code paths
    /// (e.g. the router's peer-relay glue) that already hold a transport
    /// and want to issue other phase-net commands without storing two
    /// copies of the `Arc`.
    pub fn discovery(&self) -> &Arc<Discovery> {
        &self.discovery
    }
}

#[async_trait]
impl DhtTransport for PhaseNetDhtTransport {
    async fn put_record(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        // `publish_kad_record` takes `&self` (it routes through the
        // internal command channel), so a single `Arc<Discovery>` is
        // enough — no `Mutex` here.
        self.discovery.publish_kad_record(key, value).await
    }

    async fn get_record(&self, key: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        self.discovery.get_kad_record(key).await
    }
}
