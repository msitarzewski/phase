use serde::{Deserialize, Serialize};

/// Job offer from client to node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOffer {
    /// Unique job ID
    pub job_id: String,

    /// Nonce for replay protection
    pub nonce: String,

    /// SHA-256 hash of WASM module
    pub module_hash: String,

    /// Resource requirements
    pub requirements: JobRequirements,
}

/// Resource requirements for a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequirements {
    /// CPU cores required
    pub cpu_cores: u32,

    /// Memory required (MB)
    pub memory_mb: u64,

    /// Timeout (seconds)
    pub timeout_seconds: u64,

    /// Required architecture (e.g., "x86_64", "aarch64")
    pub arch: String,

    /// Required WASM runtime
    pub wasm_runtime: String,
}

/// Response to job offer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobResponse {
    /// Job accepted - node will execute it
    Accepted {
        /// Job ID (matching offer)
        job_id: String,

        /// Estimated start time (unix timestamp)
        estimated_start: u64,

        /// Node's peer ID
        node_peer_id: String,
    },

    /// Job rejected - node cannot execute it
    Rejected {
        /// Job ID (matching offer)
        job_id: String,

        /// Rejection reason
        reason: RejectionReason,
    },
}

/// Reasons for rejecting a job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Resource requirements exceed node capacity
    InsufficientResources { missing: String },

    /// Architecture mismatch
    ArchMismatch { required: String, available: String },

    /// Runtime not supported
    RuntimeNotSupported { required: String },

    /// Node is at capacity
    QueueFull,

    /// Malformed request
    InvalidRequest { details: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_offer_serialization() {
        let offer = JobOffer {
            job_id: "test-job-123".to_string(),
            nonce: "nonce-abc".to_string(),
            module_hash: "sha256:abc123".to_string(),
            requirements: JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        };

        let json = serde_json::to_string(&offer).unwrap();
        let deserialized: JobOffer = serde_json::from_str(&json).unwrap();

        assert_eq!(offer.job_id, deserialized.job_id);
        assert_eq!(offer.module_hash, deserialized.module_hash);
    }

    #[test]
    fn test_job_response_accepted() {
        let response = JobResponse::Accepted {
            job_id: "test-job-123".to_string(),
            estimated_start: 1699564800,
            node_peer_id: "12D3KooW...".to_string(),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Accepted"));
    }

    #[test]
    fn test_job_response_rejected() {
        let response = JobResponse::Rejected {
            job_id: "test-job-123".to_string(),
            reason: RejectionReason::InsufficientResources {
                missing: "memory".to_string(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("Rejected"));
    }
}
