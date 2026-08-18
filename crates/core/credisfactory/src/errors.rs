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
    #[error(
        "cca is not permitted to originate: it must be registered, active, and out of quarantine"
    )]
    CcaCannotOriginate,
    #[error("owner has an unresolved called position")]
    OwnerHasCalledPosition,
    #[error("asset isoCode() call returned undecodable data")]
    AssetIsoUndecodable,
    #[error("asset balanceOf() call returned undecodable data")]
    AssetBalanceUndecodable,
    #[error("pledge quote has expired")]
    PledgeQuoteExpired,
    #[error("smart account does not hold matching funds for the requested credit")]
    UnmatchedFunding,
}

impl From<CredisFactoryError> for PrecompileError {
    fn from(err: CredisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
