use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

use super::aggregate::{WwdMembership, WwdStatus};
use super::reducer::{OuterWwdEvent, OuterWwdTransitionKind, WwdAdvanceEdge};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WwdCallerError {
    #[error("unknown WorldwideDay status")]
    UnknownWorldwideDayStatus,

    #[error("Metadosis forming window overflow")]
    FormingWindowOverflow,

    #[error("Metadosis lookback window overflow")]
    LookbackWindowOverflow,

    #[error("Metadosis offering window overflow")]
    OfferingWindowOverflow,

    #[error("Metadosis waiting window overflow")]
    WaitingWindowOverflow,

    #[error("Metadosis {label} duration overflow")]
    DurationOverflow { label: &'static str },
}

impl From<WwdCallerError> for PrecompileError {
    fn from(value: WwdCallerError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WwdStorageCorruptionError {
    #[error("Metadosis WWD has unknown status tag {other}")]
    WwdUnknownStatusTag { other: u8 },

    #[error("Metadosis WWD has unknown day-type tag {other}")]
    WwdUnknownDayTypeTag { other: u8 },

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

    #[error("Metadosis {label} WWD index contains duplicate {value}")]
    WwdIndexContainsDuplicate {
        label: &'static str,
        value: WorldwideDay,
    },

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

    #[error("Created transition must use commit_new_wwd")]
    CreatedTransitionMustUseCommitNewWwd,

    #[error("outer advance edges do not reach the reducer target")]
    OuterAdvanceEdgesDoNotReachReducerTarget,

    #[error("MissedOffering commit must terminate from LOOKBACK_DELAY")]
    MissedOfferingCommitMustTerminateFromLookbackDelay,

    #[error("CapacityForfeiture commit must terminate from WAITING")]
    CapacityForfeitureCommitMustTerminateFromWaiting,

    #[error("ResolveForming commit requires exactly one typed rate resolution")]
    ResolveFormingCommitRequiresSingleTypedRateResolution,

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

    #[error("fresh-devnet genesis WWD requires a known day type")]
    GenesisWwdRequiresKnownDayType,

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

    #[error("Metadosis WWD {wwd} record exists outside validated membership")]
    WwdRecordExistsOutsideValidatedMembership { wwd: WorldwideDay },

    #[error("AdvanceDue produced non-Cycle transition {unexpected:?}")]
    UnexpectedAdvanceDueTransition { unexpected: OuterWwdTransitionKind },

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
}

impl From<WwdStorageCorruptionError> for PrecompileError {
    fn from(value: WwdStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WwdTestSupportError {
    #[error("capacity fixture exceeds MAX_RETAINED_WWDS")]
    CapacityFixtureExceedsMaxRetainedWwds,

    #[error("capacity fixture contains an invalid WorldwideDay")]
    CapacityFixtureInvalidWorldwideDay,

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },
}

#[cfg(any(test, feature = "test-utils"))]
impl From<WwdTestSupportError> for PrecompileError {
    fn from(value: WwdTestSupportError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
