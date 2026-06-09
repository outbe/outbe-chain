//! Module-local error types for the Credis position contract.
//!
//! Mirrors `outbe-cosmos/x/credis/types/errors.go`. Errors that are not
//! credis-specific (out-of-gas, generic revert) come from
//! `outbe_primitives::error::PrecompileError`.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisError {
    #[error("position not found")]
    PositionNotFound,
    #[error("position already exists")]
    PositionAlreadyExists,
    #[error("position already completed")]
    PositionCompleted,
    #[error("amount must be positive")]
    InvalidAmount,
    #[error("invalid anadosis number")]
    InvalidAnadosisNumber,
}

impl From<CredisError> for PrecompileError {
    fn from(err: CredisError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
