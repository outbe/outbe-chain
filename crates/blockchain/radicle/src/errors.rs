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
