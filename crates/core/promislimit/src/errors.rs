//! Error types for the PromisLimit module.

use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the PromisLimit runtime returns. An overflow fails the block.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisLimitError {
    #[error("promis limit overflow")]
    Overflow,
    #[error("WCOEN refused the burn transfer")]
    WcoenBurnRefused,
}

impl From<PromisLimitError> for PrecompileError {
    fn from(err: PromisLimitError) -> Self {
        match err {
            PromisLimitError::Overflow => PrecompileError::FatalWithCause(Box::new(err)),
            PromisLimitError::WcoenBurnRefused => PrecompileError::RevertBytes(
                crate::precompile::IPromisLimit::WcoenBurnRefused {}
                    .abi_encode()
                    .into(),
            ),
        }
    }
}
