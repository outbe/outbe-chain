//! Error types for the EmissionLimit system precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the EmissionLimit runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[repr(u8)]
#[non_exhaustive]
pub enum EmissionLimitError {
    /// Indicates that fixed-share multiplication overflowed.
    #[error("emission fixed-share multiplication overflow")]
    FixedShareMultiplicationOverflow = 1,
    /// Indicates that the fixed allocation total overflowed.
    #[error("emission fixed allocation total overflow")]
    FixedAllocationTotalOverflow = 2,
    /// Indicates that terminal allocation calculation underflowed.
    #[error("emission terminal allocation underflow")]
    TerminalAllocationUnderflow = 3,
    /// Indicates that fixed percentage summation overflowed.
    #[error("emission fixed percentage overflow")]
    FixedPercentageOverflow = 4,
    /// Indicates that the sink table defines more than one terminal sink.
    #[error("emission sink table must have one terminal sink")]
    MultipleTerminalSinks = 5,
    /// Indicates that the terminal sink is not the final entry in the table.
    #[error("emission terminal sink must be the final sink")]
    TerminalSinkNotFinal = 6,
    /// Indicates that fixed sink percentages exceed 100.
    #[error("emission fixed percentages exceed 100")]
    FixedPercentagesExceed100 = 7,
    /// Indicates that the terminal emission sink returned an unused amount.
    #[error("terminal emission sink returned unused amount")]
    TerminalSinkReturnedUnusedAmount = 8,
    /// Indicates that the late-settlement terminal sink returned an unused amount.
    #[error("late-settlement terminal sink returned unused amount")]
    LateSettlementSinkReturnedUnusedAmount = 9,
}

impl From<EmissionLimitError> for PrecompileError {
    fn from(err: EmissionLimitError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
