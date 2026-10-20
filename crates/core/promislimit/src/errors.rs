//! Error types for the PromisLimit module.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert errors that the PromisLimit runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promis limit overflow")]
    PromisLimitOverflow,
    #[error("promis limit underflow")]
    PromisLimitUnderflow,
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
