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
    #[error("reward participation arrived after GEM batch preparation")]
    ParticipationAfterGemBatchPreparation,
    #[error("daily_voter_count overflow")]
    DailyVoterCountOverflow,
    #[error("daily_participation overflow")]
    DailyParticipationOverflow,
    #[error("daily_total_participation overflow")]
    DailyTotalParticipationOverflow,
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
    #[error("{0}")]
    RewardGemRetryable(#[from] RewardGemRetryableReason),
    #[error("GEM preparation before reward participation windows close")]
    GemPreparationBeforeParticipationWindowsClose,
}

/// Retryable validator reward Gem batch failure reasons.
#[derive(Debug, Error)]
pub enum RewardGemRetryableReason {
    #[error("validator reward Gem FIFO head exceeds tail")]
    FifoHeadExceedsTail,
    #[error(
        "validator reward Gem pending batch count {pending_batch_count} disagrees with FIFO span {span}"
    )]
    PendingBatchCountDisagreesWithSpan { pending_batch_count: u64, span: u64 },
    #[error("validator reward Gem currency is not registered: {0}")]
    CurrencyNotRegistered(PrecompileError),
    #[error(
        "validator reward Gem UTC day {reward_utc_day} delivered {delivered} of {recipient_count} recipients"
    )]
    DeliveredRecipientOutOfBounds {
        reward_utc_day: u32,
        delivered: u32,
        recipient_count: u32,
    },
    #[error("validator reward Gem delivered count overflow")]
    DeliveredCountOverflow,
    #[error("validator reward Gem FIFO head {head} has no reward UTC day")]
    FifoHeadMissingUtcDay { head: u64 },
    #[error("validator reward Gem FIFO sequence overflow")]
    FifoSequenceOverflow,
    #[error("validator reward Gem empty FIFO has a live UTC day at sequence {head}")]
    EmptyFifoHasLiveUtcDay { head: u64 },
    #[error("validator reward Gem FIFO reverse index disagrees for UTC day {reward_utc_day}")]
    FifoReverseIndexDisagrees { reward_utc_day: u32 },
    #[error("validator reward Gem FIFO UTC day {reward_utc_day} has an illegal state")]
    FifoUtcDayIllegalState { reward_utc_day: u32 },
    #[error(
        "validator reward Gem UTC day {reward_utc_day} has invalid recipient count {recipient_count}"
    )]
    InvalidRecipientCount {
        reward_utc_day: u32,
        recipient_count: u32,
    },
    #[error("validator reward Gem UTC day {reward_utc_day} total disagrees with its preparation")]
    TotalDisagreesWithPreparation { reward_utc_day: u32 },
    #[error("validator reward Gem UTC day {reward_utc_day} digest disagrees with its preparation")]
    DigestDisagreesWithPreparation { reward_utc_day: u32 },
    #[error(
        "validator reward Gem UTC day {reward_utc_day} has invalid currencies {issuance_currency}/{reference_currency}"
    )]
    InvalidCurrencies {
        reward_utc_day: u32,
        issuance_currency: u16,
        reference_currency: u16,
    },
    #[error(
        "validator reward Gem UTC day {reward_utc_day} has an empty recipient at index {index}"
    )]
    EmptyRecipientAtIndex { reward_utc_day: u32, index: u32 },
    #[error("validator reward Gem delivery total overflow")]
    DeliveryTotalOverflow,
    #[error("validator reward Gem pending batch count underflow")]
    PendingBatchCountUnderflow,
    #[error("validator reward participation total overflow")]
    ParticipationTotalOverflow,
    #[error("validator reward Gem owner is zero")]
    OwnerIsZero,
    #[error("validator reward planned total overflow")]
    PlannedTotalOverflow,
    #[error("validator reward share multiply overflow")]
    ShareMultiplyOverflow,
    #[error("validator reward Gem batch exceeds validator bound: count={count} max={max}")]
    BatchExceedsValidatorBound { count: usize, max: u32 },
    #[error("validator reward Gem recipient count overflow")]
    RecipientCountOverflow,
    #[error("validator reward Gem UTC day {utc_day} is settled without a preparation")]
    SettledWithoutPreparation { utc_day: u32 },
    #[error("validator reward Gem pending batch count overflow")]
    PendingBatchCountOverflow,
    #[error("validator reward Gem UTC day {utc_day} is already present in the FIFO")]
    AlreadyPresentInFifo { utc_day: u32 },
    #[error("validator reward Gem recipient index overflow")]
    RecipientIndexOverflow,
    #[error("validator reward Gem no-payable replay has corrupt state for UTC day {utc_day}")]
    NoPayableReplayCorruptState { utc_day: u32 },
    #[error("validator reward Gem delivered replay retains live state for UTC day {utc_day}")]
    DeliveredReplayRetainsLiveState { utc_day: u32 },
    #[error("validator reward Gem preparation replay contradicts UTC day {utc_day}")]
    PreparationReplayContradictsUtcDay { utc_day: u32 },
    #[error("validator reward Gem preparation replay has corrupt metadata for UTC day {utc_day}")]
    PreparationReplayCorruptMetadata { utc_day: u32 },
    #[error(
        "validator reward Gem preparation replay has corrupt recipient count for UTC day {utc_day}"
    )]
    PreparationReplayCorruptRecipientCount { utc_day: u32 },
    #[error("validator reward Gem preparation replay lost FIFO linkage for UTC day {utc_day}")]
    PreparationReplayLostFifoLinkage { utc_day: u32 },
    #[error(
        "validator reward Gem preparation replay has corrupt FIFO linkage for UTC day {utc_day}"
    )]
    PreparationReplayCorruptFifoLinkage { utc_day: u32 },
    #[error(
        "validator reward Gem preparation replay has corrupt recipient {index} for UTC day {utc_day}"
    )]
    PreparationReplayCorruptRecipient { index: u32, utc_day: u32 },
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
