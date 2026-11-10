//! Shared error types for `outbe-common`.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Proof-of-work errors. Factories map these variants onto their own error enums.
/// The enum stays exhaustive, so a new variant requires an explicit mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PowError {
    /// The computed hash does not have `POW_DIFFICULTY` leading zero bytes.
    #[error("insufficient proof of work")]
    InsufficientProofOfWork,
}

/// Errors that [`crate::settlement`] returns while flooring a cost into asset units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RoundingError {
    #[error("settlement asset has unsupported decimals {0}")]
    UnsupportedDecimals(u8),
    #[error("settlement rate denominator is zero")]
    ZeroRate,
    #[error("settlement conversion overflow")]
    Overflow,
    /// A positive cost floors to zero.
    #[error("settlement cost rounds to zero")]
    RoundsToZero,
}

impl From<RoundingError> for PrecompileError {
    fn from(err: RoundingError) -> Self {
        Self::Revert(err.to_string())
    }
}
