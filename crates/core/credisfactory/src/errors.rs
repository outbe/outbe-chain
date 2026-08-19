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
    #[error("owner has an unresolved called position")]
    OwnerHasCalledPosition,
    #[error("asset isoCode() call returned undecodable data")]
    AssetIsoUndecodable,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
