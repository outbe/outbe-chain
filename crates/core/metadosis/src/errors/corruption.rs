use crate::aggregate::WwdMembership;
use crate::aggregate::WwdStatus;
use crate::reducer::OuterWwdEvent;
use crate::reducer::OuterWwdTransitionKind;
use crate::reducer::WwdAdvanceEdge;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisStorageCorruptionError {
    #[error("Metadosis WWD has unknown status tag {other}")]
    WwdUnknownStatusTag { other: u8 },

    #[error("CapacityForfeiture candidate is older than retained work")]
    CapacityForfeitureCandidateOlderThanRetainedWork,

    #[error("Metadosis WWD {wwd} already exists before active insertion")]
    WwdAlreadyExistsBeforeActiveInsertion { wwd: WorldwideDay },

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },

    #[error(
        "Metadosis retained WWD population {retained_count} exceeds retained-day cap {max_retained_wwds}"
    )]
    RetainedWwdPopulationExceedsCap {
        retained_count: usize,
        max_retained_wwds: usize,
    },

    #[error("Metadosis active WWD population {count} exceeds derived cap {max_active_wwds}")]
    ActiveWwdPopulationExceedsDerivedCap {
        count: usize,
        max_active_wwds: usize,
    },

    #[error("Metadosis closed WWD population {count} exceeds retention cap {max_records_kept}")]
    ClosedWwdPopulationExceedsRetentionCap {
        count: usize,
        max_records_kept: usize,
    },

    #[error("Metadosis WWD {overlap} is both active and closed")]
    WwdBothActiveAndClosed { overlap: WorldwideDay },

    #[error("Metadosis {membership:?} index points to missing WWD {wwd}")]
    IndexPointsToMissingWwd {
        membership: WwdMembership,
        wwd: WorldwideDay,
    },

    #[error("Metadosis WWD {wwd} status/membership mismatch")]
    WwdStatusMembershipMismatch { wwd: WorldwideDay },

    #[error("Metadosis WWD {wwd} has non-monotonic phase boundaries")]
    WwdNonMonotonicPhaseBoundaries { wwd: WorldwideDay },

    #[error("Metadosis WWD {wwd} has an invalid terminal receipt")]
    WwdInvalidTerminalReceipt { wwd: WorldwideDay },

    #[error("Metadosis {label} WWD index contains duplicate {value}")]
    WwdIndexContainsDuplicate {
        label: &'static str,
        value: WorldwideDay,
    },

    // --- aggregate indexes ---
    #[error("Metadosis WWD membership is not exactly one of active/closed")]
    WwdMembershipNotExactlyActiveOrClosed,

    #[error("Metadosis active index points to a missing WWD")]
    ActiveIndexPointsToMissingWwd,

    #[error("Metadosis outer transition has no persisted WWD {worldwide_day}")]
    OuterTransitionMissingPersistedWwd { worldwide_day: WorldwideDay },

    #[error("Metadosis test fixture has no WWD {worldwide_day}")]
    TestFixtureMissingWwd { worldwide_day: WorldwideDay },

    #[error("Metadosis WWD creation requires the exact reducer Created transition")]
    WwdCreationRequiresExactCreatedTransition,

    #[error("Metadosis WWD {worldwide_day} already exists at creation commit")]
    WwdAlreadyExistsAtCreationCommit { worldwide_day: WorldwideDay },

    #[error("created WWD record disappeared")]
    CreatedWwdRecordDisappeared,

    #[error("day-limit formation has no persisted WWD {worldwide_day}")]
    DayLimitFormationMissingPersistedWwd { worldwide_day: WorldwideDay },

    #[error("day-limit formation requires an exact reducer no-op outer transition")]
    DayLimitFormationRequiresNoopOuterTransition,

    #[error("formed native day-limit receipt disappeared after commit")]
    FormedDayLimitReceiptDisappearedAfterCommit,

    #[error("Created transition must use commit_new_wwd")]
    CreatedTransitionMustUseCommitNewWwd,

    #[error("outer advance edges do not reach the reducer target")]
    OuterAdvanceEdgesDoNotReachReducerTarget,

    #[error("MissedOffering commit must terminate from LOOKBACK_DELAY")]
    MissedOfferingCommitMustTerminateFromLookbackDelay,

    #[error("CapacityForfeiture commit must terminate from WAITING")]
    CapacityForfeitureCommitMustTerminateFromWaiting,

    #[error("persisted WWD transition is missing a source status")]
    PersistedWwdTransitionMissingSourceStatus,

    #[error("Metadosis WWD {worldwide_day} reducer expected {expected:?}, found {persisted:?}")]
    WwdReducerStatusMismatch {
        worldwide_day: WorldwideDay,
        expected: WwdStatus,
        persisted: WwdStatus,
    },

    #[error("outer transition target/membership decision is inconsistent")]
    OuterTransitionTargetMembershipDecisionInconsistent,

    #[error("outer Noop transition changes status")]
    OuterNoopTransitionChangesStatus,

    #[error("Metadosis WWD {worldwide_day} has a non-contiguous reducer edge {edge:?}")]
    WwdNonContiguousReducerEdge {
        worldwide_day: WorldwideDay,
        edge: WwdAdvanceEdge,
    },

    #[error("Metadosis WWD {worldwide_day} commit expected {expected:?}, found {persisted:?}")]
    WwdCommitStatusMismatch {
        worldwide_day: WorldwideDay,
        expected: WwdStatus,
        persisted: WwdStatus,
    },

    // --- genesis.rs ---
    #[error("fresh-devnet genesis WWD is not a valid calendar day")]
    GenesisWwdNotValidCalendarDay,

    #[error("fresh-devnet genesis builder only accepts OFFERING or READY predecessors")]
    GenesisBuilderOnlyAcceptsOfferingOrReadyPredecessors,

    #[error("fresh-devnet genesis WWD phase order is invalid")]
    GenesisWwdPhaseOrderInvalid,

    #[error("fresh-devnet genesis builder cannot run after the pre-launch boundary")]
    GenesisBuilderCannotRunAfterPreLaunchBoundary,

    #[error("fresh-devnet genesis WWD already exists")]
    GenesisWwdAlreadyExists,

    #[error("fresh-devnet offering WWD is missing")]
    GenesisOfferingWwdMissing,

    #[error("fresh-devnet WWD is not in OFFERING")]
    GenesisWwdNotInOffering,

    #[error("fresh-devnet WWD phases end after the requested offering deadline")]
    GenesisWwdPhasesEndAfterOfferingDeadline,

    #[error("fresh-devnet genesis contains an unexpected active WWD")]
    GenesisContainsUnexpectedActiveWwd,

    #[error("fresh-devnet active WWD record is missing")]
    GenesisActiveWwdRecordMissing,

    #[error("fresh-devnet seeded WWD is not in OFFERING")]
    GenesisSeededWwdNotInOffering,

    // --- lifecycle.rs, lifecycle/advance.rs, lifecycle/effects.rs ---
    #[error("Metadosis bootstrap needs the genesis Worldwide Day")]
    BootstrapNeedsGenesisWorldwideDay,

    #[error("genesis Worldwide Day record disappeared")]
    GenesisWorldwideDayRecordDisappeared,

    #[error("Metadosis WWD {wwd} record exists outside validated membership")]
    WwdRecordExistsOutsideValidatedMembership { wwd: WorldwideDay },

    #[error("AdvanceDue produced non-Cycle transition {unexpected:?}")]
    UnexpectedAdvanceDueTransition { unexpected: OuterWwdTransitionKind },

    #[error("CapacityForfeiture requires the exact retained admission cap")]
    CapacityForfeitureRequiresExactRetainedAdmissionCap,

    #[error("active CapacityForfeiture victim already has a receipt")]
    ActiveCapacityForfeitureVictimAlreadyHasReceipt,

    #[error("retained WWD cap exceeds u32")]
    RetainedWwdCapExceedsU32,

    #[error("retained WWD count exceeds u32")]
    RetainedWwdCountExceedsU32,

    #[error("active WWD already has a terminal receipt")]
    ActiveWwdAlreadyHasTerminalReceipt,

    // --- native WWD transitions ---
    #[error("Metadosis outer WWD event requires a persisted day")]
    OuterWwdEventRequiresPersistedDay,

    #[error("Metadosis retained WWD count {retained_count} exceeds cap {max_retained_wwds}")]
    RetainedWwdCountExceedsCap {
        retained_count: usize,
        max_retained_wwds: usize,
    },

    #[error("Metadosis outer WWD event {event:?} is invalid from {status:?}")]
    OuterWwdEventInvalidFromStatus {
        event: OuterWwdEvent,
        status: WwdStatus,
    },

    #[error(
        "Metadosis WWD {worldwide_day} status {status:?} is ahead of block timestamp {block_time}"
    )]
    WwdStatusAheadOfBlockTimestamp {
        worldwide_day: WorldwideDay,
        status: WwdStatus,
        block_time: u64,
    },

    #[error("Metadosis full-precision demand overflow")]
    FullPrecisionDemandOverflow,

    #[error("Metadosis day limit return overflow")]
    DayLimitReturnOverflow,

    #[error("Metadosis Lysis Limit exceeds the day's nominal")]
    LysisLimitExceedsDayNominal,

    #[error("Metadosis split exceeds the day limit")]
    SplitExceedsDayLimit,

    #[error("Desis rejection receipt does not match the dispatched limit")]
    DesisRejectionReceiptMismatchDispatchedLimit,

    #[error("{reason} formed day limit does not match WWD state")]
    FormedDayLimitDoesNotMatchWwdState { reason: &'static str },

    #[error("{reason} has a day limit with no formation")]
    DayLimitWithoutFormation { reason: &'static str },

    #[error("formed native day-limit receipt is inconsistent")]
    FormedDayLimitReceiptInconsistent,

    #[error("Metadosis WWD terminal receipt has invalid retirement outcome")]
    WwdTerminalReceiptInvalidRetirementOutcome,

    #[error("terminal receipt magic/version mismatch")]
    TerminalReceiptMagicVersionMismatch,

    #[error("terminal receipt WorldwideDay/key mismatch")]
    TerminalReceiptWorldwideDayKeyMismatch,

    #[error("Metadosis WWD has an unknown terminal receipt outcome")]
    UnknownTerminalReceiptOutcome,

    #[error("terminal receipt has trailing bytes")]
    TerminalReceiptHasTrailingBytes,

    #[error("truncated terminal receipt")]
    TruncatedTerminalReceipt,

    #[error("invalid terminal receipt field width")]
    InvalidTerminalReceiptFieldWidth,

    #[error("Metadosis failure has no persisted WorldwideDay")]
    FailureMissingPersistedWorldwideDay,

    #[error("failed Metadosis WWD has no failure receipt")]
    FailedWwdMissingFailureReceipt,

    #[error("Metadosis failure cannot replace a completed WWD")]
    FailureCannotReplaceCompletedWwd,

    #[error("Metadosis WWD terminal receipt is invalid")]
    WwdTerminalReceiptInvalid,

    #[error("retained cap exceeds u32")]
    RetainedCapExceedsU32,

    #[error("capacity-forfeiture detail differs from terminal receipt")]
    CapacityForfeitureDetailDiffersFromTerminalReceipt,

    #[error("terminal receipt has non-canonical length")]
    TerminalReceiptNonCanonicalLength,

    #[error("Metadosis capacity-forfeiture receipt is immutable")]
    CapacityForfeitureReceiptImmutable,

    #[error("Metadosis WWD terminal receipt is immutable")]
    WwdTerminalReceiptImmutable,

    #[error("capacity-forfeiture terminal receipt has no detail")]
    CapacityForfeitureTerminalReceiptMissingDetail,
}

impl From<MetadosisStorageCorruptionError> for PrecompileError {
    fn from(value: MetadosisStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
