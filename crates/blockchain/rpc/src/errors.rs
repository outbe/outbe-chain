//! Error types for the Outbe JSON-RPC server.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors returned by RPC state helpers and status builders.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RpcError {
    /// A Radicle status counter does not fit in `u64`.
    #[error("Radicle status counter exceeds u64")]
    RadicleCounterOverflow,
    /// An OCOMP Lysis openings builder returned an error.
    #[error("{0}")]
    LysisOpenings(String),
}

impl RpcError {
    /// Converts any displayable failure into [`RpcError::LysisOpenings`].
    pub fn lysis_openings(error: impl std::fmt::Display) -> Self {
        Self::LysisOpenings(error.to_string())
    }
}

/// Errors that occur when a registered validator record is incomplete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum RpcValidatorStateError {
    /// A registered validator record has no consensus public key.
    #[error("registered validator is missing consensus pubkey")]
    MissingConsensusPubkey,
    /// A registered validator record has no history record.
    #[error("registered validator is missing history")]
    MissingHistory,
    /// A registered validator record has no persisted lifecycle status.
    #[error("registered validator has no persisted status")]
    MissingPersistedStatus,
}

impl From<RpcValidatorStateError> for PrecompileError {
    fn from(error: RpcValidatorStateError) -> Self {
        Self::Fatal(error.to_string())
    }
}
