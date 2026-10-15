use outbe_compressed_entities::CollectionError;
use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

use crate::{
    aggregate::{WwdMembership, WwdStatus},
    ocomp::state::JobFsmError,
    reducer::{OuterWwdEvent, OuterWwdTransitionKind, WwdAdvanceEdge},
};

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisStorageCorruptionError {
    // --- aggregate.rs ---
    #[error("Metadosis WWD has unknown status tag {other}")]
    WwdUnknownStatusTag { other: u8 },

    #[error("Metadosis WWD has unknown day-type tag {other}")]
    WwdUnknownDayTypeTag { other: u8 },

    #[error("CapacityForfeiture candidate is older than retained OCOMP work")]
    CapacityForfeitureCandidateOlderThanRetainedOcompWork,

    #[error("Metadosis WWD {wwd} already exists before active insertion")]
    WwdAlreadyExistsBeforeActiveInsertion { wwd: WorldwideDay },

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },

    #[error(
        "Metadosis retained WWD population {retained_count} exceeds OCOMP cap {max_retained_wwds}"
    )]
    RetainedWwdPopulationExceedsOcompCap {
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

    #[error("Metadosis OFFCHAIN_PENDING WWD {wwd} has no OCOMP FSM")]
    OffchainPendingWwdMissingOcompFsm { wwd: WorldwideDay },

    #[error("Metadosis closed WWD {wwd} retains a live OCOMP FSM")]
    ClosedWwdRetainsLiveOcompFsm { wwd: WorldwideDay },

    #[error("Metadosis {label} WWD index contains duplicate {value}")]
    WwdIndexContainsDuplicate {
        label: &'static str,
        value: WorldwideDay,
    },

    // --- aggregate/ocomp_indexes.rs ---
    #[error("Metadosis OCOMP state exists without an active profile")]
    OcompStateExistsWithoutActiveProfile,

    #[error("OCOMP live scheduler points outside active WWD index")]
    OcompLiveSchedulerPointsOutsideActiveWwdIndex,

    #[error("OCOMP live scheduler contains a duplicate WWD")]
    OcompLiveSchedulerContainsDuplicateWwd,

    #[error("OCOMP live FSM has no deadline")]
    OcompLiveFsmMissingDeadline,

    #[error("OCOMP live FSM has no live intent")]
    OcompLiveFsmMissingLiveIntent,

    #[error("OCOMP live FSM has no job record")]
    OcompLiveFsmMissingJobRecord,

    #[error("OCOMP pending FSM membership does not exactly match the live scheduler")]
    OcompPendingFsmMembershipMismatchLiveScheduler,

    #[error("OCOMP awaiting-finality deadline overflow")]
    OcompAwaitingFinalityDeadlineOverflow,

    #[error("OCOMP awaiting-finality FSM/job deadline mismatch")]
    OcompAwaitingFinalityFsmJobDeadlineMismatch,

    #[error("OCOMP voting FSM job is not finalized")]
    OcompVotingFsmJobNotFinalized,

    #[error("OCOMP voting FSM/job deadline mismatch")]
    OcompVotingFsmJobDeadlineMismatch,

    #[error("terminal OCOMP job remains in the live scheduler")]
    TerminalOcompJobRemainsInLiveScheduler,

    #[error("OCOMP response index points to a missing job")]
    OcompResponseIndexMissingJob,

    #[error("OCOMP response index has no matching live voting FSM")]
    OcompResponseIndexMissingLiveVotingFsm,

    #[error("OCOMP live voting FSM has no exact response deadline key")]
    OcompLiveVotingFsmMissingExactResponseDeadlineKey,

    #[error("OCOMP response index job is not finalized")]
    OcompResponseIndexJobNotFinalized,

    #[error("OCOMP response index/job deadline mismatch")]
    OcompResponseIndexJobDeadlineMismatch,

    #[error("OCOMP response index points to a job without an open window")]
    OcompResponseIndexJobWithoutOpenWindow,

    #[error("OCOMP READY index points outside active WWD index")]
    OcompReadyIndexPointsOutsideActiveWwdIndex,

    #[error("OCOMP READY index points to a missing FSM")]
    OcompReadyIndexPointsToMissingFsm,

    #[error("OCOMP READY index does not match its FSM")]
    OcompReadyIndexMismatchFsm,

    // --- api.rs & commands.rs ---
    #[error("Metadosis WWD membership is not exactly one of active/closed")]
    WwdMembershipNotExactlyActiveOrClosed,

    #[error("Metadosis active index points to a missing WWD")]
    ActiveIndexPointsToMissingWwd,

    #[error("certified Metadosis finality binding execution scope mismatch")]
    CertifiedFinalityBindingExecutionScopeMismatch,

    // --- commit.rs ---
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

    #[error("formed OCOMP day-limit receipt disappeared after commit")]
    FormedOcompDayLimitReceiptDisappearedAfterCommit,

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

    // --- lifecycle.rs, lifecycle/advance.rs, lifecycle/effects.rs ---
    #[error("Metadosis bootstrap needs the genesis Worldwide Day")]
    BootstrapNeedsGenesisWorldwideDay,

    #[error("genesis Worldwide Day record disappeared")]
    GenesisWorldwideDayRecordDisappeared,

    #[error("Metadosis WWD {wwd} record exists outside validated membership")]
    WwdRecordExistsOutsideValidatedMembership { wwd: WorldwideDay },

    #[error("AdvanceDue produced non-Cycle transition {unexpected:?}")]
    UnexpectedAdvanceDueTransition { unexpected: OuterWwdTransitionKind },

    #[error("missed offering resolved one WWD rate more than once")]
    MissedOfferingResolvedRateMoreThanOnce,

    #[error("outer advance resolved one WWD rate more than once")]
    OuterAdvanceResolvedRateMoreThanOnce,

    #[error("capacity forfeiture resolved one WWD rate more than once")]
    CapacityForfeitureResolvedRateMoreThanOnce,

    #[error("CapacityForfeiture requires the exact retained admission cap")]
    CapacityForfeitureRequiresExactRetainedAdmissionCap,

    #[error("CapacityForfeiture victim already has OCOMP state")]
    CapacityForfeitureVictimAlreadyHasOcompState,

    #[error("active CapacityForfeiture victim already has a receipt")]
    ActiveCapacityForfeitureVictimAlreadyHasReceipt,

    #[error("retained WWD cap exceeds u32")]
    RetainedWwdCapExceedsU32,

    #[error("retained WWD count exceeds u32")]
    RetainedWwdCountExceedsU32,

    #[error("active WWD already has a terminal receipt")]
    ActiveWwdAlreadyHasTerminalReceipt,

    // --- ocomp/activation.rs & ocomp/authority.rs ---
    #[error("OCOMP q-forming result requires a voting-open non-terminal job")]
    OcompQFormingRequiresVotingOpenNonTerminalJob,

    #[error("OCOMP q-forming job is not finalized")]
    OcompQFormingJobNotFinalized,

    #[error("OCOMP q-forming job already has a quorum")]
    OcompQFormingJobAlreadyHasQuorum,

    #[error("pending OCOMP job has no request profile")]
    PendingOcompJobMissingRequestProfile,

    #[error("hash live OCOMP intent: {error}")]
    HashLiveOcompIntent { error: ProtocolError },

    #[error("Nod issuance instant is missing from the Lysis freeze")]
    NodIssuanceInstantMissingFromLysisFreeze,

    #[error("hash target OCOMP intent: {error}")]
    HashTargetOcompIntent { error: ProtocolError },

    #[error("pending OCOMP job has no live FSM state")]
    PendingOcompJobMissingLiveFsmState,

    #[error("OCOMP request limit receipt is missing")]
    OcompRequestLimitReceiptMissing,

    #[error("OCOMP protocol bundle differs from the request profile")]
    OcompProtocolBundleDiffersFromRequestProfile,

    #[error("invalid OCOMP activation authority: {error}")]
    InvalidOcompActivationAuthority { error: ProtocolError },

    #[error("current ValidatorSet OCOMP snapshot is missing or inconsistent")]
    CurrentValidatorSetOcompSnapshotMissingOrInconsistent,

    #[error("current ValidatorSet OCOMP snapshot is empty")]
    CurrentValidatorSetOcompSnapshotEmpty,

    #[error("current ValidatorSet quorum exceeds u16")]
    CurrentValidatorSetQuorumExceedsU16,

    #[error("OCOMP intent membership differs from the current ValidatorSet snapshot")]
    OcompIntentMembershipDiffersFromCurrentValidatorSetSnapshot,

    #[error("fresh-devnet Metadosis requires a genesis-active OCOMP profile")]
    FreshDevnetRequiresGenesisActiveOcompProfile,

    #[error("fresh-devnet Metadosis requires complete OCOMP activation authority")]
    FreshDevnetRequiresCompleteOcompActivationAuthority,

    // --- ocomp/codec.rs ---
    #[error("OCOMP scheduler offset overflow")]
    OcompSchedulerOffsetOverflow,

    #[error("truncated OCOMP scheduler")]
    TruncatedOcompScheduler,

    #[error("invalid OCOMP scheduler field width")]
    InvalidOcompSchedulerFieldWidth,

    #[error("OCOMP scheduler has trailing bytes")]
    OcompSchedulerHasTrailingBytes,

    #[error("OCOMP canonical object byte cap overflow")]
    OcompCanonicalObjectByteCapOverflow,

    #[error("{label} exceeds canonical byte cap")]
    ExceedsCanonicalByteCap { label: &'static str },

    #[error("decode {label}: {error}")]
    DecodeCanonicalObject {
        label: &'static str,
        error: ProtocolError,
    },

    #[error("OCOMP scheduler state has no active attempt")]
    OcompSchedulerStateMissingActiveAttempt,

    #[error("encode invalid OCOMP scheduler phase cardinality")]
    EncodeInvalidOcompSchedulerPhaseCardinality,

    #[error("OCOMP scheduler encoded length mismatch")]
    OcompSchedulerEncodedLengthMismatch,

    #[error("OCOMP scheduler has non-canonical length")]
    OcompSchedulerNonCanonicalLength,

    #[error("OCOMP scheduler magic/version mismatch")]
    OcompSchedulerMagicVersionMismatch,

    #[error("pending OCOMP scheduler has no retained effect")]
    PendingOcompSchedulerMissingRetainedEffect,

    #[error("OCOMP scheduler phase/index fields are inconsistent")]
    OcompSchedulerPhaseIndexFieldsInconsistent,

    #[error("OCOMP scheduler retained-effect fields are inconsistent")]
    OcompSchedulerRetainedEffectFieldsInconsistent,

    // --- ocomp/expiry.rs ---
    #[error("OCOMP live scheduler has no IntentId")]
    OcompLiveSchedulerHasNoIntentId,

    #[error("OCOMP live scheduler job is missing")]
    OcompLiveSchedulerJobMissing,

    #[error("multiple live OCOMP jobs claim the same request block")]
    MultipleLiveOcompJobsClaimSameRequestBlock,

    #[error("OCOMP certified request parent has no authenticated state root")]
    OcompCertifiedRequestParentMissingStateRoot,

    #[error("overdue OCOMP expiry has no persisted outer WorldwideDay")]
    OverdueOcompExpiryMissingPersistedOuterWwd,

    #[error("awaiting-finality expiry has no persisted outer WorldwideDay")]
    AwaitingFinalityExpiryMissingPersistedOuterWwd,

    #[error("no-quorum OCOMP close has no live job")]
    NoQuorumOcompCloseMissingLiveJob,

    #[error("no-quorum OCOMP close/live state mismatch")]
    NoQuorumOcompCloseLiveStateMismatch,

    #[error("no-quorum OCOMP close has no persisted outer WorldwideDay")]
    NoQuorumOcompCloseMissingPersistedOuterWwd,

    #[error("OCOMP awaiting-finality job has no deadline")]
    OcompAwaitingFinalityJobMissingDeadline,

    #[error("OCOMP consensus skipped the exact awaiting-finality expiry height")]
    OcompConsensusSkippedAwaitingFinalityExpiryHeight,

    #[error("multiple OCOMP awaiting-finality jobs are due at one height")]
    MultipleOcompAwaitingFinalityJobsDueAtOneHeight,

    #[error("pending OCOMP state has no live IntentId")]
    PendingOcompStateMissingLiveIntentId,

    #[error("terminal OCOMP expiry has no retained limit")]
    TerminalOcompExpiryMissingRetainedLimit,

    #[error("terminal OCOMP expiry returned a different retained limit")]
    TerminalOcompExpiryRetainedLimitMismatch,

    #[error("expired OCOMP day has no request receipt")]
    ExpiredOcompDayMissingRequestReceipt,

    #[error("OCOMP expiry post-state is inconsistent")]
    OcompExpiryPostStateInconsistent,

    // --- ocomp/index.rs & ocomp/live_index.rs ---
    #[error("OCOMP READY index received a non-READY FSM state")]
    OcompReadyIndexReceivedNonReadyFsmState,

    #[error("OCOMP READY state has no due height")]
    OcompReadyStateMissingDueHeight,

    #[error("OCOMP READY index already contains the WWD")]
    OcompReadyIndexAlreadyContainsWwd,

    #[error("OCOMP READY index capacity exhausted")]
    OcompReadyIndexCapacityExhausted,

    #[error("OCOMP READY index is missing the exact FSM key")]
    OcompReadyIndexMissingExactFsmKey,

    #[error("OCOMP READY index count exceeds u16")]
    OcompReadyIndexCountExceedsU16,

    #[error("OCOMP READY index encoded length overflow")]
    OcompReadyIndexEncodedLengthOverflow,

    #[error("allocate bounded OCOMP READY index")]
    AllocateBoundedOcompReadyIndex,

    #[error("OCOMP READY index encoded length mismatch")]
    OcompReadyIndexEncodedLengthMismatch,

    #[error("OCOMP READY index is shorter than its header")]
    OcompReadyIndexShorterThanHeader,

    #[error("OCOMP READY index magic/version mismatch")]
    OcompReadyIndexMagicVersionMismatch,

    #[error("OCOMP READY index exceeds its fixed capacity")]
    OcompReadyIndexExceedsFixedCapacity,

    #[error("OCOMP READY index declared length overflow")]
    OcompReadyIndexDeclaredLengthOverflow,

    #[error("OCOMP READY index has non-canonical length")]
    OcompReadyIndexNonCanonicalLength,

    #[error("OCOMP READY index contains an invalid WorldwideDay")]
    OcompReadyIndexInvalidWorldwideDay,

    #[error("OCOMP READY index is not in strict canonical order")]
    OcompReadyIndexNotStrictlyOrdered,

    #[error("OCOMP READY index contains a duplicate WWD")]
    OcompReadyIndexDuplicateWwd,

    #[error("OCOMP response index contains a reserved zero field")]
    OcompResponseIndexContainsReservedZeroField,

    #[error("OCOMP response index already contains this job")]
    OcompResponseIndexAlreadyContainsJob,

    #[error("OCOMP response index is missing the exact job")]
    OcompResponseIndexMissingExactJob,

    #[error("OCOMP response index count exceeds u16")]
    OcompResponseIndexCountExceedsU16,

    #[error("OCOMP response index encoded length overflow")]
    OcompResponseIndexEncodedLengthOverflow,

    #[error("allocate bounded OCOMP response index")]
    AllocateBoundedOcompResponseIndex,

    #[error("OCOMP response index encoded length mismatch")]
    OcompResponseIndexEncodedLengthMismatch,

    #[error("OCOMP response index is shorter than its header")]
    OcompResponseIndexShorterThanHeader,

    #[error("OCOMP response index magic/version mismatch")]
    OcompResponseIndexMagicVersionMismatch,

    #[error("OCOMP response index must use empty bytes for zero windows")]
    OcompResponseIndexNonEmptyBytesForZeroWindows,

    #[error("OCOMP response index declared length overflow")]
    OcompResponseIndexDeclaredLengthOverflow,

    #[error("OCOMP response index has non-canonical length")]
    OcompResponseIndexNonCanonicalLength,

    #[error("OCOMP response index is not in strict canonical order")]
    OcompResponseIndexNotStrictlyOrdered,

    #[error("OCOMP response index contains a duplicate job")]
    OcompResponseIndexDuplicateJob,

    #[error("OCOMP live index count exceeds u16")]
    OcompLiveIndexCountExceedsU16,

    #[error("OCOMP live index magic/version mismatch")]
    OcompLiveIndexMagicVersionMismatch,

    #[error("OCOMP live index must use empty bytes for zero jobs")]
    OcompLiveIndexNonEmptyBytesForZeroJobs,

    #[error("OCOMP live index has non-canonical length")]
    OcompLiveIndexNonCanonicalLength,

    #[error("OCOMP live index is not in strict canonical order")]
    OcompLiveIndexNotStrictlyOrdered,

    #[error("OCOMP live index contains a non-live state")]
    OcompLiveIndexContainsNonLiveState,

    #[error("OCOMP live index contains a duplicate job")]
    OcompLiveIndexDuplicateJob,

    // --- ocomp/request.rs, ocomp/schema.rs, ocomp/snapshot.rs ---
    #[error("READY OCOMP state has no due height")]
    ReadyOcompStateMissingDueHeight,

    #[error("OCOMP READY state contains a prior job attempt")]
    OcompReadyStateContainsPriorJobAttempt,

    #[error("OCOMP Fidelity league snapshot missing at terminal request")]
    OcompFidelityLeagueSnapshotMissingAtTerminalRequest,

    #[error("eligible OCOMP admission changed after exact Tribute seal")]
    EligibleOcompAdmissionChangedAfterTributeSeal,

    #[error("OCOMP admission envelope changed after exact Tribute seal")]
    OcompAdmissionEnvelopeChangedAfterTributeSeal,

    #[error("hash OCOMP pre-admission envelope: {error}")]
    HashOcompPreAdmissionEnvelope { error: ProtocolError },

    #[error("stored OCOMP pre-admission envelope hash mismatch")]
    StoredOcompPreAdmissionEnvelopeHashMismatch,

    #[error(transparent)]
    JobFsm(#[from] JobFsmError),

    #[error("hash OCOMP request receipt: {error}")]
    HashOcompRequestReceipt { error: ProtocolError },

    #[error("OCOMP pending nonce exceeds u32")]
    OcompPendingNonceExceedsU32,

    #[error(transparent)]
    Collection(#[from] CollectionError),

    #[error("hash OCOMP intent: {error}")]
    HashOcompIntent { error: ProtocolError },

    #[error("hash OCOMP activation preconditions: {error}")]
    HashOcompActivationPreconditions { error: ProtocolError },

    #[error("OCOMP intent bundle differs from its Registry lineage pin")]
    OcompIntentBundleDiffersFromRegistryLineagePin,

    #[error("OCOMP Tribute projection/partition day mismatch")]
    OcompTributeProjectionPartitionDayMismatch,

    #[error("stored OCOMP Tribute root differs from provisional CE seal")]
    StoredOcompTributeRootDiffersFromProvisionalCeSeal,

    #[error("authoritative OCOMP receipt disagrees with request effect mode")]
    AuthoritativeOcompReceiptDisagreesWithRequestEffectMode,

    #[error("OCOMP deferred height overflow")]
    OcompDeferredHeightOverflow,

    #[error("OCOMP request requires a resolved day type")]
    OcompRequestRequiresResolvedDayType,

    #[error("cannot enqueue OCOMP WWD {wwd} from status {status_value:?}")]
    CannotEnqueueOcompWwdFromStatus {
        wwd: WorldwideDay,
        status_value: WwdStatus,
    },

    #[error("cannot enqueue a WWD while its OCOMP intent is live")]
    CannotEnqueueWwdWhileOcompIntentIsLive,

    #[error("idempotent OCOMP enqueue changed the due height")]
    IdempotentOcompEnqueueChangedDueHeight,

    #[error("OCOMP READY state has no exact due-index key")]
    OcompReadyStateMissingExactDueIndexKey,

    #[error("OCOMP READY index/FSM key mismatch")]
    OcompReadyIndexFsmKeyMismatch,

    #[error("encode OCOMP pre-admission envelope: {error}")]
    EncodeOcompPreAdmissionEnvelope { error: ProtocolError },

    #[error("OCOMP pre-admission seal/hash mismatch")]
    OcompPreAdmissionSealHashMismatch,

    #[error("OCOMP pre-admission envelope write/read mismatch")]
    OcompPreAdmissionEnvelopeWriteReadMismatch,

    #[error("immutable OCOMP pre-admission envelope changed")]
    ImmutableOcompPreAdmissionEnvelopeChanged,

    #[error("OCOMP day owner set is not strictly ordered and unique")]
    OcompDayOwnerSetNotStrictlyOrderedAndUnique,

    // --- ocomp/store.rs & ocomp/terminal_index.rs ---
    #[error("derive OCOMP intent storage key: {error}")]
    DeriveOcompIntentStorageKey { error: ProtocolError },

    #[error("hash stored OCOMP intent: {error}")]
    HashStoredOcompIntent { error: ProtocolError },

    #[error("OCOMP job record key/IntentId mismatch")]
    OcompJobRecordKeyIntentIdMismatch,

    #[error("OCOMP vote accountability storage key/JobId mismatch")]
    OcompVoteAccountabilityStorageKeyJobIdMismatch,

    #[error("invalid OCOMP vote accountability: {error}")]
    InvalidOcompVoteAccountability { error: ProtocolError },

    #[error("encode OCOMP vote accountability: {error}")]
    EncodeOcompVoteAccountability { error: ProtocolError },

    #[error("OCOMP WWD FSM is not initialized")]
    OcompWwdFsmNotInitialized,

    #[error("OCOMP WWD FSM storage key mismatch")]
    OcompWwdFsmStorageKeyMismatch,

    #[error("restore OCOMP FSM: {error}")]
    RestoreOcompFsm { error: JobFsmError },

    #[error("allocate bounded OCOMP live scheduler")]
    AllocateBoundedOcompLiveScheduler,

    #[error("OCOMP live index/FSM mismatch")]
    OcompLiveIndexFsmMismatch,

    #[error("terminal OCOMP FSM must not be persisted")]
    TerminalOcompFsmMustNotBePersisted,

    #[error("OCOMP scheduler/WorldwideDay status mismatch")]
    OcompSchedulerWorldwideDayStatusMismatch,

    #[error("OCOMP retained limit has no receipt")]
    OcompRetainedLimitMissingReceipt,

    #[error("hash stored request receipt: {error}")]
    HashStoredRequestReceipt { error: ProtocolError },

    #[error("OCOMP retained effect snapshot is missing")]
    OcompRetainedEffectSnapshotMissing,

    #[error("OCOMP limit receipt/state mismatch")]
    OcompLimitReceiptStateMismatch,

    #[error("OCOMP fresh READY state has a residual receipt")]
    OcompFreshReadyStateHasResidualReceipt,

    #[error("OCOMP READY FSM has no exact due-index key")]
    OcompReadyFsmMissingExactDueIndexKey,

    #[error("OCOMP pending FSM has no live intent")]
    OcompPendingFsmMissingLiveIntent,

    #[error("OCOMP live scheduler key has no job record")]
    OcompLiveSchedulerKeyMissingJobRecord,

    #[error("OCOMP live scheduler/job record mismatch")]
    OcompLiveSchedulerJobRecordMismatch,

    #[error("OCOMP live WWD remains in the READY index")]
    OcompLiveWwdRemainsInReadyIndex,

    #[error("invalid OCOMP job record: {error}")]
    InvalidOcompJobRecord { error: ProtocolError },

    #[error("encode OCOMP job record: {error}")]
    EncodeOcompJobRecord { error: ProtocolError },

    #[error("OCOMP live scheduler requires pending state")]
    OcompLiveSchedulerRequiresPendingState,

    #[error("OCOMP live scheduler has no live attempt")]
    OcompLiveSchedulerMissingLiveAttempt,

    #[error("OCOMP live scheduler identity changed")]
    OcompLiveSchedulerIdentityChanged,

    #[error("OCOMP live scheduler is missing the exact job")]
    OcompLiveSchedulerMissingExactJob,

    #[error("live OCOMP job has no finalized binding")]
    LiveOcompJobMissingFinalizedBinding,

    #[error("OCOMP terminal index violates the single-attempt model")]
    OcompTerminalIndexViolatesSingleAttemptModel,

    #[error("allocate bounded OCOMP terminal index")]
    AllocateBoundedOcompTerminalIndex,

    #[error("OCOMP terminal index is sparse below its recorded count")]
    OcompTerminalIndexSparseBelowRecordedCount,

    #[error("OCOMP terminal index rejects a zero IntentId")]
    OcompTerminalIndexRejectsZeroIntentId,

    #[error("OCOMP WorldwideDay already has a terminal job")]
    OcompWorldwideDayAlreadyHasTerminalJob,

    #[error("OCOMP terminal index entry is immutable")]
    OcompTerminalIndexEntryImmutable,

    #[error("OCOMP terminal index count overflow")]
    OcompTerminalIndexCountOverflow,

    // --- ocomp/transitions.rs & ocomp/transitions/*.rs ---
    #[error("Metadosis failure cannot replace a live canonical OCOMP job")]
    MetadosisFailureCannotReplaceLiveCanonicalOcompJob,

    #[error("terminal OCOMP WWD has no active Registry authority")]
    TerminalOcompWwdMissingActiveRegistryAuthority,

    #[error("terminal OCOMP WWD has no Registry lineage pin")]
    TerminalOcompWwdMissingRegistryLineagePin,

    #[error("OCOMP result evidence hash is zero")]
    OcompResultEvidenceHashIsZero,

    #[error("active Lysis generation differs from result evidence")]
    ActiveLysisGenerationDiffersFromResultEvidence,

    #[error("active Lysis generation cannot be overwritten")]
    ActiveLysisGenerationCannotBeOverwritten,

    #[error("commit Lysis terminal permit: {error}")]
    CommitLysisTerminalPermit { error: ProtocolError },

    #[error("OCOMP completion requires the typed completed outer transition")]
    OcompCompletionRequiresTypedCompletedOuterTransition,

    #[error("OCOMP completion has no live job")]
    OcompCompletionMissingLiveJob,

    #[error("OCOMP completion IntentId is not the live job")]
    OcompCompletionIntentIdNotLiveJob,

    #[error("OCOMP completion job record is missing")]
    OcompCompletionJobRecordMissing,

    #[error("OCOMP completion requires a voting-open job")]
    OcompCompletionRequiresVotingOpenJob,

    #[error("OCOMP completion job already has a quorum")]
    OcompCompletionJobAlreadyHasQuorum,

    #[error("encode active Lysis generation: {error}")]
    EncodeActiveLysisGeneration { error: ProtocolError },

    #[error("OCOMP completion job is not finalized")]
    OcompCompletionJobNotFinalized,

    #[error("OCOMP terminal permit is not bound to the live job")]
    OcompTerminalPermitNotBoundToLiveJob,

    #[error("hash active Lysis generation: {error}")]
    HashActiveLysisGeneration { error: ProtocolError },

    #[error("invalid applied terminal receipt: {error}")]
    InvalidAppliedTerminalReceipt { error: ProtocolError },

    #[error("hash applied terminal receipt: {error}")]
    HashAppliedTerminalReceipt { error: ProtocolError },

    #[error("invalid OCOMP completed binding: {error}")]
    InvalidOcompCompletedBinding { error: ProtocolError },

    #[error("OCOMP expiry requires the typed outer expiry transition")]
    OcompExpiryRequiresTypedOuterExpiryTransition,

    #[error("OCOMP expiry index is empty")]
    OcompExpiryIndexEmpty,

    #[error("OCOMP expiry index has no live intent")]
    OcompExpiryIndexMissingLiveIntent,

    #[error("OCOMP expiry selected a different live job")]
    OcompExpirySelectedDifferentLiveJob,

    #[error("OCOMP live index points to a missing job")]
    OcompLiveIndexPointsToMissingJob,

    #[error("expired OCOMP job retained limit mismatch")]
    ExpiredOcompJobRetainedLimitMismatch,

    #[error("OCOMP expiry produced no terminal evidence")]
    OcompExpiryProducedNoTerminalEvidence,

    #[error("OCOMP terminal evidence is inconsistent")]
    OcompTerminalEvidenceInconsistent,

    #[error("OCOMP voting open height overflow")]
    OcompVotingOpenHeightOverflow,

    #[error("OCOMP response deadline overflow")]
    OcompResponseDeadlineOverflow,

    #[error("OCOMP finality record has no matching intent")]
    OcompFinalityRecordMissingMatchingIntent,

    #[error("OCOMP finality requires AWAITING_FINALITY")]
    OcompFinalityRequiresAwaitingFinality,

    #[error("OCOMP finality has no request profile")]
    OcompFinalityMissingRequestProfile,

    #[error("OCOMP finality/window height is invalid")]
    OcompFinalityWindowHeightInvalid,

    #[error("OCOMP finality binding changed")]
    OcompFinalityBindingChanged,

    #[error("derive finalized OCOMP JobId: {error}")]
    DeriveFinalizedOcompJobId { error: ProtocolError },

    #[error("invalid finalized OCOMP job: {error}")]
    InvalidFinalizedOcompJob { error: ProtocolError },

    #[error("encode request receipt: {error}")]
    EncodeRequestReceipt { error: ProtocolError },

    #[error("immutable OCOMP request receipt changed")]
    ImmutableOcompRequestReceiptChanged,

    #[error("OCOMP IntentId already has a job record")]
    OcompIntentIdAlreadyHasJobRecord,

    #[error("OCOMP request requires the typed outer request transition")]
    OcompRequestRequiresTypedOuterRequestTransition,

    #[error("invalid OCOMP intent: {error}")]
    InvalidOcompIntent { error: ProtocolError },

    #[error("invalid OCOMP request receipt: {error}")]
    InvalidOcompRequestReceipt { error: ProtocolError },

    #[error("OCOMP request requires READY WorldwideDay")]
    OcompRequestRequiresReadyWorldwideDay,

    #[error("OCOMP intent/request receipt binding mismatch")]
    OcompIntentRequestReceiptBindingMismatch,

    #[error("create OCOMP vote slots: {error}")]
    CreateOcompVoteSlots { error: ProtocolError },

    #[error("OCOMP vote accountability already exists")]
    OcompVoteAccountabilityAlreadyExists,

    #[error("encode OCOMP vote slots: {error}")]
    EncodeOcompVoteSlots { error: ProtocolError },

    #[error("OCOMP voting cannot open at or after its deadline")]
    OcompVotingCannotOpenAtOrAfterDeadline,

    #[error("multiple OCOMP jobs are due at one bounded open height")]
    MultipleOcompJobsDueAtOneOpenHeight,

    // --- ocomp/views.rs ---
    #[error("OCOMP live jobs do not uniquely match active OFFCHAIN_PENDING days")]
    OcompLiveJobsDoNotUniquelyMatchActiveOffchainPendingDays,

    #[error("OCOMP live jobs contain a terminal record or exceed native capacity")]
    OcompLiveJobsContainTerminalRecordOrExceedCapacity,

    #[error("OCOMP live jobs omit an active OFFCHAIN_PENDING day")]
    OcompLiveJobsOmitActiveOffchainPendingDay,

    #[error("Metadosis active WWD length overflow")]
    ActiveWwdLengthOverflow,

    #[error("Metadosis closed WWD length overflow")]
    ClosedWwdLengthOverflow,

    #[error("Metadosis indexed WWD length exceeds native capacity")]
    IndexedWwdLengthExceedsNativeCapacity,

    #[error("{label} byte cap overflow")]
    NativeByteCapOverflow { label: &'static str },

    #[error("{label} exceeds native byte cap")]
    ExceedsNativeByteCap { label: &'static str },

    #[error("OCOMP indexed FSM has an invalid native byte length")]
    OcompIndexedFsmInvalidNativeByteLength,

    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    // --- ocomp/vote.rs, ocomp/vote/*.rs, ocomp/vote_admission.rs ---
    #[error("OCOMP response-window job is not finalized")]
    OcompResponseWindowJobNotFinalized,

    #[error("OCOMP response index/job binding mismatch")]
    OcompResponseIndexJobBindingMismatch,

    #[error("OCOMP historical snapshot member is missing")]
    OcompHistoricalSnapshotMemberMissing,

    #[error("OCOMP response-window vote slots are missing")]
    OcompResponseWindowVoteSlotsMissing,

    #[error("OCOMP job/accountability quorum mismatch")]
    OcompJobAccountabilityQuorumMismatch,

    #[error("OCOMP activation authority is not installed")]
    OcompActivationAuthorityNotInstalled,

    #[error("OCOMP immutable quorum changed")]
    OcompImmutableQuorumChanged,

    #[error("OCOMP quorum formed outside the voting-open transition")]
    OcompQuorumFormedOutsideVotingOpenTransition,

    #[error("OCOMP block timestamp does not fit u64")]
    OcompBlockTimestampDoesNotFitU64,

    #[error("OCOMP q-forming vote has no persisted outer WorldwideDay")]
    OcompQFormingVoteMissingPersistedOuterWwd,

    #[error("OCOMP q-forming apply removed the job")]
    OcompQFormingApplyRemovedJob,

    #[error("OCOMP q-forming apply did not commit terminal quorum state")]
    OcompQFormingApplyDidNotCommitTerminalQuorumState,

    #[error("OCOMP response deadline/job binding mismatch")]
    OcompResponseDeadlineJobBindingMismatch,

    #[error("OCOMP job/accountability quorum mismatch at close")]
    OcompJobAccountabilityQuorumMismatchAtClose,

    #[error("close OCOMP vote accountability: {error}")]
    CloseOcompVoteAccountability { error: ProtocolError },

    #[error("OCOMP deadline historical snapshot is missing")]
    OcompDeadlineHistoricalSnapshotMissing,

    #[error("OCOMP missing participant index exceeds u16")]
    OcompMissingParticipantIndexExceedsU16,

    #[error("OCOMP deadline snapshot member is missing")]
    OcompDeadlineSnapshotMemberMissing,

    #[error("record ACTIVE missing OCOMP validator {participant_index}: {error}")]
    RecordActiveMiss {
        participant_index: u16,
        error: PrecompileError,
    },

    #[error("resolve due OCOMP recovery for participant {participant_index}: {error}")]
    ResolveDueOcompRecovery {
        participant_index: u16,
        error: PrecompileError,
    },

    #[error("OCOMP response close found an invalid job status")]
    OcompResponseCloseInvalidJobStatus,

    #[error("OCOMP live scheduler byte cap overflow")]
    OcompLiveSchedulerByteCapOverflow,

    #[error("OCOMP live scheduler exceeds the active-job capacity")]
    OcompLiveSchedulerExceedsActiveJobCapacity,

    #[error("OCOMP live scheduler contains duplicate JobId bindings")]
    OcompLiveSchedulerDuplicateJobIdBindings,

    #[error("OCOMP live job without a response window is not awaiting finality")]
    OcompLiveJobWithoutResponseWindowNotAwaitingFinality,

    #[error("OCOMP finalized job did not materialize its response window on time")]
    OcompFinalizedJobDidNotMaterializeResponseWindowOnTime,

    #[error("OCOMP historical snapshot is missing")]
    OcompHistoricalSnapshotMissing,

    #[error("OCOMP historical snapshot member count changed")]
    OcompHistoricalSnapshotMemberCountChanged,

    #[error("OCOMP closed-window historical snapshot is missing")]
    OcompClosedWindowHistoricalSnapshotMissing,

    #[error("OCOMP closed-window snapshot member count changed")]
    OcompClosedWindowSnapshotMemberCountChanged,

    // --- reducer.rs, settlement.rs, state.rs, terminal/** ---
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

    #[error("accepted Desis brief exceeds the Metadosis routing limit")]
    AcceptedDesisBriefExceedsRoutingLimit,

    #[error("Desis rejection receipt does not match the dispatched limit")]
    DesisRejectionReceiptMismatchDispatchedLimit,

    #[error("{reason} formed day limit does not match WWD state")]
    FormedDayLimitDoesNotMatchWwdState { reason: &'static str },

    #[error("{reason} has a day limit with no formation")]
    DayLimitWithoutFormation { reason: &'static str },

    #[error("formed OCOMP day-limit receipt is inconsistent")]
    FormedOcompDayLimitReceiptInconsistent,

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

    #[error("Metadosis failure has no OCOMP profile")]
    FailureMissingOcompProfile,

    #[error("expired OCOMP failure requires the typed expiry transition")]
    ExpiredOcompFailureRequiresTypedExpiryTransition,

    #[error("expired OCOMP failure pre-state is inconsistent")]
    ExpiredOcompFailurePreStateInconsistent,

    #[error("expired OCOMP job is missing")]
    ExpiredOcompJobMissing,

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

impl MetadosisStorageCorruptionError {
    pub(crate) fn map_record_active_miss(
        participant_index: u16,
        error: PrecompileError,
    ) -> PrecompileError {
        match error {
            PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                Self::RecordActiveMiss {
                    participant_index,
                    error,
                }
                .into()
            }
            other => other,
        }
    }

    pub(crate) fn map_resolve_due_ocomp_recovery(
        participant_index: u16,
        error: PrecompileError,
    ) -> PrecompileError {
        match error {
            PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                Self::ResolveDueOcompRecovery {
                    participant_index,
                    error,
                }
                .into()
            }
            other => other,
        }
    }
}

impl From<MetadosisStorageCorruptionError> for PrecompileError {
    fn from(value: MetadosisStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
