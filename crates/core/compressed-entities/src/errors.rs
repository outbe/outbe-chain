//! Typed error definitions for the compressed-entities crate.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};
use thiserror::Error;

/// Errors that the finalized parent adapter returns.
#[derive(Debug, Error)]
pub enum ParentBodySourceError {
    /// The consensus request expired while the node waited for the body.
    /// This error shows that the caller cancelled the request.
    #[error("parent body source request deadline exceeded: {0}")]
    RequestDeadline(String),
    /// The local backend did not serve the request.
    /// This error starts the ADR-005 recovery path.
    #[error("parent body source unavailable: {0}")]
    Unavailable(String),
    /// The local projection broke a canonical body or index rule.
    #[error("parent body source corruption: {0}")]
    Corruption(String),
}

impl From<ParentBodySourceError> for PrecompileError {
    fn from(value: ParentBodySourceError) -> Self {
        match value {
            ParentBodySourceError::RequestDeadline(_) => Self::BodyReadRequestDeadline,
            ParentBodySourceError::Unavailable(message) => Self::BodyReadUnavailable(message),
            ParentBodySourceError::Corruption(message) => Self::BodyReadCorruption(message),
        }
    }
}

/// Errors that the benchmark helpers return.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BenchSupportError {
    /// A benchmark helper failed.
    #[error("{0}")]
    Message(String),
    /// The record count does not fit in a 64-bit integer.
    #[error("benchmark record count is not representable as u64")]
    RecordCountOverflow,
    /// The search counter for benchmark keys overflowed.
    #[error("benchmark key search overflowed")]
    KeySearchOverflow,
}

impl BenchSupportError {
    /// Converts a displayable error into a benchmark error.
    pub(crate) fn message(error: impl core::fmt::Display) -> Self {
        Self::Message(error.to_string())
    }
}

