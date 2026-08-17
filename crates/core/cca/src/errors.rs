//! Module-local error types for the CCA registry.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CcaError {
    #[error("cca is not registered")]
    NotRegistered,
    #[error("cca is already registered")]
    AlreadyRegistered,
    #[error("cca is not permitted to originate")]
    CannotOriginate,
    #[error("invalid cca state value: {0}")]
    InvalidStateValue(u8),
    #[error("invalid cca address")]
    InvalidAddress,
    #[error("cca arithmetic overflow")]
    ArithmeticOverflow,
}

impl From<CcaError> for PrecompileError {
    fn from(err: CcaError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
