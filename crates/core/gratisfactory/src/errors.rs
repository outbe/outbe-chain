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
    #[error("asset does not report a decodable ISO 4217 code")]
    AssetIsoUndecodable,
    #[error("oracle conversion overflow")]
    OracleConversionOverflow,
    #[error("pledge cost exceeds maxGratis")]
    GratisCapExceeded,
    #[error("reservation not found")]
    ReservationNotFound,
    #[error("reservation expired")]
    ReservationExpired,
    #[error("reservation asset mismatch")]
    ReservationAssetMismatch,
    #[error("reservation amount is below the pledged credit")]
    ReservationInsufficient,
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
