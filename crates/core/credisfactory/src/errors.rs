//! Error types for the CredisFactory precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the CredisFactory runtime returns.
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
    #[error("only the Credis owner can forfeit it")]
    NotCredisOwner,
    #[error("timestamp exceeds u64")]
    TimestampOverflow,
    #[error("payoff overflow")]
    PayoffOverflow,
    #[error("asset decimals undecodable")]
    AssetDecimalsUndecodable,
    #[error("asset currency undecodable")]
    AssetCurrencyUndecodable,
    #[error("asset metadata changed")]
    AssetMetadataChanged,
    #[error("COEN stake overflow")]
    CoenStakeOverflow,
    #[error("Credis changed during issuance")]
    CredisChangedDuringIssuance,
    #[error("collateral increased")]
    CollateralIncreased,
    #[error("collateral release mismatch")]
    CollateralReleaseMismatch,
    #[error("Credis changed during payment")]
    CredisChangedDuringPayment,
    #[error("forfeiture collateral mismatch")]
    ForfeitureCollateralMismatch,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
