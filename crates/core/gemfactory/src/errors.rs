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

    #[error("call notice period expired")]
    DeadlineExpired,

    #[error("unsupported gem type")]
    UnsupportedGemType,

    #[error("source intex not found")]
    SourceIntexNotFound,

    #[error("position not found")]
    PositionNotFound,

    #[error("only position owner can mint merchant gem")]
    NotPositionOwner,

    #[error("position already exists")]
    PositionAlreadyExists,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("insufficient factory capacity")]
    InsufficientCapacity,

    #[error("position expired")]
    PositionExpired,

    #[error("invalid asset")]
    InvalidAsset,

    #[error("settlement asset iso {asset} does not match settlement currency {expected}")]
    SettlementCurrencyMismatch { asset: u16, expected: u16 },

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
