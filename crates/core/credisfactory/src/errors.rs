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
    #[error("previous closed UTC-day VWAP is unavailable")]
    PreviousDayVwapUnavailable,
    #[error("timestamp exceeds u64")]
    TimestampOverflow,
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
    #[error("ERC20 transfer failed")]
    Erc20TransferFailed,
    #[error("ERC20 approval failed")]
    Erc20ApprovalFailed,
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
