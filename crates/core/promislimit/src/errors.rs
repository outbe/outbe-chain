//! Error types for the PromisLimit module.

use crate::precompile::IPromisLimit;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert errors that the PromisLimit runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promis limit overflow")]
    Overflow,
    #[error("promis limit underflow")]
    Underflow,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        use IPromisLimit as I;
        match err {
            PromisLimitError::Overflow => revert_with(I::Overflow {}),
            PromisLimitError::Underflow => revert_with(I::Underflow {}),
        }
    }
}
