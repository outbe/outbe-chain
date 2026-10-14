//! Module-local error types for the Credis contract.
//!
//! Errors that are not credis-specific (out-of-gas, generic revert) come from
//! `outbe_primitives::error::PrecompileError`.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisError {
    #[error("Credis not found")]
    CredisNotFound,
    #[error("invalid Credis state value: {0}")]
    InvalidStateValue(u8),
    #[error("credis arithmetic overflow")]
    ArithmeticOverflow,
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("Credis is non-transferable")]
    NonTransferable,
}

impl From<CredisError> for PrecompileError {
    fn from(err: CredisError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
