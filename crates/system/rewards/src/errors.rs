use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::TimeError;
use thiserror::Error;

/// Rewards module errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RewardsError {
    #[error("settled-day cleanup queue position overflow")]
    SettledDayCleanupQueueOverflow,
    #[error("inconsistent settled-day cleanup queue: {0}")]
    InvalidSettledDayCleanupQueue(&'static str),
    #[error("day settlement requires closed participation and completed inclusion windows")]
    DaySettlementNotReady,
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
            _ => Self::TimeHelper(err.to_string()),
        }
    }
}

impl From<RewardsError> for PrecompileError {
    fn from(err: RewardsError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
