//! Error types for the IntexFactory precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the IntexFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IntexFactoryError {
    #[error("zero address")]
    ZeroAddress,
    #[error("amount must be positive")]
    ZeroAmount,
    #[error("units must be positive")]
    ZeroUnits,
    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,
    #[error("zero balance")]
    ZeroBalance,
    #[error("units exceed balance")]
    UnitsExceedBalance,
    #[error("insufficient settled balance")]
    InsufficientSettled,
    #[error("insufficient proof of work")]
    InsufficientProofOfWork,
    #[error("caller is not the origin router")]
    NotOriginRouter,

    #[error("forfeited promis load overflow")]
    ForfeitedPromisLoadOverflow,

    #[error("closeCallNoticeForTest: no called group")]
    NoCalledGroupForTest,

    #[error("issueForTest: a currency per series")]
    IssueForTestCurrencyCountMismatch,

    #[error("block timestamp exceeds u32")]
    BlockTimestampExceedsU32,

    #[error("issuance chunk count exceeds u16")]
    IssuanceChunkCountExceedsU16,

    #[error("price exceeds the wire type")]
    PriceExceedsWireType,

    #[error("marked-up price overflow")]
    MarkedUpPriceOverflow,

    #[error("owner units exceed u32")]
    OwnerUnitsExceedU32,

    #[error("settled units exceed u32")]
    SettledUnitsExceedU32,

    #[error("NFT balance exceeds u32")]
    NftBalanceExceedsU32,

    #[error("NFT balanceOf undecodable")]
    NftBalanceOfUndecodable,

    #[error("the router answered no legs")]
    RouterAnsweredNoLegs,

    #[error("promis overflow")]
    PromisOverflow,

    #[error("mining sequence overflow")]
    MiningSequenceOverflow,

    #[error("exercised units exceed the series")]
    ExercisedUnitsExceedSeries,
}

impl From<outbe_common::pow::PowError> for IntexFactoryError {
    fn from(value: outbe_common::pow::PowError) -> Self {
        match value {
            outbe_common::pow::PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}

impl From<IntexFactoryError> for PrecompileError {
    fn from(err: IntexFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
