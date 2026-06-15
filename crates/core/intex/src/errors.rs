//! Module-local error types for the Intex runtime module.
//!
//! Errors that are not registry-specific come from
//! `outbe_primitives::error::PrecompileError`. Duplicate-series rejection is
//! handled by the storage DSL's record-level `create`, not a local variant.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntexError {
    #[error("series not found")]
    SeriesNotFound,
    #[error("issuedAt must be non-zero")]
    ZeroIssuedAt,
    #[error("invalid lifecycle state: expected {expected}, actual {actual}")]
    InvalidState { expected: u8, actual: u8 },
    #[error("invalid stored lifecycle state value: {0}")]
    InvalidStateValue(u8),
}

impl From<IntexError> for PrecompileError {
    fn from(err: IntexError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
