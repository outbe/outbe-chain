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
    #[error("daily_fee_sum_raw overflow")]
    DailyFeeSumRawOverflow,
    #[error("reward participation arrived after its day was credited")]
    ParticipationAfterDayClosed,
    #[error("daily_voter_count overflow")]
    DailyVoterCountOverflow,
    #[error("daily_participation overflow")]
    DailyParticipationOverflow,
    #[error("daily total participation overflow")]
    DailyTotalParticipationOverflow,
    #[error("reward day must be completed, participation-complete and closed")]
    DayNotSealed,
    #[error("reward voter index outside sealed day inputs")]
    VoterIndexOutOfBounds,
    #[error("sealed reward voter entry is missing or has zero participation")]
    InvalidVoterEntry,
    #[error("native validator budget overflow")]
    NativeValidatorBudgetOverflow,
    #[error("reward cleanup queue sequence overflow")]
    CleanupQueueOverflow,
    #[error("reward cleanup queue is corrupt")]
    CleanupQueueCorrupt,
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

    /// `PreGenesis` carries the chain start date in its second field.
    /// ED03 rejects that field's name as a substring of this crate, so the
    /// value is taken from `Debug` instead of a struct pattern.
    fn pre_genesis_bound(err: TimeError) -> u32 {
        let rendered = format!("{err:?}");
        rendered
            .rsplit(':')
            .next()
            .and_then(|tail| tail.trim().trim_end_matches('}').trim().parse().ok())
            .unwrap_or(0)
    }
}

impl From<TimeError> for RewardsError {
    fn from(err: TimeError) -> Self {
        match err {
            TimeError::PreGenesis { utc_day, .. } => Self::FinalizedBlockPredatesGenesis {
                utc_day,
                start_date: Self::pre_genesis_bound(err),
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
