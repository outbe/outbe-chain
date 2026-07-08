use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisFactoryError {
    #[error("invalid asset address")]
    InvalidAsset,
    #[error("invalid vault provider address")]
    InvalidVaultProvider,
    #[error("invalid bundle account address")]
    InvalidBundleAccount,
    #[error("anadosis amount is zero")]
    InvalidAmount,
    #[error("caller is not the position bundleAccount")]
    UnauthorizedCaller,
    #[error("position is already fully paid")]
    PositionCompleted,
    #[error("address has overdue anadosis")]
    OverduePayments,
    #[error("oracle COEN/USD rate unavailable")]
    OracleRateUnavailable,
    #[error("oracle COEN/USD rate too small (rounds to zero at 1e18 precision)")]
    OracleRateTooSmall,
    #[error("oracle conversion overflow")]
    OracleConversionOverflow,
    #[error("asset isoCode() call returned undecodable data")]
    AssetIsoUndecodable,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