/// Revert-class errors returned by compressed-entity operations.
#[derive(Debug, Error)]
pub enum CompressedEntitiesError {
    /// An insert attempted to create an entity that already exists.
    #[error("compressed entity already exists")]
    EntityAlreadyExists,
    /// An update attempted to change collection or entity identity.
    #[error("compressed entity update collection or identity mismatch")]
    UpdateCollectionOrIdentityMismatch,
    /// An update or delete targeted a missing entity.
    #[error("compressed entity is absent")]
    EntityAbsent,
    /// A verified body capability no longer matches the current commitment.
    #[error("verified body capability no longer matches current value")]
    CapabilityStale,
    /// Canonical body encoding or envelope validation failed.
    #[error(transparent)]
    CanonicalBody(#[from] crate::CanonicalBodyError),
    /// Commitment or field validation failed.
    #[error(transparent)]
    Commitment(#[from] crate::CommitmentError),
    /// Collection key derivation failed.
    #[error(transparent)]
    Collection(#[from] crate::collection::CollectionError),
    /// The same collection retirement was requested more than once in one block.
    #[error("duplicate compressed-entity retirement request")]
    DuplicateRetirementRequest,
    /// A collection retirement conflicts with an earlier entity mutation in the same block.
    #[error("collection retirement conflicts with an earlier entity mutation")]
    RetirementConflictsWithEntityMutation,
    /// An entity mutation conflicts with a pending collection retirement in the same block.
    #[error("compressed-entity mutation conflicts with pending collection retirement")]
    MutationConflictsWithPendingRetirement,
    /// A requested page limit cannot be represented as `usize`.
    #[error("page limit is not representable")]
    PageLimitNotRepresentable,
    /// A requested page limit is outside the allowed `1..=max` range.
    #[error("page limit must be in 1..={max}")]
    InvalidPageLimit { max: u32 },
    /// A `TributeByDay` pagination cursor has a worldwide-day prefix that does not match the query.
    #[error("TributeByDay cursor has the wrong day prefix")]
    TributeByDayCursorWrongDayPrefix,
}

impl From<CompressedEntitiesError> for PrecompileError {
    fn from(value: CompressedEntitiesError) -> Self {
        Self::Revert(value.to_string())
    }
}

/// Fatal invariant and lifecycle errors in compressed-entity execution.
#[derive(Debug, Error)]
pub enum CompressedEntitiesFatalError {
    /// Authenticated parent tree does not expose collection roots.
    #[error("authenticated parent tree does not expose collection roots")]
    ParentTreeDoesNotExposeCollectionRoots,
    /// Empty reference parent does not match the EVM root.
    #[error("empty reference parent does not match the EVM root")]
    EmptyReferenceParentRootMismatch,
    /// Empty parent cannot retire a collection.
    #[error("empty parent cannot retire a collection")]
    EmptyParentCannotRetireCollection,
    /// Exact-parent tree factory configured after begin-block.
    #[error("exact-parent tree factory configured after begin-block")]
    ExactParentFactoryConfiguredAfterBeginBlock,
    /// Exact-parent tree factory configured more than once.
    #[error("exact-parent tree factory configured more than once")]
    ExactParentFactoryConfiguredMoreThanOnce,
    /// Authenticated parent tree lock poisoned.
    #[error("authenticated parent tree lock poisoned")]
    AuthenticatedParentTreeLockPoisoned,
    /// Authenticated parent tree factory lock poisoned.
    #[error("authenticated parent tree factory lock poisoned")]
    AuthenticatedParentTreeFactoryLockPoisoned,
    /// Authenticated parent identity lock poisoned.
    #[error("authenticated parent identity lock poisoned")]
    AuthenticatedParentIdentityLockPoisoned,
    /// Missing exact-parent identity metadata.
    #[error("missing exact-parent identity metadata")]
    MissingExactParentIdentityMetadata,
    /// Provisional compressed-entity seal lock poisoned.
    #[error("provisional compressed-entity seal lock poisoned")]
    ProvisionalSealLockPoisoned,
    /// Provisional compressed-entity seal is absent.
    #[error("provisional compressed-entity seal is absent")]
    ProvisionalSealAbsent,
    /// Compressed-entity seal requested before end-block completed.
    #[error("compressed-entity seal requested before end-block completed")]
    SealRequestedBeforeEndBlockCompleted,
    /// Completed compressed-entity seal lock poisoned.
    #[error("completed compressed-entity seal lock poisoned")]
    CompletedSealLockPoisoned,
    /// Completed compressed-entity seal is absent.
    #[error("completed compressed-entity seal is absent")]
    CompletedSealAbsent,
    /// Collection root requested before compressed-entity seal completed.
    #[error("collection root requested before compressed-entity seal completed")]
    CollectionRootRequestedBeforeSealCompleted,
    /// SealOutput was not completed by this execution scope.
    #[error("SealOutput was not completed by this execution scope")]
    SealOutputNotCompletedByScope,
    /// Compressed-entity SealOutput does not match its staged tree batch.
    #[error("compressed-entity SealOutput does not match its staged tree batch")]
    SealOutputBatchMismatch,
    /// Compressed-entity SealOutput does not match the authenticated parent.
    #[error("compressed-entity SealOutput does not match the authenticated parent")]
    SealOutputParentMismatch,
    /// Sealed compressed-entity collection is absent.
    #[error("sealed compressed-entity collection is absent")]
    SealedCollectionAbsent,
    /// Compressed-entity work meter lock poisoned.
    #[error("compressed-entity work meter lock poisoned")]
    WorkMeterLockPoisoned,
    /// Compressed-entity work transaction window is already active.
    #[error("compressed-entity work transaction window is already active")]
    WorkTransactionWindowAlreadyActive,
    /// Compressed-entity work transaction window is not active.
    #[error("compressed-entity work transaction window is not active")]
    WorkTransactionWindowNotActive,
    /// Invalid compressed-entity work checkpoint restore.
    #[error("invalid compressed-entity work checkpoint restore")]
    InvalidWorkCheckpointRestore,
    /// Compressed-entity explicit gas window is already active.
    #[error("compressed-entity explicit gas window is already active")]
    ExplicitGasWindowAlreadyActive,
    /// Compressed-entity gas checkpoint exceeds current counter.
    #[error("compressed-entity gas checkpoint exceeds current counter")]
    GasCheckpointExceedsCurrentCounter,
    /// Compressed-entity explicit gas window starts after current counter.
    #[error("compressed-entity explicit gas window starts after current counter")]
    ExplicitGasWindowStartsAfterCurrentCounter,
    /// Compressed-entity explicit gas counter overflow.
    #[error("compressed-entity explicit gas counter overflow")]
    ExplicitGasCounterOverflow,
    /// Compressed-entity execution attempted outside active block lifecycle.
    #[error("compressed-entity execution attempted outside active block lifecycle")]
    ExecutionOutsideActiveLifecycle,
    /// Finalized RPC read scope cannot enter block mutation lifecycle.
    #[error("finalized RPC read scope cannot enter block mutation lifecycle")]
    RpcReadScopeCannotEnterMutationLifecycle,
    /// Compressed-entity begin_block called more than once.
    #[error("compressed-entity begin_block called more than once")]
    BeginBlockCalledMoreThanOnce,
    /// Compressed-entity end_block called outside active lifecycle.
    #[error("compressed-entity end_block called outside active lifecycle")]
    EndBlockCalledOutsideActiveLifecycle,
    /// Compressed-entity seal identity recorded more than once.
    #[error("compressed-entity seal identity recorded more than once")]
    SealIdentityRecordedMoreThanOnce,
    /// Exact parent tree root does not match compressed-entity EVM root.
    #[error("exact parent tree root does not match compressed-entity EVM root")]
    ExactParentRootMismatch,
    /// Prepared compressed-entity tree batch has the wrong parent root.
    #[error("prepared compressed-entity tree batch has the wrong parent root")]
    PreparedTreeBatchWrongParentRoot,
    /// Invalid pending StoredBody in transient overlay.
    #[error("invalid pending StoredBody: {0}")]
    InvalidPendingStoredBody(#[source] crate::CanonicalBodyError),
    /// Verified payload has no typed variant.
    #[error("verified payload has no typed variant")]
    VerifiedPayloadMissingVariant,
    /// Body length exceeds gas range.
    #[error("body length exceeds gas range")]
    BodyLengthExceedsGasRange,
    /// Zero index delta escaped state validation.
    #[error("zero index delta escaped state validation")]
    ZeroIndexDeltaEscapedValidation,
    /// Parent continuation or merged lookahead produced an empty result page.
    #[error("parent continuation or merged lookahead produced an empty result page")]
    EmptyResultPageWithContinuation,
    /// Compressed-entity schema initialization found non-empty root/reserved slots.
    #[error("compressed-entity schema initialization found non-empty root/reserved slots")]
    SchemaInitFoundNonEmptySlots,
    /// Compressed-entity genesis allocation is missing on write.
    #[error("compressed-entity genesis allocation is missing; lazy initialization is forbidden")]
    MissingGenesisAllocationOnWrite,
    /// Compressed-entity genesis allocation is missing on read.
    #[error("compressed-entity genesis allocation is missing; reads cannot bootstrap it")]
    MissingGenesisAllocationOnRead,
    /// Compressed-entity schema migration requires an empty overlay.
    #[error("compressed-entity schema migration requires an empty overlay")]
    SchemaMigrationRequiresEmptyOverlay,
    /// Unsupported compressed-entity storage schema.
    #[error("unsupported compressed-entity storage schema {schema}")]
    UnsupportedStorageSchema { schema: u64 },
    /// Compressed-entity reserved schema slots are non-zero.
    #[error("compressed-entity reserved schema slots are non-zero")]
    ReservedSchemaSlotsNonZero,
    /// Malformed compressed-entity retirement journal.
    #[error("malformed compressed-entity retirement journal")]
    MalformedRetirementJournal,
    /// Compressed-entity block overlay is dirty at begin_block.
    #[error("compressed-entity block overlay is dirty at begin_block")]
    DirtyOverlayAtBeginBlock,
    /// Untouched compressed-entity locator has residual dynamic state.
    #[error("untouched compressed-entity locator has residual dynamic state")]
    UntouchedLocatorHasResidualState,
    /// Compressed-entity Set entry has no pending body.
    #[error("compressed-entity Set entry has no pending body")]
    SetEntryMissingPendingBody,
    /// Compressed-entity Deleted entry retains pending body.
    #[error("compressed-entity Deleted entry retains pending body")]
    DeletedEntryRetainsPendingBody,
    /// Compressed-entity body identity record/hash mismatch.
    #[error("compressed-entity body identity record/hash mismatch")]
    BodyIdentityRecordHashMismatch,
    /// Pending Set was not prepared by first-touch logic.
    #[error("pending Set was not prepared by first-touch logic")]
    PendingSetNotPrepared,
    /// Pending Deleted was not prepared by first-touch logic.
    #[error("pending Deleted was not prepared by first-touch logic")]
    PendingDeletedNotPrepared,
    /// Duplicate compressed-entity touched body locator.
    #[error("duplicate compressed-entity touched body locator")]
    DuplicateTouchedBodyLocator,
    /// Touched compressed-entity body has no identity.
    #[error("touched compressed-entity body has no identity")]
    TouchedBodyMissingIdentity,
    /// Compressed-entity touched body locator mismatch.
    #[error("compressed-entity touched body locator mismatch")]
    TouchedBodyLocatorMismatch,
    /// Duplicate compressed-entity touched identity.
    #[error("duplicate compressed-entity touched identity")]
    DuplicateTouchedIdentity,
    /// Touched compressed-entity body is Untouched.
    #[error("touched compressed-entity body is Untouched")]
    TouchedBodyIsUntouched,
    /// Compressed-entity cleanup body has no identity.
    #[error("compressed-entity cleanup body has no identity")]
    CleanupBodyMissingIdentity,
    /// Compressed-entity cleanup body locator mismatch.
    #[error("compressed-entity cleanup body locator mismatch")]
    CleanupBodyLocatorMismatch,
    /// Compressed-entity cleanup Set entry has no body.
    #[error("compressed-entity cleanup Set entry has no body")]
    CleanupSetEntryMissingBody,
    /// Compressed-entity cleanup Deleted entry retains body.
    #[error("compressed-entity cleanup Deleted entry retains body")]
    CleanupDeletedEntryRetainsBody,
    /// Compressed-entity cleanup reached untouched body.
    #[error("compressed-entity cleanup reached untouched body")]
    CleanupReachedUntouchedBody,
    /// Duplicate compressed-entity touched index key.
    #[error("duplicate compressed-entity touched index key")]
    DuplicateTouchedIndexKey,
    /// Compressed-entity cleanup reached untouched index.
    #[error("compressed-entity cleanup reached untouched index")]
    CleanupReachedUntouchedIndex,
    /// Compressed-entity cleanup index hash mismatch.
    #[error("compressed-entity cleanup index hash mismatch")]
    CleanupIndexHashMismatch,
    /// Malformed retirement journal during cleanup.
    #[error("malformed retirement journal during cleanup")]
    MalformedRetirementJournalDuringCleanup,
    /// Compressed-entity cleanup left non-empty touched lists.
    #[error("compressed-entity cleanup left non-empty touched lists")]
    CleanupLeftNonEmptyTouchedLists,
    /// Compressed-entity body cleanup post-condition failed.
    #[error("compressed-entity body cleanup post-condition failed")]
    BodyCleanupPostConditionFailed,
    /// Compressed-entity index cleanup post-condition failed.
    #[error("compressed-entity index cleanup post-condition failed")]
    IndexCleanupPostConditionFailed,
    /// Compressed-entity retirement cleanup post-condition failed.
    #[error("compressed-entity retirement cleanup post-condition failed")]
    RetirementCleanupPostConditionFailed,
    /// Index add requested for an already-current membership.
    #[error("index add requested for an already-current membership")]
    IndexAddAlreadyCurrent,
    /// Index remove requested for a non-current membership.
    #[error("index remove requested for a non-current membership")]
    IndexRemoveNonCurrent,
    /// Untouched index delta has residual record.
    #[error("untouched index delta has residual record")]
    UntouchedIndexDeltaHasResidualRecord,
    /// Compressed-entity index record/hash mismatch.
    #[error("compressed-entity index record/hash mismatch")]
    IndexRecordHashMismatch,
    /// Touched index key has zero status.
    #[error("touched index key has zero status")]
    TouchedIndexKeyHasZeroStatus,
    /// Compressed-entity touched index hash mismatch.
    #[error("compressed-entity touched index hash mismatch")]
    TouchedIndexHashMismatch,
    /// Invalid compressed-entity collection id.
    #[error("invalid compressed-entity collection id {id}")]
    InvalidCollectionId { id: u8 },
    /// Invalid compressed-entity pending word.
    #[error("invalid compressed-entity pending word {word:#x}")]
    InvalidPendingWord { word: alloy_primitives::U256 },
    /// Invalid compressed-entity index delta word.
    #[error("invalid compressed-entity index delta word {word:#x}")]
    InvalidIndexDeltaWord { word: alloy_primitives::U256 },
    /// Invalid compressed-entity index kind.
    #[error("invalid compressed-entity index kind {id}")]
    InvalidIndexKind { id: u8 },
    /// Compressed-entity index record is truncated.
    #[error("compressed-entity index record is truncated")]
    IndexRecordTruncated,
    /// Unsupported compressed-entity index record version.
    #[error("unsupported compressed-entity index record version {version}")]
    UnsupportedIndexRecordVersion { version: u8 },
    /// Non-canonical compressed-entity index record length.
    #[error("non-canonical compressed-entity index record length")]
    NonCanonicalIndexRecordLength,
    /// Non-canonical compressed-entity index record.
    #[error("non-canonical compressed-entity index record")]
    NonCanonicalIndexRecord,
    /// Collection derivation error.
    #[error(transparent)]
    Collection(#[from] crate::collection::CollectionError),
    /// Sparse Merkle tree error.
    #[error(transparent)]
    Smt(#[from] crate::smt::TreeError),
    /// Sharding error.
    #[error(transparent)]
    Sharding(#[from] crate::sharding::ShardingError),
    /// Persistence error.
    #[error(transparent)]
    Persistence(#[from] crate::persistence::PersistenceError),
    /// Staging error.
    #[error(transparent)]
    Staging(#[from] crate::staging::StagingError),
    /// Commitment error.
    #[error(transparent)]
    Commitment(#[from] crate::CommitmentError),
    /// Entity ID error.
    #[error(transparent)]
    EntityId(#[from] core::array::TryFromSliceError),
    /// Tree corruption error.
    #[error("compressed-entity tree corruption: {0}")]
    TreeCorruption(#[from] CompressedEntitiesTreeCorruptionError),
}

impl From<CompressedEntitiesFatalError> for PrecompileError {
    fn from(value: CompressedEntitiesFatalError) -> Self {
        Self::Fatal(value.to_string())
    }
}

/// Tree corruption errors surfaced with the `"compressed-entity tree corruption: ..."` prefix.
#[derive(Debug, Error)]
pub enum CompressedEntitiesTreeCorruptionError {
    /// Sparse Merkle tree failure.
    #[error(transparent)]
    Smt(#[from] crate::smt::TreeError),
    /// Collection derivation failure.
    #[error(transparent)]
    Collection(#[from] crate::collection::CollectionError),
    /// Sharding failure.
    #[error(transparent)]
    Sharding(#[from] crate::sharding::ShardingError),
    /// Commitment derivation failure.
    #[error(transparent)]
    Commitment(#[from] crate::CommitmentError),
    /// Persistence layer failure.
    #[error(transparent)]
    Persistence(#[from] crate::persistence::PersistenceError),
    /// Staging batch failure.
    #[error(transparent)]
    Staging(#[from] crate::staging::StagingError),
    /// Collection key/domain collision.
    #[error("collection key/domain collision")]
    CollectionKeyDomainCollision,
    /// Authenticated catalog session already sealed.
    #[error("authenticated catalog session already sealed")]
    CatalogSessionAlreadySealed,
    /// Orphan collection records behind catalog non-membership.
    #[error("orphan collection records behind catalog non-membership")]
    OrphanCollectionRecords,
    /// Present collection has the wrong number of shard roots.
    #[error("present collection has {actual} shard roots, expected {expected}")]
    ShardRootCountMismatch { actual: usize, expected: u32 },
    /// Present collection has a missing shard root.
    #[error("present collection has a missing shard root")]
    MissingShardRoot,
    /// Catalog collection root does not match exact shard-root vector.
    #[error("catalog collection root does not match exact shard-root vector")]
    CatalogCollectionRootMismatch,
    /// Authenticated catalog session lock poisoned.
    #[error("authenticated catalog session lock poisoned")]
    CatalogSessionLockPoisoned,
    /// Verified collection cache entry is missing.
    #[error("verified collection cache entry is missing")]
    MissingVerifiedCollectionCacheEntry,
    /// Compressed-entity seal block is not parent height + 1.
    #[error("compressed-entity seal block is not parent height + 1")]
    SealBlockNotParentPlusOne,
    /// Collection session is missing.
    #[error("collection session is missing")]
    MissingCollectionSession,
    /// Collection shard set already sealed.
    #[error("collection shard set already sealed")]
    CollectionShardSetAlreadySealed,
    /// Derived mutation shard was not prepared.
    #[error("derived mutation shard was not prepared")]
    DerivedMutationShardNotPrepared,
    /// Duplicate collection retirement at seal.
    #[error("duplicate collection retirement at seal")]
    DuplicateCollectionRetirementAtSeal,
    /// Collection mutation and retirement coexist in one block.
    #[error("collection mutation and retirement coexist in one block")]
    CollectionMutationAndRetirementCoexist,
    /// Retirement collection session is missing.
    #[error("retirement collection session is missing")]
    MissingRetirementCollectionSession,
    /// Retirement collection is absent from the parent catalog.
    #[error("retirement collection is absent from the parent catalog")]
    RetirementCollectionAbsentFromParentCatalog,
    /// Authenticated catalog sealed more than once.
    #[error("authenticated catalog sealed more than once")]
    CatalogSealedMoreThanOnce,
    /// Duplicate compressed-entity identity at seal.
    #[error("duplicate compressed-entity identity at seal")]
    DuplicateEntityIdentityAtSeal,
    /// Compressed-entity tree-key collision.
    #[error("compressed-entity tree-key collision")]
    TreeKeyCollision,
}

impl From<CompressedEntitiesTreeCorruptionError> for PrecompileError {
    fn from(value: CompressedEntitiesTreeCorruptionError) -> Self {
        CompressedEntitiesFatalError::TreeCorruption(value).into()
    }
}

/// Tree-unavailable errors returned when the exact authenticated parent tree is not ready.
#[derive(Debug, Error)]
pub enum CompressedEntitiesTreeUnavailableError {
    /// Opened parent tree root does not match EVM slot 1.
    #[error("opened parent tree root does not match EVM slot 1")]
    OpenedParentRootMismatch,
    /// No exact-parent tree factory was installed.
    #[error("no exact-parent tree factory was installed")]
    MissingExactParentTreeFactory,
    /// Exact-parent factory returned a tree with the wrong block hash or root.
    #[error("exact-parent factory returned a tree with the wrong block hash or root")]
    FactoryReturnedWrongParentIdentity,
    /// Exact-parent tree has not been opened by begin-block.
    #[error("exact-parent tree has not been opened by begin-block")]
    ExactParentTreeNotOpened,
    /// Authenticated parent tree root does not match EVM slot 1.
    #[error("authenticated parent tree root does not match EVM slot 1")]
    AuthenticatedParentRootMismatch,
    /// Requested EVM root does not match exact catalog view.
    #[error("requested EVM root does not match exact catalog view")]
    RequestedRootDoesNotMatchCatalogView,
    /// Persistence error when opening exact parent view.
    #[error(transparent)]
    Persistence(#[from] crate::persistence::PersistenceError),
    /// Tree service error when opening exact parent view.
    #[error(transparent)]
    Service(#[from] crate::tree_manager::TreeServiceError),
}

impl From<CompressedEntitiesTreeUnavailableError> for PrecompileError {
    fn from(value: CompressedEntitiesTreeUnavailableError) -> Self {
        Self::TreeUnavailable(value.to_string())
    }
}

/// Body-read corruption errors returned when finalized-parent bodies or indexes violate canonical invariants.
#[derive(Debug, Error)]
pub enum CompressedEntitiesBodyCorruptionError {
    /// Committed body is missing from finalized parent.
    #[error("committed body {entity_id} is missing from finalized parent")]
    MissingCommittedBody { entity_id: crate::WwdEntityId },
    /// Removed ID is missing from finalized-parent index.
    #[error("Removed ID is missing from finalized-parent index")]
    RemovedIdMissingFromParentIndex,
    /// Added ID already exists in finalized-parent index.
    #[error("Added ID {id} already exists in finalized-parent index")]
    AddedIdAlreadyExistsInParentIndex { id: crate::WwdEntityId },
    /// Duplicate finalized-parent ID.
    #[error("duplicate finalized-parent ID {id}")]
    DuplicateParentId { id: crate::WwdEntityId },
    /// Removed ID was skipped by the ordered finalized-parent index.
    #[error("Removed ID was skipped by the ordered finalized-parent index")]
    RemovedIdSkippedByParentIndex,
    /// Listed compressed entity is canonically absent.
    #[error("listed compressed entity {id} is canonically absent")]
    ListedEntityCanonicallyAbsent { id: crate::WwdEntityId },
    /// Listed compressed entity violates query predicate.
    #[error("listed compressed entity {id} violates query predicate")]
    ListedEntityViolatesQueryPredicate { id: crate::WwdEntityId },
    /// Parent page exceeds requested limit.
    #[error("parent page exceeds requested limit")]
    ParentPageExceedsLimit,
    /// Parent IDs are not strictly ascending after the cursor.
    #[error("parent IDs are not strictly ascending after the cursor")]
    ParentIdsNotStrictlyAscending,
    /// Parent TributeByDay ID has the wrong day prefix.
    #[error("parent TributeByDay ID has the wrong day prefix")]
    ParentTributeByDayWrongDayPrefix,
    /// Parent next_after must equal its last returned ID.
    #[error("parent next_after must equal its last returned ID")]
    ParentNextAfterMismatch,
    /// Empty parent page cannot advertise a continuation.
    #[error("empty parent page cannot advertise a continuation")]
    EmptyParentPageWithContinuation,
}

impl From<CompressedEntitiesBodyCorruptionError> for PrecompileError {
    fn from(value: CompressedEntitiesBodyCorruptionError) -> Self {
        Self::BodyReadCorruption(value.to_string())
    }
}

/// Verification failure for a stored body read from either the transient overlay or finalized parent.
#[derive(Debug, Error)]
pub enum StoredBodyVerificationError {
    /// Unsupported stored body schema.
    #[error("unsupported stored body schema {schema}")]
    UnsupportedSchema { schema: u32 },
    /// Decoded body identity does not match the requested entity ID.
    #[error("body identity {decoded} does not match requested {requested}")]
    IdentityMismatch {
        decoded: crate::WwdEntityId,
        requested: crate::WwdEntityId,
    },
    /// Commitment derivation failed.
    #[error(transparent)]
    Commitment(#[from] crate::CommitmentError),
    /// Canonical body decoding failed.
    #[error(transparent)]
    CanonicalBody(#[from] crate::CanonicalBodyError),
    /// Recomputed body commitment does not match expected leaf commitment.
    #[error(
        "body commitment mismatch for {entity_id}; [CE_BODY_DIAGNOSTIC] entity={entity:?} origin={origin} scheme={scheme} schema={schema} expected=0x{expected_hex} actual=0x{actual_hex} payload_len={payload_len} payload_hex={payload_hex} stored_body_hex={stored_body_hex} decoded={decoded:?}"
    )]
    CommitmentMismatch {
        entity_id: crate::WwdEntityId,
        entity: crate::api::EntityRef,
        origin: &'static str,
        scheme: u32,
        schema: u32,
        expected_hex: String,
        actual_hex: String,
        payload_len: usize,
        payload_hex: String,
        stored_body_hex: String,
        decoded: Box<crate::api::VerifiedPayload>,
    },
}

/// Appends diagnostic parent data to a precompile error for body corruption.
pub(crate) fn enrich_body_read_corruption(
    error: PrecompileError,
    evm_block: Result<u64>,
    parent_root: B256,
    scope_parent_root: Result<B256>,
    diagnostic_binding: &str,
) -> PrecompileError {
    match error {
        PrecompileError::BodyReadCorruption(message) => PrecompileError::BodyReadCorruption(
            format!(
                "{message}; [CE_BODY_DIAGNOSTIC] evm_block={evm_block:?} evm_ce_root={parent_root} parent_root={scope_parent_root:?} {diagnostic_binding} thread={:?}",
                std::thread::current().id(),
            ),
        ),
        other => other,
    }
}

/// Typed reasons for [`crate::proof::PointReadServiceError::InvalidPackage`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum PointReadInvalidPackageReason {
    /// Failed to derive collection key for the entity.
    #[error("collection derivation")]
    CollectionDerivation,
    /// Recomputed collection root did not match the expected collection root.
    #[error("collection root mismatch")]
    CollectionRootMismatch,
    /// Unknown compressed-entity domain ID.
    #[error("unknown domain")]
    UnknownDomain,
    /// `PointReadResultV1::Unavailable` is not a verifiable proof package.
    #[error("unavailable is not a proof package")]
    UnavailableNotProofPackage,
    /// Present point-read package has an empty body payload.
    #[error("empty present body")]
    EmptyPresentBody,
    /// Catalog key is not a canonical field element.
    #[error("catalog key")]
    CatalogKey,
    /// Catalog leaf is not a canonical field element.
    #[error("catalog leaf")]
    CatalogLeaf,
    /// Catalog CKB proof root computation failed.
    #[error("catalog proof")]
    CatalogProof,
    /// Sealed root wrapper computation failed.
    #[error("sealed root")]
    SealedRoot,
    /// Recomputed sealed root did not match header artifact `r_sealed`.
    #[error("header root mismatch")]
    HeaderRootMismatch,
    /// Tree key derivation failed.
    #[error("tree key")]
    TreeKey,
    /// Shard index derivation failed.
    #[error("shard index")]
    ShardIndex,
    /// Top sibling count did not match `log2(shard_count)`.
    #[error("top sibling count")]
    TopSiblingCount,
    /// Leaf bytes are not a canonical field element.
    #[error("leaf field")]
    LeafField,
    /// Shard CKB proof root computation failed.
    #[error("shard proof")]
    ShardProof,
    /// Top-tree sibling root is not a canonical field element.
    #[error("non-canonical top sibling")]
    NonCanonicalTopSibling,
    /// Recomputed top-tree node is not a canonical field element.
    #[error("invalid top root")]
    InvalidTopRoot,
    /// Collection root computation failed.
    #[error("collection root")]
    CollectionRoot,
    /// Common proof metadata binding did not match request or selected header.
    #[error("common binding")]
    CommonBinding,
    /// Finalized marker height/hash did not match selected header.
    #[error("marker/header identity")]
    MarkerHeaderIdentity,
    /// Finalized marker commitment scheme or root did not match selected header artifact.
    #[error("marker/header root")]
    MarkerHeaderRoot,
    /// Selected header is block 0 (genesis).
    #[error("genesis header")]
    GenesisHeader,
    /// Block extra-data artifact decoding failed.
    #[error("header artifacts")]
    HeaderArtifacts,
    /// Block extra-data is missing tag `0x08` (`compressed_entities_root`).
    #[error("missing tag 0x08")]
    MissingTag0x08,
    /// Header artifact commitment scheme version is not active.
    #[error("fork scheme")]
    ForkScheme,
    /// Stored body envelope decoding failed.
    #[error("stored body envelope")]
    StoredBodyEnvelope,
    /// Encrypted v2 tribute body decoding failed.
    #[error("encrypted tribute body")]
    EncryptedTributeBody,
    /// Tribute v1 body decoding failed.
    #[error("tribute body")]
    TributeBody,
    /// Encrypted v2 NOD item body decoding failed.
    #[error("encrypted nod body")]
    EncryptedNodBody,
    /// NOD item v1 body decoding failed.
    #[error("nod item body")]
    NodItemBody,
    /// NOD bucket v1 body decoding failed.
    #[error("nod bucket body")]
    NodBucketBody,
    /// GEM v1 body decoding failed.
    #[error("gem body")]
    GemBody,
    /// GEM bucket v1 body decoding failed.
    #[error("gem bucket body")]
    GemBucketBody,
    /// Decoded body identity did not match requested entity ID.
    #[error("body identity")]
    BodyIdentity,
    /// Body commitment derivation failed.
    #[error("body commitment")]
    BodyCommitment,
    /// Shard-top root slice or selected shard index is out of bounds.
    #[error("invalid shard-top envelope")]
    InvalidShardTopEnvelope,
    /// Poseidon top-node hash failed.
    #[error("top hash")]
    TopHash,
}

impl PointReadInvalidPackageReason {
    /// Static message preserved in [`crate::proof::PointReadServiceError::InvalidPackage`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CollectionDerivation => "collection derivation",
            Self::CollectionRootMismatch => "collection root mismatch",
            Self::UnknownDomain => "unknown domain",
            Self::UnavailableNotProofPackage => "unavailable is not a proof package",
            Self::EmptyPresentBody => "empty present body",
            Self::CatalogKey => "catalog key",
            Self::CatalogLeaf => "catalog leaf",
            Self::CatalogProof => "catalog proof",
            Self::SealedRoot => "sealed root",
            Self::HeaderRootMismatch => "header root mismatch",
            Self::TreeKey => "tree key",
            Self::ShardIndex => "shard index",
            Self::TopSiblingCount => "top sibling count",
            Self::LeafField => "leaf field",
            Self::ShardProof => "shard proof",
            Self::NonCanonicalTopSibling => "non-canonical top sibling",
            Self::InvalidTopRoot => "invalid top root",
            Self::CollectionRoot => "collection root",
            Self::CommonBinding => "common binding",
            Self::MarkerHeaderIdentity => "marker/header identity",
            Self::MarkerHeaderRoot => "marker/header root",
            Self::GenesisHeader => "genesis header",
            Self::HeaderArtifacts => "header artifacts",
            Self::MissingTag0x08 => "missing tag 0x08",
            Self::ForkScheme => "fork scheme",
            Self::StoredBodyEnvelope => "stored body envelope",
            Self::EncryptedTributeBody => "encrypted tribute body",
            Self::TributeBody => "tribute body",
            Self::EncryptedNodBody => "encrypted nod body",
            Self::NodItemBody => "nod item body",
            Self::NodBucketBody => "nod bucket body",
            Self::GemBody => "gem body",
            Self::GemBucketBody => "gem bucket body",
            Self::BodyIdentity => "body identity",
            Self::BodyCommitment => "body commitment",
            Self::InvalidShardTopEnvelope => "invalid shard-top envelope",
            Self::TopHash => "top hash",
        }
    }
}

impl From<PointReadInvalidPackageReason> for crate::proof::PointReadServiceError {
    fn from(reason: PointReadInvalidPackageReason) -> Self {
        Self::InvalidPackage(reason.as_str())
    }
}

/// Typed reasons for [`crate::proof::PointReadServiceError::Materialization`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum PointReadMaterializationReason {
    /// Absent catalog leaf still has persisted collection records.
    #[error("orphan records behind absent catalog leaf")]
    OrphanRecordsBehindAbsentCatalogLeaf,
    /// Collection shard-root count does not match domain shard count.
    #[error("incomplete collection shard-root vector")]
    IncompleteCollectionShardRootVector,
    /// A shard root in a present collection is missing.
    #[error("missing shard root")]
    MissingShardRoot,
    /// Recomputed collection root from shard roots does not match catalog leaf.
    #[error("catalog leaf does not match shard roots")]
    CatalogLeafDoesNotMatchShardRoots,
}

impl From<PointReadMaterializationReason> for crate::proof::PointReadServiceError {
    fn from(reason: PointReadMaterializationReason) -> Self {
        Self::Materialization(reason.to_string())
    }
}

/// Typed reasons for [`crate::smt::TreeError::ReducerInvariant`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SmtReducerInvariantReason {
    /// Archive node counter overflowed `u64`.
    #[error("archive node count overflow")]
    ArchiveNodeCountOverflow,
    /// Active subtree disappeared during ordered lift.
    #[error("current subtree disappeared")]
    CurrentSubtreeDisappeared,
    /// Completed subtree is on the right branch at its fork height.
    #[error("ordered subtree is not the left fork child")]
    OrderedSubtreeNotLeftForkChild,
    /// Fork height index is outside the 256-level frontier.
    #[error("fork height outside frontier")]
    ForkHeightOutsideFrontier,
    /// Frontier slot at the fork height was already occupied.
    #[error("fork frontier already contains a subtree")]
    ForkFrontierAlreadyContainsSubtree,
    /// Empty reducer had non-empty frontier slots at finish.
    #[error("empty reducer retains a pending subtree")]
    EmptyReducerRetainsPendingSubtree,
    /// Non-empty reducer had remaining frontier slots after top merge.
    #[error("finished reducer retains a pending subtree")]
    FinishedReducerRetainsPendingSubtree,
    /// Root subtree was missing after top merge.
    #[error("root subtree disappeared")]
    RootSubtreeDisappeared,
    /// Current subtree was missing when merging a level.
    #[error("current subtree is missing")]
    CurrentSubtreeMissing,
    /// Left frontier subtree and current subtree are not ordered siblings.
    #[error("pending subtrees are not ordered siblings")]
    PendingSubtreesNotOrderedSiblings,
}

impl SmtReducerInvariantReason {
    /// Static message preserved in [`crate::smt::TreeError::ReducerInvariant`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArchiveNodeCountOverflow => "archive node count overflow",
            Self::CurrentSubtreeDisappeared => "current subtree disappeared",
            Self::OrderedSubtreeNotLeftForkChild => "ordered subtree is not the left fork child",
            Self::ForkHeightOutsideFrontier => "fork height outside frontier",
            Self::ForkFrontierAlreadyContainsSubtree => "fork frontier already contains a subtree",
            Self::EmptyReducerRetainsPendingSubtree => "empty reducer retains a pending subtree",
            Self::FinishedReducerRetainsPendingSubtree => {
                "finished reducer retains a pending subtree"
            }
            Self::RootSubtreeDisappeared => "root subtree disappeared",
            Self::CurrentSubtreeMissing => "current subtree is missing",
            Self::PendingSubtreesNotOrderedSiblings => "pending subtrees are not ordered siblings",
        }
    }
}

impl From<SmtReducerInvariantReason> for crate::smt::TreeError {
    fn from(reason: SmtReducerInvariantReason) -> Self {
        Self::ReducerInvariant(reason.as_str())
    }
}

/// Typed store/vendor invariant errors for the CKB SMT facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SmtStoreInvariantError {
    /// Zero leaf was passed to `insert_leaf` instead of `remove_leaf`.
    #[error("zero leaf must be deleted")]
    ZeroLeafMustBeDeleted,
    /// Empty key list was passed to `prove` or `verify`.
    #[error("proof keys must not be empty")]
    ProofKeysMustNotBeEmpty,
}

impl From<SmtStoreInvariantError> for outbe_sparse_merkle_tree_v061::error::Error {
    fn from(error: SmtStoreInvariantError) -> Self {
        Self::Store(error.to_string())
    }
}

impl From<SmtStoreInvariantError> for crate::smt::TreeError {
    fn from(error: SmtStoreInvariantError) -> Self {
        Self::Vendor(error.to_string())
    }
}

/// Typed tree/archive errors surfaced through [`crate::collection_reconstruction::TributePartitionReconstructionError::Tree`].
#[derive(Debug, Error)]
pub enum ArchiveTreeError {
    /// Failed to fsync a proof shard file.
    #[error("sync proof shard: {0}")]
    SyncProofShard(#[source] std::io::Error),
}

impl From<ArchiveTreeError>
    for crate::collection_reconstruction::TributePartitionReconstructionError
{
    fn from(error: ArchiveTreeError) -> Self {
        Self::Tree(error.to_string())
    }
}

/// Typed reasons for [`crate::staging::StagingError::InvalidShardEnvelope`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ShardEnvelopeErrorReason {
    /// Changed shard's parent and new roots are identical.
    #[error("changed shard roots must differ")]
    ChangedShardRootsMustDiffer,
    /// Changed shard has neither branch changes nor leaf changes.
    #[error("changed shard must contain effective records")]
    ChangedShardMustContainEffectiveRecords,
    /// Shard count does not fit in `usize`.
    #[error("shard count is not representable")]
    ShardCountNotRepresentable,
    /// Shard count is not a power of two in `1..=256`.
    #[error("invalid shard count")]
    InvalidShardCount,
    /// Parent or new shard-root vector length does not equal `shard_count`.
    #[error("root vector length mismatch")]
    RootVectorLengthMismatch,
    /// Aggregated top root of shard roots does not match declared top root.
    #[error("aggregate root mismatch")]
    AggregateRootMismatch,
    /// Shard index does not fit in `usize`.
    #[error("shard index overflow")]
    ShardIndexOverflow,
    /// Changed shard index is `>= shard_count`.
    #[error("shard index out of range")]
    ShardIndexOutOfRange,
    /// Shard-root top aggregation failed.
    #[error("invalid shard root aggregate")]
    InvalidShardRootAggregate,
    /// Changed shard batch roots do not match the shard-root vectors at that index.
    #[error("changed shard root mismatch")]
    ChangedShardRootMismatch,
    /// Leaf key in a changed shard is not a canonical field element.
    #[error("invalid leaf key")]
    InvalidLeafKey,
    /// Shard index derivation failed for a changed leaf key.
    #[error("invalid shard derivation")]
    InvalidShardDerivation,
    /// Changed leaf key belongs to a different shard index.
    #[error("misderived leaf shard")]
    MisderivedLeafShard,
    /// Shard root changed at an index that has no `ProvisionalShardBatch`.
    #[error("missing changed shard")]
    MissingChangedShard,
    /// `ProvisionalShardBatch` is present for a shard index whose root did not change.
    #[error("unchanged shard included")]
    UnchangedShardIncluded,
    /// Speculative shard store ended with unchanged root but non-empty record changes.
    #[error("net-no-op shard contains records")]
    NetNoOpShardContainsRecords,
}

impl ShardEnvelopeErrorReason {
    /// Static message preserved in [`crate::staging::StagingError::InvalidShardEnvelope`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChangedShardRootsMustDiffer => "changed shard roots must differ",
            Self::ChangedShardMustContainEffectiveRecords => {
                "changed shard must contain effective records"
            }
            Self::ShardCountNotRepresentable => "shard count is not representable",
            Self::InvalidShardCount => "invalid shard count",
            Self::RootVectorLengthMismatch => "root vector length mismatch",
            Self::AggregateRootMismatch => "aggregate root mismatch",
            Self::ShardIndexOverflow => "shard index overflow",
            Self::ShardIndexOutOfRange => "shard index out of range",
            Self::InvalidShardRootAggregate => "invalid shard root aggregate",
            Self::ChangedShardRootMismatch => "changed shard root mismatch",
            Self::InvalidLeafKey => "invalid leaf key",
            Self::InvalidShardDerivation => "invalid shard derivation",
            Self::MisderivedLeafShard => "misderived leaf shard",
            Self::MissingChangedShard => "missing changed shard",
            Self::UnchangedShardIncluded => "unchanged shard included",
            Self::NetNoOpShardContainsRecords => "net-no-op shard contains records",
        }
    }
}

impl From<ShardEnvelopeErrorReason> for crate::staging::StagingError {
    fn from(reason: ShardEnvelopeErrorReason) -> Self {
        Self::InvalidShardEnvelope(reason.as_str())
    }
}

/// Typed reasons for [`crate::staging::StagingError::InvalidCatalogEnvelope`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum CatalogEnvelopeErrorReason {
    /// Non-Tribute or incomplete collection was passed to `RetirementBatch::new`.
    #[error("only a complete Tribute collection may retire")]
    OnlyCompleteTributeCollectionMayRetire,
    /// Aggregating parent shard roots of a retiring collection failed.
    #[error("invalid retirement shard roots")]
    InvalidRetirementShardRoots,
    /// Computing parent collection root of a retiring collection failed.
    #[error("invalid retirement collection")]
    InvalidRetirementCollection,
    /// Recomputed parent collection root of a retiring collection did not match.
    #[error("retirement parent collection root mismatch")]
    RetirementParentCollectionRootMismatch,
    /// Collection shard count does not match the domain's topology.
    #[error("collection shard count does not match domain topology")]
    CollectionShardCountMismatch,
    /// Computing parent collection root failed.
    #[error("invalid parent collection")]
    InvalidParentCollection,
    /// Recomputed parent collection root did not match declared parent collection root.
    #[error("parent collection root mismatch")]
    ParentCollectionRootMismatch,
    /// Absent parent collection has non-zero parent shard roots.
    #[error("absent collection must start from all-zero shard roots")]
    AbsentCollectionNonZeroParentShardRoots,
    /// Computing new collection root failed.
    #[error("invalid new collection")]
    InvalidNewCollection,
    /// Recomputed new collection root Mismatched or changed shard set is empty.
    #[error("changed collection root or shard payload mismatch")]
    ChangedCollectionRootOrShardPayloadMismatch,
    /// Computing sealed root for an identity batch parent failed.
    #[error("invalid identity parent")]
    InvalidIdentityParent,
    /// Computing sealed root for a benchmark fixture parent failed.
    #[error("invalid benchmark parent")]
    InvalidBenchmarkParent,
    /// Benchmark fixture collection key is invalid.
    #[error("invalid benchmark collection")]
    InvalidBenchmarkCollection,
    /// Aggregating benchmark fixture shard roots failed.
    #[error("invalid benchmark shard roots")]
    InvalidBenchmarkShardRoots,
    /// Computing benchmark fixture collection root failed.
    #[error("invalid benchmark collection root")]
    InvalidBenchmarkCollectionRoot,
    /// Computing benchmark fixture sealed root failed.
    #[error("invalid benchmark root")]
    InvalidBenchmarkRoot,
    /// Identity candidate has a catalog batch or changed catalog root.
    #[error("identity candidate contains catalog changes")]
    IdentityCandidateContainsCatalogChanges,
    /// Changed collections are present without a catalog batch.
    #[error("changed collections require one aggregate catalog batch")]
    ChangedCollectionsRequireCatalogBatch,
    /// Catalog batch parent/new roots do not match the envelope or did not change.
    #[error("catalog batch root mismatch")]
    CatalogBatchRootMismatch,
    /// Catalog batch leaf change count does not match changed collection count.
    #[error("catalog leaf changes do not cover changed collections")]
    CatalogLeafChangesDoNotCoverChangedCollections,
    /// Collection mutation has an unknown domain ID.
    #[error("unknown collection domain")]
    UnknownCollectionDomain,
    /// Recomputed collection root in catalog validation did not match.
    #[error("collection root mismatch")]
    CollectionRootMismatch,
    /// Catalog leaf change does not set the new collection root.
    #[error("catalog leaf is not the changed collection root")]
    CatalogLeafNotChangedCollectionRoot,
    /// Collection retirement has an unknown domain ID.
    #[error("unknown retirement domain")]
    UnknownRetirementDomain,
    /// Retired collection's catalog leaf is not deleted in the catalog batch.
    #[error("retired catalog leaf is not deleted")]
    RetiredCatalogLeafNotDeleted,
    /// Computing parent sealed-root wrapper failed.
    #[error("invalid parent sealed-root wrapper")]
    InvalidParentSealedRootWrapper,
    /// Computing new sealed-root wrapper failed.
    #[error("invalid new sealed-root wrapper")]
    InvalidNewSealedRootWrapper,
    /// Sealed-root wrapper does not match catalog root.
    #[error("sealed-root wrapper mismatch")]
    SealedRootWrapperMismatch,
    /// Snapshot is missing the catalog tree root.
    #[error("catalog root is missing")]
    CatalogRootMissing,
    /// Computing sealed root from snapshot catalog root failed.
    #[error("invalid exact-parent catalog root")]
    InvalidExactParentCatalogRoot,
    /// Snapshot catalog root does not seal to the required authoritative root.
    #[error("exact-parent catalog root does not match authoritative sealed root")]
    ExactParentCatalogRootMismatch,
}

impl CatalogEnvelopeErrorReason {
    /// Static message preserved in [`crate::staging::StagingError::InvalidCatalogEnvelope`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnlyCompleteTributeCollectionMayRetire => {
                "only a complete Tribute collection may retire"
            }
            Self::InvalidRetirementShardRoots => "invalid retirement shard roots",
            Self::InvalidRetirementCollection => "invalid retirement collection",
            Self::RetirementParentCollectionRootMismatch => {
                "retirement parent collection root mismatch"
            }
            Self::CollectionShardCountMismatch => {
                "collection shard count does not match domain topology"
            }
            Self::InvalidParentCollection => "invalid parent collection",
            Self::ParentCollectionRootMismatch => "parent collection root mismatch",
            Self::AbsentCollectionNonZeroParentShardRoots => {
                "absent collection must start from all-zero shard roots"
            }
            Self::InvalidNewCollection => "invalid new collection",
            Self::ChangedCollectionRootOrShardPayloadMismatch => {
                "changed collection root or shard payload mismatch"
            }
            Self::InvalidIdentityParent => "invalid identity parent",
            Self::InvalidBenchmarkParent => "invalid benchmark parent",
            Self::InvalidBenchmarkCollection => "invalid benchmark collection",
            Self::InvalidBenchmarkShardRoots => "invalid benchmark shard roots",
            Self::InvalidBenchmarkCollectionRoot => "invalid benchmark collection root",
            Self::InvalidBenchmarkRoot => "invalid benchmark root",
            Self::IdentityCandidateContainsCatalogChanges => {
                "identity candidate contains catalog changes"
            }
            Self::ChangedCollectionsRequireCatalogBatch => {
                "changed collections require one aggregate catalog batch"
            }
            Self::CatalogBatchRootMismatch => "catalog batch root mismatch",
            Self::CatalogLeafChangesDoNotCoverChangedCollections => {
                "catalog leaf changes do not cover changed collections"
            }
            Self::UnknownCollectionDomain => "unknown collection domain",
            Self::CollectionRootMismatch => "collection root mismatch",
            Self::CatalogLeafNotChangedCollectionRoot => {
                "catalog leaf is not the changed collection root"
            }
            Self::UnknownRetirementDomain => "unknown retirement domain",
            Self::RetiredCatalogLeafNotDeleted => "retired catalog leaf is not deleted",
            Self::InvalidParentSealedRootWrapper => "invalid parent sealed-root wrapper",
            Self::InvalidNewSealedRootWrapper => "invalid new sealed-root wrapper",
            Self::SealedRootWrapperMismatch => "sealed-root wrapper mismatch",
            Self::CatalogRootMissing => "catalog root is missing",
            Self::InvalidExactParentCatalogRoot => "invalid exact-parent catalog root",
            Self::ExactParentCatalogRootMismatch => {
                "exact-parent catalog root does not match authoritative sealed root"
            }
        }
    }
}

impl From<CatalogEnvelopeErrorReason> for crate::staging::StagingError {
    fn from(reason: CatalogEnvelopeErrorReason) -> Self {
        Self::InvalidCatalogEnvelope(reason.as_str())
    }
}

/// Typed lock identifiers for [`crate::tree_manager::TreeServiceError::LockPoisoned`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum TreeServiceLockKind {
    /// Export lease gate mutex/condvar lock.
    #[error("export lease gate")]
    ExportLeaseGate,
    /// Finalization boundary serialization mutex.
    #[error("finalization boundary")]
    FinalizationBoundary,
    /// Speculative candidate cache mutex.
    #[error("candidate cache")]
    CandidateCache,
}

impl TreeServiceLockKind {
    /// Static lock name preserved in [`crate::tree_manager::TreeServiceError::LockPoisoned`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExportLeaseGate => "export lease gate",
            Self::FinalizationBoundary => "finalization boundary",
            Self::CandidateCache => "candidate cache",
        }
    }
}

impl From<TreeServiceLockKind> for crate::tree_manager::TreeServiceError {
    fn from(kind: TreeServiceLockKind) -> Self {
        Self::LockPoisoned(kind.as_str())
    }
}
