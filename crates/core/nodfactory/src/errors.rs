use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodFactoryError {
    #[error("invalid owner")]
    InvalidOwner,

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("nod not found")]
    NodNotFound,

    #[error("not the owner")]
    NotOwner,

    #[error("nod is not qualified")]
    NodNotQualified,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("nonce exceeds uint64 range")]
    NonceExceedsUint64Range,

    #[error("invalid asset")]
    InvalidAsset,
}

impl From<NodFactoryError> for PrecompileError {
    fn from(value: NodFactoryError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

impl From<PowError> for NodFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::NonceExceedsUint64Range => Self::NonceExceedsUint64Range,
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
