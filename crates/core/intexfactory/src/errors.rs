//! Module-local error types. Other errors come from
//! `outbe_primitives::error::PrecompileError`.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntexFactoryError {
    #[error("zero address")]
    ZeroAddress,
    #[error("amount must be positive")]
    ZeroAmount,
    #[error("dependency not wired")]
    NotWired,
    #[error("series not found")]
    SeriesNotFound,
    #[error("series not settleable in state {0}")]
    NotSettleable(u8),
    #[error("settlement deadline expired")]
    DeadlineExpired,
    #[error("zero balance")]
    ZeroBalance,
    #[error("amount exceeds balance")]
    AmountExceedsBalance,
    #[error("caller not authorized to settle for holder")]
    NotAuthorized,
    #[error("insufficient settled balance")]
    InsufficientSettled,
    #[error("insufficient proof of work")]
    InsufficientProofOfWork,
    #[error("zero shares received from vault")]
    ZeroSharesReceived,
    #[error("caller is not the origin messenger")]
    NotOriginMessenger,
    #[error("no contributors recorded for series {0}")]
    NoContributors(u32),
    #[error("no in-flight distribution for series {0}")]
    NoDistribution(u32),
}

impl From<IntexFactoryError> for PrecompileError {
    fn from(err: IntexFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
