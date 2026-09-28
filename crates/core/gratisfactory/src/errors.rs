use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisFactoryError {
    #[error("fidelity index not eligible")]
    FidelityNotEligible,
    #[error("invalid asset address")]
    InvalidAsset,
    #[error("pledge amount is zero")]
    InvalidAmount,
    #[error("pledge entry price rounds to zero")]
    EntryPriceZero,
    #[error("asset does not report a decodable ISO 4217 code")]
    AssetIsoUndecodable,
    #[error("pledge valuation price unavailable")]
    PledgePriceUnavailable,
    #[error("asset has no registered Reserve vault")]
    ReserveVaultUnavailable,
    #[error("asset does not report decodable decimals")]
    AssetDecimalsUndecodable,
    #[error("unsupported pledge asset decimals")]
    UnsupportedAssetDecimals,
    #[error("pledge cost exceeds maxGratis")]
    GratisCapExceeded,
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
