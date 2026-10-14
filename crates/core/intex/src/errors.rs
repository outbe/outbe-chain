//! Module-local error types for the Intex runtime module.
//!
//! Errors that are not registry-specific come from
//! `outbe_primitives::error::PrecompileError`. The storage DSL's record-level
//! `create` rejects a duplicate series, so no local variant exists for it.

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
    #[error("invalid series id components")]
    InvalidSeriesId,
    #[error("realized units exceed the issued count for series")]
    RealizedUnitsOverflow,
    #[error("proceeds deadline cannot be the finalized marker")]
    ProceedsDeadlineFinalizedMarker,
}

impl From<IntexError> for PrecompileError {
    fn from(err: IntexError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
