//! Error types for the IntexFactory precompile.

use crate::precompile::IIntexFactory;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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
    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => IntexFactoryError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

impl From<outbe_common::pow::PowError> for IntexFactoryError {
    fn from(value: outbe_common::pow::PowError) -> Self {
        match value {
            outbe_common::pow::PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<IntexFactoryError> for PrecompileError {
    fn from(err: IntexFactoryError) -> Self {
        use IIntexFactory as I;
        match err {
            IntexFactoryError::ZeroAddress => revert_with(I::ZeroAddress {}),
            IntexFactoryError::ZeroAmount => revert_with(I::ZeroAmount {}),
            IntexFactoryError::ZeroUnits => revert_with(I::ZeroUnits {}),
            IntexFactoryError::SettlementDeadlinePassed => {
                revert_with(I::SettlementDeadlinePassed {})
            }
            IntexFactoryError::ZeroBalance => revert_with(I::ZeroBalance {}),
            IntexFactoryError::UnitsExceedBalance => revert_with(I::UnitsExceedBalance {}),
            IntexFactoryError::InsufficientSettled => revert_with(I::InsufficientSettled {}),
            IntexFactoryError::InsufficientProofOfWork => {
                revert_with(I::InsufficientProofOfWork {})
            }
            IntexFactoryError::NotOriginRouter => revert_with(I::NotOriginRouter {}),
            IntexFactoryError::ForfeitedPromisLoadOverflow => {
                revert_with(I::ForfeitedPromisLoadOverflow {})
            }
            IntexFactoryError::NoCalledGroupForTest => revert_with(I::NoCalledGroupForTest {}),
            IntexFactoryError::IssueForTestCurrencyCountMismatch => {
                revert_with(I::IssueForTestCurrencyCountMismatch {})
            }
            IntexFactoryError::BlockTimestampExceedsU32 => {
                revert_with(I::BlockTimestampExceedsU32 {})
            }
            IntexFactoryError::IssuanceChunkCountExceedsU16 => {
                revert_with(I::IssuanceChunkCountExceedsU16 {})
            }
            IntexFactoryError::PriceExceedsWireType => revert_with(I::PriceExceedsWireType {}),
            IntexFactoryError::MarkedUpPriceOverflow => revert_with(I::MarkedUpPriceOverflow {}),
            IntexFactoryError::OwnerUnitsExceedU32 => revert_with(I::OwnerUnitsExceedU32 {}),
            IntexFactoryError::SettledUnitsExceedU32 => revert_with(I::SettledUnitsExceedU32 {}),
            IntexFactoryError::NftBalanceExceedsU32 => revert_with(I::NftBalanceExceedsU32 {}),
            IntexFactoryError::NftBalanceOfUndecodable => {
                revert_with(I::NftBalanceOfUndecodable {})
            }
            IntexFactoryError::RouterAnsweredNoLegs => revert_with(I::RouterAnsweredNoLegs {}),
            IntexFactoryError::PromisOverflow => revert_with(I::PromisOverflow {}),
            IntexFactoryError::MiningSequenceOverflow => revert_with(I::MiningSequenceOverflow {}),
            IntexFactoryError::ExercisedUnitsExceedSeries => {
                revert_with(I::ExercisedUnitsExceedSeries {})
            }
            IntexFactoryError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
