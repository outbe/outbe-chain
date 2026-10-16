//! Error types for the Radicle manager, sidecar, and endpoint network.

use crate::endpoint::MAX_ADDRESSES;
use thiserror::Error;

/// Phase errors that the Radicle manager reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PhaseError {
    /// The validator sidecar is unavailable before readiness.
    #[error("validator sidecar is unavailable before readiness")]
    SidecarUnavailable,
    /// The local Heartwood `NodeId` is unavailable.
    #[error("local Heartwood NodeId is unavailable")]
    LocalNodeIdUnavailable,
    /// The local Heartwood `NodeId` does not match the finalized `ValidatorSet` binding.
    #[error("local Heartwood NodeId does not match finalized ValidatorSet binding")]
    BindingMismatch,
}

/// Runtime errors returned by the Radicle manager and its adapters.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ManagerError {
    /// The finality feed returned an error.
    #[error("finality source failed: {0}")]
    Finality(String),
    /// The finalized state provider returned an error.
    #[error("exact finalized provider failed: {0}")]
    Provider(String),
    /// The finalized state snapshot is inconsistent.
    #[error("finalized state is inconsistent: {0}")]
    Snapshot(String),
    /// Endpoint discovery failed.
    #[error("endpoint discovery failed: {0}")]
    Endpoint(String),
    /// Heartwood control failed.
    #[error("Heartwood control failed: {0}")]
    Control(String),
    /// The sidecar status query failed.
    #[error("sidecar status failed: {0}")]
    Status(String),
    /// The manager stopped.
    #[error("manager stopped")]
    Stopped,
    /// A component exceeded its shutdown deadline.
    #[error("{0} shutdown deadline exceeded")]
    ShutdownDeadline(&'static str),
    /// A background Radicle task failed.
    #[error("Radicle task failed: {0}")]
    Task(String),
    /// Both manager shutdown and endpoint shutdown failed.
    #[error("manager shutdown failed: {manager}; endpoint shutdown failed: {endpoint}")]
    Shutdown {
        /// The manager shutdown error.
        manager: Box<ManagerError>,
        /// The endpoint shutdown error.
        endpoint: Box<ManagerError>,
    },
}

impl ManagerError {
    /// Creates [`ManagerError::Finality`] from a displayable error.
    pub fn finality(error: impl std::fmt::Display) -> Self {
        Self::Finality(error.to_string())
    }

    /// Creates [`ManagerError::Provider`] from a displayable error.
    pub fn provider(error: impl std::fmt::Display) -> Self {
        Self::Provider(error.to_string())
    }

    /// Creates [`ManagerError::Snapshot`] from a displayable error.
    pub fn snapshot(error: impl std::fmt::Display) -> Self {
        Self::Snapshot(error.to_string())
    }

    /// Creates [`ManagerError::Endpoint`] from a displayable error.
    pub fn endpoint(error: impl std::fmt::Display) -> Self {
        Self::Endpoint(error.to_string())
    }

    /// Creates [`ManagerError::Control`] from a displayable error.
    pub fn control(error: impl std::fmt::Display) -> Self {
        Self::Control(error.to_string())
    }

    /// Creates [`ManagerError::Status`] from a displayable error.
    pub fn status(error: impl std::fmt::Display) -> Self {
        Self::Status(error.to_string())
    }

    /// Creates [`ManagerError::Task`] from a displayable error.
    pub fn task(error: impl std::fmt::Display) -> Self {
        Self::Task(error.to_string())
    }

    /// Creates a [`ManagerError::Task`] for an endpoint service failure.
    pub fn endpoint_service_task(error: impl std::fmt::Display) -> Self {
        Self::task(RadicleTaskError::EndpointService(error.to_string()))
    }

    /// Creates a [`ManagerError::Task`] when both endpoint stop and join fail.
    pub fn endpoint_stop_and_join_failed(
        stop: impl std::fmt::Display,
        join: impl std::fmt::Display,
    ) -> Self {
        Self::task(RadicleTaskError::EndpointStopAndJoinFailed {
            stop: stop.to_string(),
            join: join.to_string(),
        })
    }

    /// Creates a [`ManagerError::Task`] for an endpoint actor failure.
    pub fn endpoint_actor_task(error: impl std::fmt::Display) -> Self {
        Self::task(RadicleTaskError::EndpointActor(error.to_string()))
    }

