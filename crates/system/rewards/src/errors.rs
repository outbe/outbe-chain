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
    DayFeeSumNativeOverflow,
    #[error("reward participation arrived after its day was sealed")]
    ParticipationAfterDaySealed,
    #[error("day_voter_count overflow")]
    DayVoterCountOverflow,
    #[error("day_votes overflow")]
    DayVotesOverflow,
    #[error("day total votes overflow")]
    DayTotalVotesOverflow,
    #[error("reward day must be valid, completed and voting-complete before sealing")]
    DayNotReadyToSeal,
    #[error("reward day is already sealed")]
    DayAlreadySealed,
    #[error("reward day must be completed, participation-complete and closed")]
    DayNotSealed,
    #[error("reward voter index outside sealed day inputs")]
    VoterIndexOutOfBounds,
    #[error("sealed reward voter entry is missing or has zero participation")]
    InvalidVoterEntry,
    #[error("native validator budget overflow")]
    NativeValidatorBudgetOverflow,
    #[error("reward cleanup queue sequence overflow")]
    DayCleanupQueueOverflow,
    #[error("reward cleanup queue is corrupt")]
    CorruptDayCleanupQueue,
    #[error("reward credit outside inclusion window")]
    CreditOutsideInclusionWindow,
    #[error("late reward credit has no canonical UTC day")]
    LateCreditMissingCanonicalUtcDay,
    #[error("late_voter_count overflow")]
    LateVoterCountOverflow,
    #[error("block fee committee is empty")]
    BlockFeeEmptyCommittee,
    #[error("sum of voter portions exceeds block fees")]
    SumOfVoterPortionsExceedsBlockFees,
    #[error("block fee sum overflow")]
    BlockFeeSumOverflow,
    #[error("block fee invariant mismatch")]
    BlockFeeInvariantMismatch,
    #[error("block fee portion multiply overflow")]
    BlockFeePortionOverflow,
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

