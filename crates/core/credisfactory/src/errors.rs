use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisFactoryError {
    #[error("invalid asset address")]
    InvalidAsset,
    #[error("invalid smart account address")]
    InvalidSmartAccount,
    #[error("anadosis amount is zero")]
    InvalidAmount,
    #[error("position is already fully paid")]
    PositionCompleted,
    #[error("address has overdue anadosis")]
    OverduePayments,
    #[error("asset isoCode() call returned undecodable data")]
    AssetIsoUndecodable,
    #[error("position has not reached its credis expiry")]
    NotExpired,
    #[error("position has no outstanding balance to expire")]
    NothingOutstanding,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
