mod lifecycle;
mod metrics;
mod network;
mod sidecar;
mod status;

pub use lifecycle::EndpointTaskOwner;
pub use metrics::RadicleMetrics;
pub use network::{
    EndpointEvidenceHandle, EndpointNetwork, EndpointNetworkService, LocalEndpointIdentity,
    LocalEndpointIdentityChannel, LocalEndpointIdentityHandle, LocalEndpointIdentityPublisher,
    SignedEndpointEvidence, shutdown_bounded,
};
pub use sidecar::{SidecarError, SidecarInfo, query_sidecar};
pub use status::{
    ExactBindingState, GenesisFallbackFinalizedFeed, ObservedRepositoryStatus,
    ObservedSnapshotReader, RadicleRepositorySnapshot, RadicleRepositoryState,
    RadicleStatusChannel, RadicleStatusHandle, RadicleStatusPublisher, RadicleStatusSnapshot,
    RadicleVotingGate, RadicleVotingGateError, VotingGateInput, evaluate_voting_gate,
};

pub const PRODUCTION_REPAIR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