/// Storage-level finalization and late-credit failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinalizationError {
    /// Signer bitmap length does not match the committee.
    #[error("consensus metadata signer bitmap length mismatch")]
    SignerBitmapLengthMismatch,
    /// Committee lists the same address twice.
    #[error("consensus metadata committee contains duplicate addresses")]
    DuplicateCommitteeAddresses,
    /// A committee address is not a registered validator.
    #[error("consensus metadata committee member is not a registered validator: {address}")]
    UnregisteredCommitteeMember {
        /// Missing validator.
        address: alloy_primitives::Address,
    },
    /// A missed proposer is outside the finalized committee.
    #[error(
        "consensus metadata missed proposer is not in finalized committee: {validator} (view {view})"
    )]
    MissedProposerNotInCommittee {
        /// Claimed missed proposer.
        validator: alloy_primitives::Address,
        /// View in which the proposer was missed.
        view: u64,
    },
    /// A bitmap entry is neither 0 nor 1.
    #[error("consensus metadata signer bitmap contains non-binary entry")]
    NonBinarySignerBitmapEntry,
    /// The executor did not preload the parent execution summary.
    #[error(
        "missing preloaded execution summary for finalized block {block_number} ({block_hash})"
    )]
    MissingPreloadedExecutionSummary {
        /// Finalized parent height.
        block_number: u64,
        /// Finalized parent hash.
        block_hash: B256,
    },
    /// An identical replay observed a different accounting height.
    #[error(
        "CertifiedParentAccounting identical replay at unexpected progress: last_accounted={last_accounted}, expected={expected}"
    )]
    IdenticalReplayUnexpectedProgress {
        /// Stored accounting height.
        last_accounted: u64,
        /// Height this replay must match.
        expected: u64,
    },
    /// Fresh metadata does not follow the stored height by one.
    #[error(
        "CertifiedParentAccounting progress gap: last_accounted={last_accounted}, expected_previous={expected_previous}, accounting_parent={accounting_parent}"
    )]
    ProgressGap {
        /// Stored accounting height.
        last_accounted: u64,
        /// Height that must already be stored.
        expected_previous: u64,
        /// Parent height being accounted.
        accounting_parent: u64,
    },
    /// Phase 1 ran before block 2.
    #[error("CertifiedParentAccounting system tx requires block_number >= 2")]
    RequiresBlockNumberAtLeastTwo,
    /// Block number subtraction underflowed.
    #[error("block number underflow")]
    BlockNumberUnderflow,
    /// Metadata does not name the immediate parent.
    #[error(
        "CertifiedParentAccounting metadata must target immediate parent: expected {expected}, got {actual}"
    )]
    TargetImmediateParentMismatch {
        /// Immediate parent height.
        expected: u64,
        /// Height named by the metadata.
        actual: u64,
    },
    /// Finalized parent hash is zero.
    #[error("CertifiedParentAccounting metadata has zero finalized block hash")]
    ZeroFinalizedBlockHash,
    /// Credit epoch does not match the escrow.
    #[error(
        "LateFinalizeCredits: epoch mismatch for fb_number {fb_number} (escrow {escrowed_epoch}, credit {credit_epoch})"
    )]
    LateCreditEpochMismatch {
        /// Finalized block number.
        fb_number: u64,
        /// Escrowed epoch.
        escrowed_epoch: u64,
        /// Epoch named by the credit.
        credit_epoch: u64,
    },
    /// Credit committee hash does not match the escrow.
    #[error(
        "LateFinalizeCredits: committee_set_hash mismatch for fb_number {fb_number} (escrow {escrowed_csh}, credit {credit_csh})"
    )]
    LateCreditCommitteeSetHashMismatch {
        /// Finalized block number.
        fb_number: u64,
        /// Escrowed committee hash.
        escrowed_csh: B256,
        /// Hash named by the credit.
        credit_csh: B256,
    },
    /// Credit view does not match the escrow.
    #[error(
        "LateFinalizeCredits: view mismatch for fb_number {fb_number} (escrow {escrowed_view}, credit {credit_view})"
    )]
    LateCreditViewMismatch {
        /// Finalized block number.
        fb_number: u64,
        /// Escrowed view.
        escrowed_view: u64,
        /// View named by the credit.
        credit_view: u64,
    },
    /// Credit parent view does not match the escrow.
    #[error(
        "LateFinalizeCredits: parent_view mismatch for fb_number {fb_number} (escrow {escrowed_parent_view}, credit {credit_parent_view})"
    )]
    LateCreditParentViewMismatch {
        /// Finalized block number.
        fb_number: u64,
        /// Escrowed parent view.
        escrowed_parent_view: u64,
        /// Parent view named by the credit.
        credit_parent_view: u64,
    },
    /// No escrow exists for the credited block.
    #[error("LateFinalizeCredits: no escrow for fb_number {fb_number} (credit fb_hash {fb_hash})")]
    LateCreditMissingEscrow {
        /// Finalized block number.
        fb_number: u64,
        /// Hash named by the credit.
        fb_hash: B256,
    },
    /// Escrowed hash does not match the credit.
    #[error(
        "LateFinalizeCredits: fb_hash mismatch for fb_number {fb_number} (escrow {escrowed_hash}, credit {credit_hash})"
    )]
    LateCreditFbHashMismatch {
        /// Finalized block number.
        fb_number: u64,
        /// Escrowed hash.
        escrowed_hash: B256,
        /// Hash named by the credit.
        credit_hash: B256,
    },
    /// Credit height is not below the current block.
    #[error("LateFinalizeCredits: fb_number {fb_number} >= block {block_number}")]
    LateCreditFbNumberAtOrAboveBlock {
        /// Finalized block number.
        fb_number: u64,
        /// Current block number.
        block_number: u64,
    },
    /// Credit is outside the inclusion window.
    #[error(
        "LateFinalizeCredits: fb_number {fb_number} outside inclusion window (distance {distance}, K={window_k}) for block {block_number}"
    )]
    LateCreditOutsideInclusionWindow {
        /// Finalized block number.
        fb_number: u64,
        /// Distance from the current block.
        distance: u64,
        /// Inclusion window length.
        window_k: u64,
        /// Current block number.
        block_number: u64,
    },
    /// Inclusion distance does not fit in `u8`.
    #[error(
        "LateFinalizeCredits: inclusion distance {distance} exceeds u8 for block {block_number}"
    )]
    LateCreditInclusionDistanceExceedsU8 {
        /// Distance from the current block.
        distance: u64,
        /// Current block number.
        block_number: u64,
    },
    /// The credited epoch has no committee snapshot.
    #[error(
        "LateFinalizeCredits: missing committee snapshot for epoch={epoch} key={snapshot_key}"
    )]
    LateCreditMissingCommitteeSnapshot {
        /// Epoch named by the credit.
        epoch: u64,
        /// Snapshot key that was missing.
        snapshot_key: B256,
    },
    /// The late-finalize proof did not verify.
    #[error("LateFinalizeCredits: proof verify failed for fb={fb_hash}: {reason}")]
    LateCreditProofVerifyFailed {
        /// Finalized block hash.
        fb_hash: B256,
        /// Verifier failure text.
        reason: String,
    },
    /// A signer index is outside the snapshot committee.
    #[error("LateFinalizeCredits: signer index {idx} out of committee range")]
    LateCreditSignerIndexOutOfRange {
        /// Rejected index.
        idx: usize,
    },
}

impl From<FinalizationError> for PrecompileError {
    fn from(error: FinalizationError) -> Self {
        match error {
            FinalizationError::MissingPreloadedExecutionSummary { .. } => {
                PrecompileError::Storage(error.to_string())
            }
            FinalizationError::SignerBitmapLengthMismatch
            | FinalizationError::DuplicateCommitteeAddresses
            | FinalizationError::UnregisteredCommitteeMember { .. }
            | FinalizationError::MissedProposerNotInCommittee { .. }
            | FinalizationError::NonBinarySignerBitmapEntry
            | FinalizationError::IdenticalReplayUnexpectedProgress { .. }
            | FinalizationError::ProgressGap { .. }
            | FinalizationError::RequiresBlockNumberAtLeastTwo
            | FinalizationError::BlockNumberUnderflow
            | FinalizationError::TargetImmediateParentMismatch { .. }
            | FinalizationError::ZeroFinalizedBlockHash
            | FinalizationError::LateCreditEpochMismatch { .. }
            | FinalizationError::LateCreditCommitteeSetHashMismatch { .. }
            | FinalizationError::LateCreditViewMismatch { .. }
            | FinalizationError::LateCreditParentViewMismatch { .. }
            | FinalizationError::LateCreditMissingEscrow { .. }
            | FinalizationError::LateCreditFbHashMismatch { .. }
            | FinalizationError::LateCreditFbNumberAtOrAboveBlock { .. }
            | FinalizationError::LateCreditOutsideInclusionWindow { .. }
            | FinalizationError::LateCreditInclusionDistanceExceedsU8 { .. }
            | FinalizationError::LateCreditMissingCommitteeSnapshot { .. }
            | FinalizationError::LateCreditProofVerifyFailed { .. }
            | FinalizationError::LateCreditSignerIndexOutOfRange { .. } => {
                PrecompileError::Revert(error.to_string())
            }
        }
    }
}
