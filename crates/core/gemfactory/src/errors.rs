use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemFactoryError {
    #[error("gem not found")]
    GemNotFound,

    #[error("invalid state for action")]
    InvalidState,

    #[error("call notice period expired")]
    DeadlineExpired,

    #[error("unsupported gem type")]
    UnsupportedGemType,

    #[error("source intex not found")]
    SourceIntexNotFound,

    #[error("source intex is not issued")]
    SourceIntexNotIssued,

    #[error("position not found")]
    PositionNotFound,

    #[error("only position owner can issue merchant gem")]
    NotPositionOwner,

    #[error("position already exists")]
    PositionAlreadyExists,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("insufficient factory capacity")]
    InsufficientCapacity,

    #[error("position expired")]
    PositionExpired,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("{currency} is not an ISO 4217 currency code")]
    InvalidCurrency { currency: u16 },

    #[error("oracle nominal unavailable")]
    OracleUnavailable,

    #[error("invalid owner")]
    InvalidOwner,

    #[error("overflow")]
    Overflow,

    #[error("promis load must be positive")]
    ZeroPromisLoad,
}

impl From<GemFactoryError> for PrecompileError {
    fn from(value: GemFactoryError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

impl From<PowError> for GemFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