    /// Creates a [`ManagerError::Task`] when both endpoint actor run and cleanup fail.
    pub fn endpoint_actor_cleanup_failed(
        error: impl std::fmt::Display,
        cleanup: impl std::fmt::Display,
    ) -> Self {
        Self::task(RadicleTaskError::EndpointActorCleanupAlsoFailed {
            error: error.to_string(),
            cleanup: cleanup.to_string(),
        })
    }

    /// Creates a [`ManagerError::Task`] for a manager task failure.
    pub fn manager_task(error: impl std::fmt::Display) -> Self {
        Self::task(RadicleTaskError::Manager(error.to_string()))
    }
}

/// Typed task lifecycle failures wrapped by [`ManagerError::Task`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum RadicleTaskError {
    /// The endpoint task owner mutex was poisoned.
    #[error("endpoint owner poisoned")]
    EndpointOwnerPoisoned,
    /// The endpoint task was already installed.
    #[error("endpoint already installed")]
    EndpointAlreadyInstalled,
    /// The endpoint drain was already claimed.
    #[error("endpoint drain already claimed")]
    EndpointDrainAlreadyClaimed,
    /// The endpoint service task failed.
    #[error("endpoint service: {0}")]
    EndpointService(String),
    /// Both stopping and joining the endpoint service failed.
    #[error("{stop}; endpoint join also failed: {join}")]
    EndpointStopAndJoinFailed {
        /// The stop error message.
        stop: String,
        /// The join error message.
        join: String,
    },
    /// The endpoint actor task failed.
    #[error("endpoint actor: {0}")]
    EndpointActor(String),
    /// Both endpoint actor execution and cleanup failed.
    #[error("{error}; endpoint actor cleanup also failed: {cleanup}")]
    EndpointActorCleanupAlsoFailed {
        /// The primary error message.
        error: String,
        /// The cleanup error message.
        cleanup: String,
    },
    /// The manager task failed.
    #[error("manager: {0}")]
    Manager(String),
}

impl From<RadicleTaskError> for ManagerError {
    fn from(error: RadicleTaskError) -> Self {
        Self::Task(error.to_string())
    }
}

/// Typed endpoint lifecycle failures wrapped by [`ManagerError::Endpoint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EndpointLifecycleError {
    /// The shutdown command mailbox is closed.
    #[error("shutdown mailbox closed")]
    ShutdownMailboxClosed,
    /// The shutdown acknowledgement channel closed before replying.
    #[error("shutdown acknowledgement closed")]
    ShutdownAckClosed,
    /// Refreshing endpoints exceeded the handle deadline.
    #[error("endpoint refresh deadline exceeded")]
    RefreshDeadlineExceeded,
    /// The endpoint actor stopped unexpectedly.
    #[error("endpoint actor stopped unexpectedly")]
    ActorStoppedUnexpectedly,
    /// The endpoint actor join handle was missing.
    #[error("endpoint actor join missing")]
    ActorJoinMissing,
}

impl From<EndpointLifecycleError> for ManagerError {
    fn from(error: EndpointLifecycleError) -> Self {
        Self::Endpoint(error.to_string())
    }
}

/// Typed Heartwood control failures wrapped by [`ManagerError::Control`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum HeartwoodControlReason {
    /// The Heartwood control call exceeded its deadline.
    #[error("Heartwood control deadline exceeded")]
    DeadlineExceeded,
    /// A peer has no signed endpoint to connect to.
    #[error("peer has no signed endpoint")]
    PeerHasNoSignedEndpoint,
    /// The Heartwood control socket returned an empty response.
    #[error("empty control response")]
    EmptyResponse,
    /// The Heartwood `NodeId` is invalid.
    #[error("invalid Heartwood NodeId")]
    InvalidNodeId,
    /// The Heartwood address is invalid.
    #[error("invalid Heartwood address")]
    InvalidAddress,
    /// The IPv6 endpoint is invalid.
    #[error("invalid IPv6 endpoint")]
    InvalidIpv6Endpoint,
    /// The DNS endpoint is invalid.
    #[error("invalid DNS endpoint")]
    InvalidDnsEndpoint,
    /// The canonical endpoint encoding is invalid.
    #[error("invalid canonical endpoint encoding")]
    InvalidCanonicalEndpointEncoding,
}

