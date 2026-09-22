use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisFactoryError {
    #[error("invalid asset address")]
    InvalidAsset,
    #[error("invalid smart account address")]
    InvalidSmartAccount,
    #[error("settlement amount is zero")]
    InvalidAmount,
    #[error("asset isoCode() call returned undecodable data")]
    AssetIsoUndecodable,
    #[error("smart account is not deployed")]
    SmartAccountNotDeployed,
    #[error("attached COEN must equal the pledged collateral exactly")]
    CcaStakeMismatch,
    #[error("reservation not found")]
    ReservationNotFound,
    #[error("reservation expired")]
    ReservationExpired,
    #[error("reservation account mismatch")]
    ReservationAccountMismatch,
    #[error("reservation cca mismatch")]
    ReservationCcaMismatch,
    #[error("reservation asset mismatch")]
    ReservationAssetMismatch,
    #[error("reservation amount is below the pledged credit")]
    ReservationInsufficient,
    #[error("previous closed UTC-day VWAP is unavailable")]
    PreviousDayVwapUnavailable,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
