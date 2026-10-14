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
    #[error("amount must be positive")]
    ZeroIssueTerm,
    #[error("pledge source is zero")]
    InvalidSource,
    #[error("Credis owner is zero")]
    InvalidOwner,
    #[error("Credis already exists")]
    CredisAlreadyExists,
    #[error("Credis is closed")]
    CredisClosed,
    #[error("payment is below the interest accrued since the last settlement")]
    PaymentBelowAccruedInterest,
    #[error("Credis is not called")]
    NotCalled,
    #[error("settlement deadline has not passed")]
    SettlementDeadlineNotPassed,
    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,
    #[error("Credis has no outstanding principal")]
    NothingOutstanding,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
