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

    #[error("settlement asset {asset} is not registered for settlement currency {expected}")]
    SettlementCurrencyMismatch {
        asset: alloy_primitives::Address,
        expected: u16,
    },

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("issuance currency {iso_code} is not registered")]
    IssuanceCurrencyNotRegistered { iso_code: u16 },

    #[error("{currency} is not an ISO 4217 currency code")]
    InvalidCurrency { currency: u16 },

    #[error("settlement asset {asset} has {decimals} decimals, expected {expected}")]
    SettlementDecimalsMismatch {
        asset: alloy_primitives::Address,
        decimals: u8,
        expected: u8,
    },

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
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
