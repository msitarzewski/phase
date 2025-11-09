use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::{
    identity::Keypair,
    kad::{store::MemoryStore, Behaviour as KademliaBehaviour, Config as KademliaConfig, Event as KademliaEvent},
    swarm::SwarmEvent,
    Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::peer::{PeerCapabilities, PeerInfo};

/// Discovery configuration
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Listen address (e.g., "/ip4/0.0.0.0/tcp/0")
    pub listen_addr: String,

    /// Bootstrap peers to connect to
    pub bootstrap_peers: Vec<String>,

    /// Local peer capabilities
    pub capabilities: PeerCapabilities,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_string(),
            bootstrap_peers: Vec::new(),
            capabilities: PeerCapabilities::default(),
        }
    }
}

/// Peer discovery service using Kademlia DHT
pub struct Discovery {
    swarm: Swarm<KademliaBehaviour<MemoryStore>>,
    local_peer_id: PeerId,
    capabilities: PeerCapabilities,
}

impl Discovery {
    /// Create a new discovery service
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
        // Generate keypair for this node
        let keypair = Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(keypair.public());

        info!("Local peer ID: {}", local_peer_id);

        // Create Kademlia behaviour
        let store = MemoryStore::new(local_peer_id);
        let kad_config = KademliaConfig::default();
        let mut kad_behaviour = KademliaBehaviour::with_config(local_peer_id, store, kad_config);

        // Add bootstrap peers
        for peer_addr in &config.bootstrap_peers {
            if let Ok(addr) = peer_addr.parse::<Multiaddr>() {
                // Extract peer ID and add
                // Note: Bootstrap peer format should be: /ip4/x.x.x.x/tcp/port/p2p/PeerID
                debug!("Adding bootstrap peer: {}", peer_addr);
                // TODO: Parse peer ID from multiaddr and add
            }
        }

        // Build swarm
        let swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                Default::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_behaviour(|_key| kad_behaviour)?
            .build();

        Ok(Self {
            swarm,
            local_peer_id,
            capabilities: config.capabilities,
        })
    }

    /// Start listening on the configured address
    pub fn listen(&mut self, addr: &str) -> Result<()> {
        let listen_addr: Multiaddr = addr.parse()
            .context("Failed to parse listen address")?;

        self.swarm.listen_on(listen_addr.clone())?;
        info!("Listening on: {}", listen_addr);

        Ok(())
    }

    /// Bootstrap the DHT
    pub fn bootstrap(&mut self) -> Result<()> {
        self.swarm.behaviour_mut().bootstrap()?;
        info!("DHT bootstrap initiated");
        Ok(())
    }

    /// Get local peer ID
    pub fn local_peer_id(&self) -> &PeerId {
        &self.local_peer_id
    }

    /// Get local capabilities
    pub fn capabilities(&self) -> &PeerCapabilities {
        &self.capabilities
    }

    /// Run the discovery event loop
    pub async fn run(&mut self) -> Result<()> {
        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(event)) => {
                    self.handle_kad_event(event).await?;
                }
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    info!("Listening on new address: {}", address);
                }
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. }) => {
                    info!("Connected to peer: {}", peer_id);
                }
                Some(SwarmEvent::ConnectionClosed { peer_id, cause, .. }) => {
                    debug!("Connection closed to {}: {:?}", peer_id, cause);
                }
                Some(event) => {
                    debug!("Other swarm event: {:?}", event);
                }
                None => break,
            }
        }

        Ok(())
    }

    /// Handle Kademlia DHT events
    async fn handle_kad_event(&mut self, event: KademliaEvent) -> Result<()> {
        match event {
            KademliaEvent::OutboundQueryProgressed { result, .. } => {
                debug!("Outbound query result: {:?}", result);
            }
            KademliaEvent::RoutingUpdated { peer, .. } => {
                debug!("Routing table updated with peer: {}", peer);
            }
            KademliaEvent::UnroutablePeer { peer } => {
                warn!("Unroutable peer: {}", peer);
            }
            KademliaEvent::RoutablePeer { peer, address } => {
                info!("Discovered routable peer: {} at {}", peer, address);
            }
            KademliaEvent::PendingRoutablePeer { peer, address } => {
                debug!("Pending routable peer: {} at {}", peer, address);
            }
            _ => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_creation() {
        let config = DiscoveryConfig::default();
        let discovery = Discovery::new(config);
        assert!(discovery.is_ok());
    }

    #[test]
    fn test_default_capabilities() {
        let caps = PeerCapabilities::default();
        assert!(!caps.arch.is_empty());
        assert!(caps.cpu_cores > 0);
    }
}
