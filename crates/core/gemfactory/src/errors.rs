use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemFactoryError {
    #[error("gem not found")]
    GemNotFound,

    #[error("not gem owner")]
    NotGemOwner,

    #[error("invalid state for action")]
    InvalidState,

    #[error("merchant flow deferred")]
    MerchantDeferred,

    #[error("invalid asset")]
    InvalidAsset,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("nonce exceeds uint64 range")]
    NonceExceedsUint64Range,

    #[error("issuance currency {iso_code} is not registered")]
    IssuanceCurrencyNotRegistered { iso_code: u16 },

    #[error("issuance currency {issuance} must equal reference currency {reference}")]
    IssuanceReferenceMismatch { issuance: u16, reference: u16 },

    #[error("oracle nominal unavailable")]
    OracleUnavailable,

    #[error("invalid owner")]
    InvalidOwner,

    #[error("overflow")]
    Overflow,
}

impl From<GemFactoryError> for PrecompileError {
    fn from(value: GemFactoryError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

impl From<PowError> for GemFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::NonceExceedsUint64Range => Self::NonceExceedsUint64Range,
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
