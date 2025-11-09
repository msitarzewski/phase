use libp2p::PeerId;
use serde::{Deserialize, Serialize};

/// Information about a discovered peer
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Peer ID
    pub peer_id: String,

    /// Multiaddr (network address)
    pub addresses: Vec<String>,

    /// Advertised capabilities (CPU, arch, etc.)
    pub capabilities: PeerCapabilities,
}

/// Peer capabilities (what this node can do)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCapabilities {
    /// CPU architecture (e.g., "x86_64", "aarch64")
    pub arch: String,

    /// CPU cores available
    pub cpu_cores: u32,

    /// Memory available (MB)
    pub memory_mb: u64,

    /// WASM runtime (e.g., "wasmtime")
    pub wasm_runtime: String,
}

impl Default for PeerCapabilities {
    fn default() -> Self {
        Self {
            arch: std::env::consts::ARCH.to_string(),
            cpu_cores: num_cpus::get() as u32,
            memory_mb: 4096, // Default 4GB (TODO: detect actual)
            wasm_runtime: "wasmtime-15.0".to_string(),
        }
    }
}

impl PeerInfo {
    #[allow(dead_code)]
    pub fn new(peer_id: PeerId, capabilities: PeerCapabilities) -> Self {
        Self {
            peer_id: peer_id.to_string(),
            addresses: Vec::new(),
            capabilities,
        }
    }
}