impl From<HeartwoodControlReason> for ManagerError {
    fn from(error: HeartwoodControlReason) -> Self {
        Self::Control(error.to_string())
    }
}

/// Typed repository status failures wrapped by [`ManagerError::Status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum RepositoryStatusReason {
    /// The repository status endpoint is not a loopback address.
    #[error("repository status endpoint must be loopback")]
    NonLoopbackEndpoint,
    /// The repository status response did not match the requested identity.
    #[error("repository status identity mismatch")]
    IdentityMismatch,
}

impl From<RepositoryStatusReason> for ManagerError {
    fn from(error: RepositoryStatusReason) -> Self {
        Self::Status(error.to_string())
    }
}

/// Typed snapshot inconsistency failures wrapped by [`ManagerError::Snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SnapshotInconsistencyReason {
    /// Forward and reverse Radicle bindings in `ValidatorSet` disagree.
    #[error("Radicle reverse binding mismatch for {0}")]
    ReverseBindingMismatch(alloy_primitives::Address),
}

impl From<SnapshotInconsistencyReason> for ManagerError {
    fn from(error: SnapshotInconsistencyReason) -> Self {
        Self::Snapshot(error.to_string())
    }
}

/// Typed finality feed failures wrapped by [`ManagerError::Finality`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FinalityFeedReason {
    /// The finality subscription mutex was poisoned.
    #[error("finality subscription lock poisoned")]
    SubscriptionLockPoisoned,
    /// The finality feed was already subscribed.
    #[error("finality feed already subscribed")]
    AlreadySubscribed,
}

impl From<FinalityFeedReason> for ManagerError {
    fn from(error: FinalityFeedReason) -> Self {
        Self::Finality(error.to_string())
    }
}

/// Errors returned when querying the Heartwood control socket.
#[derive(Debug, Error)]
pub enum SidecarError {
    /// A Unix socket I/O operation failed.
    #[error("Heartwood control command failed: {0}")]
    Io(#[from] std::io::Error),
    /// The control command exceeded its deadline.
    #[error("Heartwood control command exceeded its deadline")]
    Deadline,
    /// The control socket returned a malformed response.
    #[error("Heartwood control response is malformed: {0}")]
    Malformed(String),
    /// The Heartwood `NodeId` is invalid.
    #[error("Heartwood NodeId is invalid")]
    NodeId,
    /// The `externalAddresses` list is empty, oversized, or contains duplicates.
    #[error("Heartwood externalAddresses must contain 1..={MAX_ADDRESSES} unique addresses")]
    Addresses,
}

impl SidecarError {
    /// Creates [`SidecarError::Malformed`] from a displayable error.
    pub fn malformed(error: impl std::fmt::Display) -> Self {
        Self::Malformed(error.to_string())
    }

    /// Creates [`SidecarError::Malformed`] for an invalid external address.
    pub fn invalid_external_address(value: impl std::fmt::Display) -> Self {
        Self::malformed(SidecarMalformedError::InvalidExternalAddress(
            value.to_string(),
        ))
    }
}

/// Typed malformed-response reasons wrapped by [`SidecarError::Malformed`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SidecarMalformedError {
    /// The `externalAddresses` field is missing from the config response.
    #[error("missing externalAddresses")]
    MissingExternalAddresses,
    /// An entry in `externalAddresses` is not a JSON string.
    #[error("non-string external address")]
    NonStringExternalAddress,
    /// An entry in `externalAddresses` could not be parsed as an endpoint address.
    #[error("invalid external address {0}")]
    InvalidExternalAddress(String),
}

impl From<SidecarMalformedError> for SidecarError {
    fn from(error: SidecarMalformedError) -> Self {
        Self::Malformed(error.to_string())
    }
}

/// Errors returned when reading finalized state into precompile storage.
#[derive(Debug, Error)]
pub enum SnapshotStateReadError {
    /// Reading a storage slot from the finalized state provider failed.
    #[error("finalized state read failed: {0}")]
    FinalizedStateReadFailed(String),
}

impl From<SnapshotStateReadError> for outbe_primitives::error::PrecompileError {
    fn from(error: SnapshotStateReadError) -> Self {
        Self::Storage(error.to_string())
    }
}
