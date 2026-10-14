//! Error types for the GemFactory precompile.

use alloy_primitives::{B256, U256};
use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the GemFactory runtime returns.
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

    #[error("only a genesis gem is issued without a floor")]
    ZeroFloorPrice,

    #[error("overflow")]
    Overflow,

    #[error("promis load must be positive")]
    ZeroPromisLoad,

    #[error("{0} undecodable")]
    UndecodableReturn(&'static str),

    #[error("gem bucket {bucket_key} has an empty member slot during forfeit")]
    EmptyMemberSlotDuringForfeit { bucket_key: B256 },

    #[error("gem bucket {bucket_key} member {gem_id} has no body during forfeit")]
    MissingMemberBodyDuringForfeit { bucket_key: B256, gem_id: U256 },

    #[error("gem bucket {bucket_key} indexes an ineligible member {gem_id}")]
    IneligibleBucketMember { bucket_key: B256, gem_id: U256 },

    #[error("gem call price overflow")]
    CallPriceOverflow,
}

impl From<GemFactoryError> for PrecompileError {
    fn from(value: GemFactoryError) -> Self {
        match value {
            fatal @ GemFactoryError::CallPriceOverflow => PrecompileError::Fatal(fatal.to_string()),
            other => PrecompileError::Revert(other.to_string()),
        }
    }
}

impl From<PowError> for GemFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
