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
    #[error("proceeds from an unexpected caller {0}")]
    UnauthorizedProceedsCaller(alloy_primitives::Address),
    #[error("proceeds from a chain {0} that the day did not start on")]
    UnexpectedProceedsSource(u32),
    #[error("proceeds from a sender other than IntexAuctionTargetRouter")]
    UnauthorizedProceedsSender(alloy_primitives::Bytes),
    #[error("proceeds data does not decode as (emissionDay, pageIndex)")]
    UndecodableProceedsData,
    #[error("WCOEN refused the proceeds transfer")]
    ProceedsTransferRefused,
    #[error("chain id exceeds u32")]
    ChainIdExceedsU32,
    #[error("message of type {0} breaks its wire limits")]
    InvalidWireMessage(u8),

    #[error("forfeited promis load overflow")]
    ForfeitedPromisLoadOverflow,

    #[error("Intex series page count exceeds u16")]
    IntexSeriesPageCountExceedsU16,

    #[error("price exceeds the wire type")]
    PriceExceedsWireType,

    #[error("marked-up price overflow")]
    MarkedUpPriceOverflow,

    #[error("settled units exceed u32")]
    SettledUnitsExceedU32,

    #[error("NFT balanceOf undecodable")]
    NftBalanceOfUndecodable,

    #[error("promis overflow")]
    PromisOverflow,

    #[error("mining sequence overflow")]
    MiningSequenceOverflow,

    #[error("mined units exceed the series")]
    MinedUnitsExceedSeries,
    #[error("intex is not Issued")]
    IntexNotIssued,
    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_system_common::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => IntexFactoryError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

impl From<outbe_core_common::pow::PowError> for IntexFactoryError {
    fn from(value: outbe_core_common::pow::PowError) -> Self {
        match value {
            outbe_core_common::pow::PowError::InsufficientProofOfWork => {
                Self::InsufficientProofOfWork
            }
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
            IntexFactoryError::UnauthorizedProceedsCaller(caller) => {
                revert_with(I::UnauthorizedProceedsCaller { caller })
            }
            IntexFactoryError::UnexpectedProceedsSource(src_chain_id) => {
                revert_with(I::UnexpectedProceedsSource {
                    srcChainId: src_chain_id,
                })
            }
            IntexFactoryError::UnauthorizedProceedsSender(from) => {
                revert_with(I::UnauthorizedProceedsSender { from })
            }
            IntexFactoryError::UndecodableProceedsData => {
                revert_with(I::UndecodableProceedsData {})
            }
            IntexFactoryError::ProceedsTransferRefused => {
                revert_with(I::ProceedsTransferRefused {})
            }
            IntexFactoryError::ChainIdExceedsU32 => revert_with(I::ChainIdExceedsU32 {}),
            IntexFactoryError::InvalidWireMessage(msg_type) => {
                revert_with(I::InvalidWireMessage { msgType: msg_type })
            }
            IntexFactoryError::ForfeitedPromisLoadOverflow => {
                revert_with(I::ForfeitedPromisLoadOverflow {})
            }
            IntexFactoryError::IntexSeriesPageCountExceedsU16 => {
                revert_with(I::IntexSeriesPageCountExceedsU16 {})
            }
            IntexFactoryError::PriceExceedsWireType => revert_with(I::PriceExceedsWireType {}),
            IntexFactoryError::MarkedUpPriceOverflow => revert_with(I::MarkedUpPriceOverflow {}),
            IntexFactoryError::SettledUnitsExceedU32 => revert_with(I::SettledUnitsExceedU32 {}),
            IntexFactoryError::NftBalanceOfUndecodable => {
                revert_with(I::NftBalanceOfUndecodable {})
            }
            IntexFactoryError::PromisOverflow => revert_with(I::PromisOverflow {}),
            IntexFactoryError::MiningSequenceOverflow => revert_with(I::MiningSequenceOverflow {}),
            IntexFactoryError::MinedUnitsExceedSeries => revert_with(I::MinedUnitsExceedSeries {}),
            IntexFactoryError::IntexNotIssued => revert_with(I::IntexNotIssued {}),
            IntexFactoryError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
