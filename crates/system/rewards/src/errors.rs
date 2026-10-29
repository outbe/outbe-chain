use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::TimeError;
use thiserror::Error;

/// Rewards module errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RewardsError {
    #[error("block_guard_ring_seq overflow")]
    BlockGuardRingSeqOverflow,
    #[error("reward window height overflow")]
    RewardWindowHeightOverflow,
    #[error("daily_fee_sum_raw overflow")]
    DailyFeeSumRawOverflow,
    #[error("reward participation arrived after its day was credited")]
    ParticipationAfterDayClosed,
    #[error("daily_voter_count overflow")]
    DailyVoterCountOverflow,
    #[error("daily_participation overflow")]
    DailyParticipationOverflow,
    #[error("reward credit outside inclusion window")]
    CreditOutsideInclusionWindow,
    #[error("late reward credit has no canonical UTC day")]
    LateCreditMissingCanonicalUtcDay,
    #[error("late_voter_count overflow")]
    LateVoterCountOverflow,
    #[error("late settle denominator is zero (empty committee)")]
    LateSettleDenominatorZero,
    #[error("late settle insolvent: distributed exceeds escrow")]
    LateSettleInsolvent,
    #[error("late parity overflow")]
    LateParityOverflow,
    #[error("late settle parity violated")]
    LateSettleParityViolated,
    #[error("late payout multiply overflow")]
    LatePayoutMultiplyOverflow,
    #[error("late distributed overflow")]
    LateDistributedOverflow,
    #[error("late residue dust overflow")]
    LateResidueDustOverflow,
    #[error("Rewards precompile exposes no callable methods")]
    NoCallableMethods,
    #[error(
        "Rewards.genesis_utc_day not initialized - \
         RewardsLifecycle::begin_block did not run on block 0"
    )]
    GenesisUtcDayNotInitialized,
    #[error(
        "finalized block predates genesis: utc_day={utc_day}, \
         genesis_utc_day={genesis_utc_day}"
    )]
    FinalizedBlockPredatesGenesis { utc_day: u32, genesis_utc_day: u32 },
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

impl From<TimeError> for RewardsError {
    fn from(err: TimeError) -> Self {
        match err {
            TimeError::PreGenesis {
                utc_day,
                genesis_utc_day,
            } => Self::FinalizedBlockPredatesGenesis {
                utc_day,
                genesis_utc_day,
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
        PrecompileError::Revert(err.to_string())
    }
}
