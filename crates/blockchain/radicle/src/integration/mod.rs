pub mod bootstrap;
mod lifecycle;
pub mod local;
mod material;
mod metrics;
mod network;
pub mod publication;
pub mod review;
mod sidecar;
pub mod signing;
mod status;

#[cfg(test)]
mod bootstrap_probe;

pub use lifecycle::EndpointTaskOwner;
pub use material::RepositoryMaterialPublisher;
pub use metrics::RadicleMetrics;
pub use network::{
    shutdown_bounded, EndpointEvidenceHandle, EndpointNetwork, EndpointNetworkService,
    EndpointSigningIdentity, EndpointTransport, LocalEndpointIdentity,
    LocalEndpointIdentityChannel, LocalEndpointIdentityHandle, LocalEndpointIdentityPublisher,
    SignedEndpointEvidence,
};
pub use sidecar::{query_sidecar, SidecarError, SidecarInfo};
pub use status::{
    evaluate_voting_gate, ExactBindingState, GenesisFallbackFinalizedFeed,
    ObservedRepositoryStatus, ObservedSnapshotReader, RadicleRepositorySnapshot,
    RadicleRepositoryState, RadicleStatusChannel, RadicleStatusHandle, RadicleStatusPublisher,
    RadicleStatusSnapshot, RadicleVotingGate, RadicleVotingGateError, VotingGateInput,
};

pub const PRODUCTION_REPAIR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
