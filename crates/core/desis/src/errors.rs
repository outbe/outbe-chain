//! Error types for the Desis precompile.

use crate::precompile::IDesis;
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::{error::PrecompileError, time::EmissionDate};
use thiserror::Error;

/// Errors that the Desis runtime returns.
#[derive(Debug, Error)]
pub enum DesisError {
    #[error("invalid emission day: {0}")]
    InvalidEmissionDay(EmissionDate),

    #[error("invalid stage transition")]
    InvalidStageTransition,

    #[error("pending clearing data missing for series {0}")]
    PendingClearingDataMissing(EmissionDate),

    #[error("unauthorized origin: {0}")]
    UnauthorizedOrigin(Address),

    #[error("winning bid references an unpriced currency: {0}")]
    UnpricedReferenceCurrency(u16),

    #[error("chain relayed {0} bidders, more than the refund fan-out can carry")]
    RefundFanOutTooLarge(usize),

    #[error("relayed bid names a currency no series id can spell: issuance {0}, reference {1}")]
    UnspellableBidCurrency(u16, u16),

    #[error("relayed bid batch carries {0} bids, over the {1} the codec may send")]
    BidBatchTooLarge(usize, usize),

    #[error("Desis Allocation {allocation} exceeds Desis Limit {limit} for emission day {day}")]
    DesisAllocationExceedsLimit {
        day: EmissionDate,
        allocation: U256,
        limit: U256,
    },

    #[error("auction brief limit exceeds Desis u128 domain")]
    AuctionBriefLimitExceedsDomain,

    #[error("processBidsBatch: array length mismatch")]
    BatchArrayLengthMismatch,

    #[error("brief anchor exceeds u32")]
    BriefAnchorExceedsU32,

    #[error("min bid quantity scaling overflow")]
    MinBidQuantityScalingOverflow,

    #[error("targetsOf undecodable")]
    TargetsOfUndecodable,

    #[error("processBidsBatch: batch total mismatch")]
    BatchTotalMismatch,

    #[error("processBidsDone: invalid total batches")]
    InvalidTotalBatches,

    #[error("processBidsBatch: invalid batch index/total")]
    InvalidBatchIndexOrTotal,

    #[error("schedule timestamp exceeds u32")]
    ScheduleTimestampExceedsU32,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => DesisError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

impl DesisError {}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<DesisError> for PrecompileError {
    fn from(e: DesisError) -> Self {
        use IDesis as I;
        match e {
            DesisError::InvalidEmissionDay(day) => revert_with(I::InvalidEmissionDay {
                emissionDay: day.value(),
            }),
            DesisError::InvalidStageTransition => revert_with(I::InvalidStageTransition {}),
            DesisError::PendingClearingDataMissing(day) => {
                revert_with(I::PendingClearingDataMissing {
                    emissionDay: day.value(),
                })
            }
            DesisError::UnauthorizedOrigin(value) => revert_with(I::UnauthorizedOrigin { value }),
            DesisError::UnpricedReferenceCurrency(value) => {
                revert_with(I::UnpricedReferenceCurrency { value })
            }
            DesisError::RefundFanOutTooLarge(value) => revert_with(I::RefundFanOutTooLarge {
                value: u64::try_from(value).unwrap_or(u64::MAX),
            }),
            DesisError::UnspellableBidCurrency(issuance, reference_currency) => {
                revert_with(I::UnspellableBidCurrency {
                    issuance,
                    referenceCurrency: reference_currency,
                })
            }
            DesisError::BidBatchTooLarge(count, max) => revert_with(I::BidBatchTooLarge {
                count: u64::try_from(count).unwrap_or(u64::MAX),
                max: u64::try_from(max).unwrap_or(u64::MAX),
            }),
            DesisError::DesisAllocationExceedsLimit {
                day,
                allocation,
                limit,
            } => revert_with(I::DesisAllocationExceedsLimit {
                emissionDay: day.value(),
                allocation,
                limit,
            }),
            DesisError::AuctionBriefLimitExceedsDomain => {
                revert_with(I::AuctionBriefLimitExceedsDomain {})
            }
            DesisError::BatchArrayLengthMismatch => revert_with(I::BatchArrayLengthMismatch {}),
            DesisError::BriefAnchorExceedsU32 => revert_with(I::BriefAnchorExceedsU32 {}),
            DesisError::MinBidQuantityScalingOverflow => {
                revert_with(I::MinBidQuantityScalingOverflow {})
            }
            DesisError::TargetsOfUndecodable => revert_with(I::TargetsOfUndecodable {}),
            DesisError::BatchTotalMismatch => revert_with(I::BatchTotalMismatch {}),
            DesisError::InvalidTotalBatches => revert_with(I::InvalidTotalBatches {}),
            DesisError::InvalidBatchIndexOrTotal => revert_with(I::InvalidBatchIndexOrTotal {}),
            DesisError::ScheduleTimestampExceedsU32 => {
                revert_with(I::ScheduleTimestampExceedsU32 {})
            }
            DesisError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
