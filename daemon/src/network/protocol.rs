use serde::{Deserialize, Serialize};

/// Job offer from client to node
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Complete job request with WASM payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    /// Unique job ID
    pub job_id: String,

    /// SHA-256 hash of WASM module
    pub module_hash: String,

    /// WASM module bytes
    #[serde(with = "serde_bytes_base64")]
    pub wasm_bytes: Vec<u8>,

    /// Arguments to pass to WASM
    pub args: Vec<String>,

    /// Resource requirements
    pub requirements: JobRequirements,
}

/// Job execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    /// Job ID (matching request)
    pub job_id: String,

    /// Standard output
    pub stdout: String,

    /// Standard error
    pub stderr: String,

    /// Exit code (0 = success)
    pub exit_code: u32,

    /// Execution receipt (JSON)
    pub receipt_json: String,
}

/// Helper module for base64 encoding of bytes (for JSON compatibility)
mod serde_bytes_base64 {
    use serde::{Deserialize, Deserializer, Serializer};
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

impl JobRequest {
    /// Create a new job request
    pub fn new(
        job_id: String,
        module_hash: String,
        wasm_bytes: Vec<u8>,
        args: Vec<String>,
        requirements: JobRequirements,
    ) -> Self {
        Self {
            job_id,
            module_hash,
            wasm_bytes,
            args,
            requirements,
        }
    }

    /// Validate the job request
    pub fn validate(&self) -> Result<(), String> {
        if self.job_id.is_empty() {
            return Err("job_id cannot be empty".to_string());
        }
        if self.wasm_bytes.is_empty() {
            return Err("wasm_bytes cannot be empty".to_string());
        }
        if self.requirements.cpu_cores < 1 {
            return Err("cpu_cores must be at least 1".to_string());
        }
        Ok(())
    }
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

    #[test]
    fn test_job_request_serialization() {
        let wasm_bytes = vec![0x00, 0x61, 0x73, 0x6d]; // WASM magic bytes
        let request = JobRequest::new(
            "job-456".to_string(),
            "sha256:def456".to_string(),
            wasm_bytes.clone(),
            vec!["arg1".to_string(), "arg2".to_string()],
            JobRequirements {
                cpu_cores: 2,
                memory_mb: 256,
                timeout_seconds: 60,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: JobRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(request.job_id, deserialized.job_id);
        assert_eq!(request.module_hash, deserialized.module_hash);
        assert_eq!(request.wasm_bytes, deserialized.wasm_bytes);
        assert_eq!(request.args, deserialized.args);
    }

    #[test]
    fn test_job_request_validation() {
        let valid = JobRequest::new(
            "job-789".to_string(),
            "sha256:abc".to_string(),
            vec![0x00, 0x61, 0x73, 0x6d],
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );
        assert!(valid.validate().is_ok());

        let empty_id = JobRequest::new(
            "".to_string(),
            "sha256:abc".to_string(),
            vec![0x00],
            vec![],
            JobRequirements {
                cpu_cores: 1,
                memory_mb: 128,
                timeout_seconds: 30,
                arch: "x86_64".to_string(),
                wasm_runtime: "wasmtime-27".to_string(),
            },
        );
        assert!(empty_id.validate().is_err());
    }

    #[test]
    fn test_job_result_serialization() {
        let result = JobResult {
            job_id: "job-result-1".to_string(),
            stdout: "Hello, world!".to_string(),
            stderr: "".to_string(),
            exit_code: 0,
            receipt_json: r#"{"version":"0.1","module_hash":"sha256:abc"}"#.to_string(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: JobResult = serde_json::from_str(&json).unwrap();

        assert_eq!(result.job_id, deserialized.job_id);
        assert_eq!(result.stdout, deserialized.stdout);
        assert_eq!(result.exit_code, deserialized.exit_code);
    }
}
