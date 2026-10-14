//! Error types for the PromisLimit module.

use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert errors that the PromisLimit runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promislimit total_unallocated overflow")]
    TotalUnallocatedOverflow,
    #[error("promislimit total_unallocated underflow")]
    TotalUnallocatedUnderflow,
    #[error("certified carry-over activation binding mismatch")]
    ActivationBindingMismatch,
    #[error("invalid certified carry-over limit conservation")]
    InvalidLimitConservation,
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

/// Fatal errors that the PromisLimit runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitFatalError {
    #[error("invalid certified carry-over protocol value: {0}")]
    InvalidProtocolValue(#[from] ProtocolError),
}

impl From<PromisLimitFatalError> for PrecompileError {
    fn from(err: PromisLimitFatalError) -> Self {
        PrecompileError::Fatal(err.to_string())
    }
}
