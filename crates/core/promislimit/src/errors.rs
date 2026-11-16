//! Error types for the PromisLimit module.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the PromisLimit runtime returns. An overflow fails the block.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promis limit overflow")]
    Overflow,
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        PrecompileError::FatalWithCause(Box::new(err))
    }
}
