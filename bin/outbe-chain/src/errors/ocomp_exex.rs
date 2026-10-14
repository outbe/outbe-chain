//! Typed errors for the embedded OCOMP ExEx runtime.

use alloy_primitives::B256;

/// Errors that occur in the embedded OCOMP ExEx runtime.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OcompExExError {
    #[error("pinned OCOMP vote membership is corrupt: {0}")]
    PinnedVoteMembershipCorrupt(String),
    #[error("OCOMP runtime and reducer generations disagree")]
    RuntimeReducerGenerationsDisagree,
    #[error("OCOMP runtime and reducer projections disagree")]
    RuntimeReducerProjectionsDisagree,
    #[error("OCOMP canonical status/finalized payload shape is invalid")]
    InvalidCanonicalStatusPayloadShape,
    #[error("closed non-expired OCOMP job has no released export authority")]
    ClosedNonExpiredJobMissingExportAuthority,
    #[error("q-forming OCOMP vote is not in the current finalized frame")]
    QuorumVoteNotInCurrentFrame,
    #[error("completed OCOMP job is not finalized")]
    CompletedJobNotFinalized,
    #[error("q-forming ResultVoteV1 disagrees with finalized OCOMP authority")]
    QuorumVoteDisagreesWithAuthority,
    #[error("more than one q-forming OCOMP vote matched one completed job")]
    MultipleQuorumVotesMatched,
    #[error("q-forming OCOMP ResultVoteV1 is missing")]
    QuorumVoteMissing,
    #[error("embedded OCOMP job disappeared")]
    EmbeddedJobDisappeared,
    #[error("restored OCOMP job is unknown")]
    RestoredJobUnknown,
    #[error("completed OCOMP job lacks terminal record")]
    CompletedJobLacksTerminalRecord,
    #[error("completed OCOMP job lacks completed binding")]
    CompletedJobLacksCompletedBinding,
    #[error("OCOMP quorum is ahead of the reconciled finalized target")]
    QuorumAheadOfFinalizedTarget,
    #[error("OCOMP quorum block is unavailable")]
    QuorumBlockUnavailable,
    #[error("OCOMP quorum frame is unavailable")]
    QuorumFrameUnavailable,
    #[error("OCOMP quorum frame is missing")]
    QuorumFrameMissing,
    #[error("unexpected embedded OCOMP local-failure action")]
    UnexpectedLocalFailureAction,
    #[error("computed OCOMP job is unknown")]
    ComputedJobUnknown,
    #[error("canonical OCOMP result disappeared")]
    CanonicalResultDisappeared,
    #[error("unexpected embedded OCOMP local-result action")]
    UnexpectedLocalResultAction,
    #[error("Validator OCOMP key hash is unavailable")]
    ValidatorOcompKeyHashUnavailable,
    #[error("OCOMP job is not finalized")]
    JobNotFinalized,
    #[error("conflicting OCOMP request locator replay")]
    ConflictingRequestLocatorReplay,
    #[error("OCOMP request locator disagrees with exact typed state")]
    RequestLocatorDisagreesWithState,
    #[error("unified OCOMP scan height overflow")]
    UnifiedScanHeightOverflow,
    #[error("unified OCOMP frame scan is not contiguous")]
    UnifiedFrameScanNotContiguous,
    #[error("closed OCOMP job disappeared before spool retirement")]
    ClosedJobDisappearedBeforeSpoolRetirement,
    #[error("closed OCOMP job references an unknown discovery spool")]
    ClosedJobUnknownDiscoverySpool,
    #[error("OCOMP closure checkpoint is ahead of or conflicts with finalized target")]
    ClosureCheckpointConflictsWithTarget,
    #[error("OCOMP readiness checkpoint is unavailable")]
    ReadinessCheckpointUnavailable,
    #[error("OCOMP live notification stream closed")]
    LiveNotificationStreamClosed,
    #[error("embedded OCOMP fatal evidence has an invalid shape")]
    InvalidFatalEvidenceShape,
    #[error("embedded OCOMP fatal evidence has an invalid length")]
    InvalidFatalEvidenceLength,
    #[error("finalized block has no proposer-signed system transaction")]
    FinalizedBlockMissingSystemTx,
    #[error("materialization wake lost FIFO head")]
    MaterializationWakeLostFifoHead,
    #[error("materialization head has no certified generation")]
    MaterializationHeadMissingCertifiedGeneration,
    #[error("OCOMP finalized payload disappeared")]
    FinalizedPayloadDisappeared,
    #[error("OCOMP finalized job disagrees with its request block identity")]
    FinalizedJobRequestBlockIdentityMismatch,
    #[error("OCOMP finalized job carries a conflicting JobId")]
    FinalizedJobConflictingJobId,
    #[error("OCOMP intent replay resolved to a conflicting JobId")]
    IntentReplayConflictingJobId,
    #[error("released OCOMP job is not canonically terminal")]
    ReleasedJobNotCanonicallyTerminal,
    #[error("OCOMP retained pin disagrees with finalized typed state")]
    RetainedPinDisagreesWithFinalizedState,
    #[error("multiple OCOMP discovery generations have durable authority")]
    MultipleDiscoveryGenerationsHaveDurableAuthority,
    #[error("OCOMP retention returned no discovery generation")]
    RetentionReturnedNoDiscoveryGeneration,
    #[error("OCOMP discovery spool is missing for bundle {0}")]
    DiscoverySpoolMissingForBundle(B256),
    #[error("embedded OCOMP job disappeared before export")]
    EmbeddedJobDisappearedBeforeExport,
    #[error("OCOMP spool ACK differs from the exact finalized offer")]
    SpoolAckDiffersFromOffer,
    #[error("closed OCOMP job has no released retention authority")]
    ClosedJobMissingReleasedRetentionAuthority,
    #[error("released OCOMP retention authority conflicts with canonical typed state")]
    ReleasedRetentionAuthorityConflictsWithState,
    #[error("released OCOMP job references an unknown spool")]
    ReleasedJobReferencesUnknownSpool,
    #[error("released OCOMP job has no durable discovery ACK")]
    ReleasedJobMissingDurableDiscoveryAck,
    #[error("released OCOMP ACK differs from its original finalized offer")]
    ReleasedAckDiffersFromOffer,
    #[error("released OCOMP export has no durable discovery ACK")]
    ReleasedExportMissingDurableDiscoveryAck,
    #[error("released OCOMP ACK conflicts with canonical durable authority")]
    ReleasedAckConflictsWithCanonicalAuthority,
    #[error("open Node-owned OCOMP domain: {0}")]
    OpenNodeOwnedDomain(String),
    #[error("unified OCOMP closure checkpoint is unavailable")]
    UnifiedClosureCheckpointUnavailable,
    #[error("unified OCOMP closure checkpoint conflicts with canonical history")]
    UnifiedClosureCheckpointConflictsWithHistory,
    #[error("unified projection sink lock is poisoned")]
    UnifiedProjectionSinkLockPoisoned,
    #[error("unified OCOMP closure checkpoint is ahead of durable projection")]
    UnifiedClosureCheckpointAheadOfProjection,
    #[error("unified OCOMP closure exists before the durable projection checkpoint")]
    UnifiedClosureExistsBeforeDurableProjection,
    #[error("projection accepted an absent finalized target")]
    ProjectionAcceptedAbsentFinalizedTarget,
    #[error("projection reconciled a different finalized target identity")]
    ProjectionReconciledDifferentTargetIdentity,
    #[error("recovered durable projection floor {block_number} ({block_hash}) is unavailable")]
    RecoveredProjectionFloorUnavailable { block_number: u64, block_hash: B256 },
    #[error(
        "recovered durable projection floor {block_number} changed from {expected} to {actual}"
    )]
    RecoveredProjectionFloorChanged {
        block_number: u64,
        expected: B256,
        actual: B256,
    },
    #[error("durable OCOMP closure is ahead of recovered finality")]
    DurableClosureAheadOfRecoveredFinality,
    #[error("unified finalized height overflow")]
    UnifiedFinalizedHeightOverflow,
    #[error("unexpected embedded OCOMP vote action")]
    UnexpectedVoteAction,
}
