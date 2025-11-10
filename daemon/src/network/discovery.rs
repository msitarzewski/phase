use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use futures::StreamExt;
use libp2p::{
    identity::Keypair,
    kad::{store::MemoryStore, Behaviour as KademliaBehaviour, Event as KademliaEvent},
    mdns,
    swarm::{NetworkBehaviour, SwarmEvent},
    Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::peer::PeerCapabilities;
use super::protocol::{JobOffer, JobResponse, RejectionReason, JobRequest, JobResult};
use super::execution::ExecutionHandler;

/// Combined network behaviour for Kademlia DHT and mDNS local discovery
#[derive(NetworkBehaviour)]
struct CombinedBehaviour {
    kademlia: KademliaBehaviour<MemoryStore>,
    mdns: mdns::tokio::Behaviour,
}

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

/// Peer discovery service using Kademlia DHT and mDNS
pub struct Discovery {
    swarm: Swarm<CombinedBehaviour>,
    local_peer_id: PeerId,
    capabilities: PeerCapabilities,
    execution_handler: ExecutionHandler,
}

impl Discovery {
    /// Create a new discovery service
    pub fn new(config: DiscoveryConfig) -> Result<Self> {
        // Generate keypair for this node
        let keypair = Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(keypair.public());

        info!("Local peer ID: {}", local_peer_id);

        // Generate Ed25519 signing key for receipts (separate from libp2p keypair)
        use rand::RngCore;
        let mut secret_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret_bytes);
        let signing_key = SigningKey::from_bytes(&secret_bytes);

        // Create execution handler
        let execution_handler = ExecutionHandler::new(signing_key.clone());

        info!("Node public key: {}", execution_handler.public_key_hex());

        // Create Kademlia behaviour
        let store = MemoryStore::new(local_peer_id);
        let kad_behaviour = KademliaBehaviour::new(local_peer_id, store);

        // Add bootstrap peers
        for peer_addr in &config.bootstrap_peers {
            if let Ok(_addr) = peer_addr.parse::<Multiaddr>() {
                // Extract peer ID and add
                // Note: Bootstrap peer format should be: /ip4/x.x.x.x/tcp/port/p2p/PeerID
                debug!("Adding bootstrap peer: {}", peer_addr);
                // TODO: Parse peer ID from multiaddr and add
            }
        }

        // Build swarm with tokio executor
        let swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_quic()
            .with_behaviour(|key| {
                // Create mDNS behaviour for local network discovery
                let mdns_behaviour = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

                Ok(CombinedBehaviour {
                    kademlia: kad_behaviour,
                    mdns: mdns_behaviour,
                })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        Ok(Self {
            swarm,
            local_peer_id,
            capabilities: config.capabilities,
            execution_handler,
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
        match self.swarm.behaviour_mut().kademlia.bootstrap() {
            Ok(_) => {
                info!("DHT bootstrap initiated");
                Ok(())
            }
            Err(e) => {
                warn!("DHT bootstrap failed (this is normal for standalone nodes): {}", e);
                warn!("Node will wait for incoming connections or manual peer additions");
                info!("mDNS is active for local network peer discovery");
                Ok(())  // Not fatal - continue running
            }
        }
    }

    /// Get local peer ID
    pub fn local_peer_id(&self) -> &PeerId {
        &self.local_peer_id
    }

    /// Get local capabilities
    pub fn capabilities(&self) -> &PeerCapabilities {
        &self.capabilities
    }

    /// Get node's public key (hex-encoded)
    pub fn public_key_hex(&self) -> String {
        self.execution_handler.public_key_hex()
    }

    /// Execute a job request (for testing/local execution)
    pub async fn execute_job(&self, request: JobRequest) -> Result<JobResult> {
        self.execution_handler.execute_job(request).await
    }

    /// Advertise this node's capabilities on the DHT
    pub fn advertise_capabilities(&mut self) -> Result<()> {
        use libp2p::kad::RecordKey;

        // Create a capability identifier key
        let capability_key = format!(
            "/phase/capability/{}/{}",
            self.capabilities.arch,
            self.capabilities.wasm_runtime
        );

        let key = RecordKey::new(&capability_key.as_bytes());

        // Start providing this capability
        self.swarm.behaviour_mut().kademlia.start_providing(key)
            .context("Failed to advertise capabilities")?;

        info!("Advertising capabilities: {}", capability_key);
        Ok(())
    }

    /// Discover peers with specific capability
    pub fn discover_peers(&mut self, arch: &str, runtime: &str) -> Result<()> {
        use libp2p::kad::RecordKey;

        let capability_key = format!("/phase/capability/{}/{}", arch, runtime);
        let key = RecordKey::new(&capability_key.as_bytes());

        self.swarm.behaviour_mut().kademlia.get_providers(key);

        info!("Discovering peers with capability: {}", capability_key);
        Ok(())
    }

    /// Handle incoming job offer
    pub fn handle_job_offer(&self, offer: JobOffer) -> JobResponse {
        use std::time::{SystemTime, UNIX_EPOCH};

        info!("Received job offer: {} (module: {})", offer.job_id, offer.module_hash);

        // Check architecture compatibility
        if offer.requirements.arch != self.capabilities.arch {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::ArchMismatch {
                    required: offer.requirements.arch,
                    available: self.capabilities.arch.clone(),
                },
            };
        }

        // Check runtime compatibility
        if !self.capabilities.wasm_runtime.contains(&offer.requirements.wasm_runtime.split('-').next().unwrap_or("")) {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::RuntimeNotSupported {
                    required: offer.requirements.wasm_runtime,
                },
            };
        }

        // Check resource availability
        if offer.requirements.cpu_cores > self.capabilities.cpu_cores {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::InsufficientResources {
                    missing: format!("CPU: need {}, have {}", offer.requirements.cpu_cores, self.capabilities.cpu_cores),
                },
            };
        }

        if offer.requirements.memory_mb > self.capabilities.memory_mb {
            return JobResponse::Rejected {
                job_id: offer.job_id,
                reason: RejectionReason::InsufficientResources {
                    missing: format!("Memory: need {} MB, have {} MB", offer.requirements.memory_mb, self.capabilities.memory_mb),
                },
            };
        }

        // Job accepted
        let estimated_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        JobResponse::Accepted {
            job_id: offer.job_id,
            estimated_start,
            node_peer_id: self.local_peer_id.to_string(),
        }
    }

    /// Run the discovery event loop
    pub async fn run(&mut self) -> Result<()> {
        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(event)) => {
                    self.handle_behaviour_event(event).await?;
                }
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    info!("Listening on new address: {}", address);

                    // Log NAT traversal info
                    if address.to_string().contains("127.0.0.1") || address.to_string().contains("localhost") {
                        debug!("Local address - no NAT traversal needed");
                    } else if address.to_string().contains("0.0.0.0") {
                        info!("Listening on all interfaces - configure port forwarding for NAT traversal");
                        info!("Note: QUIC transport assists with NAT traversal");
                    } else {
                        info!("External address detected: {}", address);
                    }
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

    /// Handle combined behaviour events (Kademlia + mDNS)
    async fn handle_behaviour_event(&mut self, event: CombinedBehaviourEvent) -> Result<()> {
        match event {
            CombinedBehaviourEvent::Kademlia(kad_event) => {
                self.handle_kad_event(kad_event).await?;
            }
            CombinedBehaviourEvent::Mdns(mdns_event) => {
                self.handle_mdns_event(mdns_event).await?;
            }
        }
        Ok(())
    }

    /// Handle mDNS discovery events
    async fn handle_mdns_event(&mut self, event: mdns::Event) -> Result<()> {
        match event {
            mdns::Event::Discovered(list) => {
                for (peer_id, multiaddr) in list {
                    info!("mDNS discovered peer: {} at {}", peer_id, multiaddr);

                    // Add peer to Kademlia routing table
                    self.swarm.behaviour_mut().kademlia.add_address(&peer_id, multiaddr);
                }
            }
            mdns::Event::Expired(list) => {
                for (peer_id, multiaddr) in list {
                    debug!("mDNS peer expired: {} at {}", peer_id, multiaddr);
                }
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

    #[tokio::test]
    async fn test_discovery_creation() {
        let config = DiscoveryConfig::default();
        let discovery = Discovery::new(config);

        // mDNS may fail in restricted test environments due to netlink permissions
        // This is expected and acceptable - the daemon will work in production
        match discovery {
            Ok(_) => {
                // Success - full functionality available
            }
            Err(e) => {
                let error_msg = format!("{:?}", e);
                // If error is permission-related for network monitoring, it's acceptable
                if error_msg.contains("Permission denied") {
                    eprintln!("Note: mDNS disabled in test (needs network permissions)");
                } else {
                    panic!("Unexpected error creating discovery: {:?}", e);
                }
            }
        }
    }

    #[test]
    fn test_default_capabilities() {
        let caps = PeerCapabilities::default();
        assert!(!caps.arch.is_empty());
        assert!(caps.cpu_cores > 0);
    }
}
