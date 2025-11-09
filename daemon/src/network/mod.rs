pub mod discovery;
pub mod peer;
pub mod protocol;

pub use discovery::{Discovery, DiscoveryConfig};
pub use peer::{PeerInfo, PeerCapabilities};
pub use protocol::{JobOffer, JobResponse, JobRequirements, RejectionReason};
