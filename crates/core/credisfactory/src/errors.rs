use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisFactoryError {
    #[error("settlement amount is zero")]
    InvalidAmount,
    #[error("attached COEN must equal the pledged collateral exactly")]
    CcaStakeMismatch,
    #[error("reservation not found")]
    ReservationNotFound,
    #[error("reservation expired")]
    ReservationExpired,
    #[error("reservation cca mismatch")]
    ReservationCcaMismatch,
    #[error("previous closed UTC-day VWAP is unavailable")]
    PreviousDayVwapUnavailable,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
