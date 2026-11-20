//! Error types for the Desis precompile.

use crate::constants::MAX_TARGET_CHAINS;
use crate::precompile::IDesis;
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_system_common::time::EmissionDate;
use thiserror::Error;

/// Errors that the Desis runtime returns.
#[derive(Debug, Error)]
pub enum DesisError {
    #[error("invalid intake cleanup state")]
    InvalidCleanupState,

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

    #[error("chain relayed {0} bidders, more than the winners pages can carry")]
    ReturnFanOutTooLarge(usize),

    #[error("relayed bid names a currency no series id can spell: issuance {0}, reference {1}")]
    UnspellableIntexAuctionBidCurrency(u16, u16),

    #[error("relayed bids page carries {0} bids, over the {1} the codec may send")]
    IntexAuctionBidsPageTooLarge(usize, usize),

    #[error("Desis Allocation {allocation} exceeds Desis Limit {limit} for emission day {day}")]
    DesisAllocationExceedsLimit {
        day: EmissionDate,
        allocation: U256,
        limit: U256,
    },

    #[error("collectIntexAuctionBids: array length mismatch")]
    PageArrayLengthMismatch,

    #[error("brief anchor exceeds u32")]
    DesisLimitAnchorExceedsU32,

    #[error("min bid quantity scaling overflow")]
    MinIntexAuctionBidQuantityScalingOverflow,

    #[error("targetsOf undecodable")]
    TargetsOfUndecodable,

    #[error("emission day {0} has {1} targets, more than the cap")]
    TooManyTargets(EmissionDate, usize),

    #[error("collectIntexAuctionBids: page total mismatch")]
    PageTotalMismatch,

    #[error("completeIntexAuctionBids: invalid total pages")]
    InvalidTotalPages,

    #[error("collectIntexAuctionBids: invalid page index/total")]
    InvalidPageIndexOrTotal,

    #[error("schedule timestamp exceeds u32")]
    ScheduleTimestampExceedsU32,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(
    err: outbe_system_common::time::TimeError,
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
            DesisError::InvalidCleanupState => PrecompileError::Fatal(e.to_string()),
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
            DesisError::ReturnFanOutTooLarge(value) => revert_with(I::ReturnFanOutTooLarge {
                value: u64::try_from(value).unwrap_or(u64::MAX),
            }),
            DesisError::UnspellableIntexAuctionBidCurrency(issuance, reference_currency) => {
                revert_with(I::UnspellableIntexAuctionBidCurrency {
                    issuance,
                    referenceCurrency: reference_currency,
                })
            }
            DesisError::IntexAuctionBidsPageTooLarge(count, max) => {
                revert_with(I::IntexAuctionBidsPageTooLarge {
                    count: u64::try_from(count).unwrap_or(u64::MAX),
                    max: u64::try_from(max).unwrap_or(u64::MAX),
                })
            }
            DesisError::DesisAllocationExceedsLimit {
                day,
                allocation,
                limit,
            } => revert_with(I::DesisAllocationExceedsLimit {
                emissionDay: day.value(),
                allocation,
                limit,
            }),
            DesisError::PageArrayLengthMismatch => revert_with(I::PageArrayLengthMismatch {}),
            DesisError::DesisLimitAnchorExceedsU32 => revert_with(I::DesisLimitAnchorExceedsU32 {}),
            DesisError::MinIntexAuctionBidQuantityScalingOverflow => {
                revert_with(I::MinIntexAuctionBidQuantityScalingOverflow {})
            }
            DesisError::TargetsOfUndecodable => revert_with(I::TargetsOfUndecodable {}),
            DesisError::TooManyTargets(day, count) => revert_with(I::TooManyTargets {
                emissionDay: day.value(),
                count: count as u64,
                max: MAX_TARGET_CHAINS as u64,
            }),
            DesisError::PageTotalMismatch => revert_with(I::PageTotalMismatch {}),
            DesisError::InvalidTotalPages => revert_with(I::InvalidTotalPages {}),
            DesisError::InvalidPageIndexOrTotal => revert_with(I::InvalidPageIndexOrTotal {}),
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
