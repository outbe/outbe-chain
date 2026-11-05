use alloy_primitives::B256;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::TimeError;
use thiserror::Error;

alloy_sol_types::sol! {
    // Preserve Cycle's economic-allocation failure encoding after moving the calculation.
    error NativeValidatorBudgetOverflow();
}

/// Rewards module errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RewardsError {
    #[error("block_guard_ring_seq overflow")]
    BlockGuardRingSeqOverflow,
    #[error("reward window height overflow")]
    RewardWindowHeightOverflow,
    #[error("day_fee_sum_native overflow")]
    DayFeeSumOverflow,
    #[error("vote or fee arrived after its day was sealed")]
    VoteAfterDaySealed,
    #[error("day {day} is not after the last sealed day {last_sealed_day}")]
    DayAlreadySealed { day: u32, last_sealed_day: u32 },
    #[error("day_voter_count overflow")]
    DayVoterCountOverflow,
    #[error("day_votes overflow")]
    DayVotesOverflow,
    #[error("day_total_votes overflow")]
    DayTotalVotesOverflow,
    #[error("reward day must be completed, sealed and voting-complete")]
    DayNotSealed,
    #[error("reward voter index outside sealed day inputs")]
    VoterIndexOutOfBounds,
    #[error("sealed reward voter entry is missing or has zero participation")]
    InvalidVoterEntry,
    #[error("native validator budget overflow")]
    NativeValidatorBudgetOverflow,
    #[error("reward day cleanup queue sequence overflow")]
    DayCleanupQueueOverflow,
    #[error("reward day cleanup queue is corrupt")]
    CorruptDayCleanupQueue,
    #[error("reward credit outside inclusion window")]
    CreditOutsideInclusionWindow,
    #[error("late reward credit has no canonical UTC day")]
    LateCreditMissingCanonicalUtcDay,
    #[error("late_voter_count overflow")]
    LateVoterCountOverflow,
    #[error("block fee committee is empty")]
    BlockFeeEmptyCommittee,
    #[error("sum of voter portions exceeds the block fees")]
    SumOfVoterPortionsExceedsBlockFees,
    #[error("block fee sum overflow")]
    BlockFeeSumOverflow,
    #[error("transferred block fees plus rest do not equal the block fees")]
    BlockFeeInvariantMismatch,
    #[error("late payout multiply overflow")]
    LatePayoutMultiplyOverflow,
    #[error("late distributed overflow")]
    LateDistributedOverflow,
    #[error("Rewards precompile exposes no callable methods")]
    NoCallableMethods,
    #[error(
        "finalized block predates genesis: utc_day={utc_day}, \
         start_date={start_date}"
    )]
    FinalizedBlockPredatesGenesis { utc_day: u32, start_date: u32 },
    #[error("time helper error: {0}")]
    TimeHelper(String),
    #[error("date key {date_key} is before 20200101")]
    DateBeforeMinimum { date_key: u32 },
    #[error("date step from {date_key} does not fit u32")]
    DateOverflow { date_key: u32 },
    #[error("date step from {date_key} does not fit u64")]
    TimestampStepOverflow { date_key: u32 },
    #[error(
        "contradictory consensus metadata for fb_hash={fb_hash}: \
         stored fingerprint={stored}, new fingerprint={new}"
    )]
    ContradictoryConsensusMetadata {
        fb_hash: B256,
        stored: B256,
        new: B256,
    },
}

impl RewardsError {
    pub(crate) fn map_time_err(err: TimeError) -> PrecompileError {
        Self::from(err).into()
    }
}

impl From<TimeError> for RewardsError {
    fn from(err: TimeError) -> Self {
        match err {
            TimeError::PreGenesis {
                utc_day,
                start_date,
            } => Self::FinalizedBlockPredatesGenesis {
                utc_day,
                start_date,
            },
            TimeError::DateBeforeMinimum { date_key } => Self::DateBeforeMinimum { date_key },
            TimeError::DateOverflow { date_key } => Self::DateOverflow { date_key },
            TimeError::TimestampStepOverflow { date_key } => {
                Self::TimestampStepOverflow { date_key }
            }
            // `TimeError` is non_exhaustive. The three step variants above stay explicit.
            _ => Self::TimeHelper(err.to_string()),
        }
    }
}

impl From<RewardsError> for PrecompileError {
    fn from(err: RewardsError) -> Self {
        match err {
            RewardsError::NativeValidatorBudgetOverflow => {
                PrecompileError::RevertBytes(NativeValidatorBudgetOverflow {}.abi_encode().into())
            }
            other => PrecompileError::Revert(other.to_string()),
        }
    }
}
