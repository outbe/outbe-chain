//! Error types for the PromisLimit module.

use crate::precompile::IPromisLimit;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the PromisLimit runtime returns. An overflow fails the block.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promis limit overflow")]
    Overflow,
    #[error("promis limit underflow")]
    Underflow,
    #[error("unauthorized promis limit burn caller {caller} for source {burn_source}")]
    UnauthorizedBurnCaller { caller: Address, burn_source: u8 },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        use IPromisLimit as I;
        match err {
            err @ PromisLimitError::Overflow => PrecompileError::FatalWithCause(Box::new(err)),
            PromisLimitError::Underflow => revert_with(I::Underflow {}),
            PromisLimitError::UnauthorizedBurnCaller {
                caller,
                burn_source,
            } => revert_with(I::UnauthorizedBurnCaller {
                caller,
                source: burn_source,
            }),
        }
    }
}
