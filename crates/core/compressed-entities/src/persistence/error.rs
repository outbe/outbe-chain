use super::{EnvironmentIdentity, ExactParentIdentity, FinalizedMarker, TreeNamespace};
use crate::staging::StagingError;
use crate::CollectionKey;
use alloy_primitives::B256;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("I/O error at {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("MDBX error at {path}: {message}")]
    Database { path: PathBuf, message: String },
    #[error("MDBX commit outcome is unknown at {path} for marker {marker:?}: {message}")]
    CommitOutcomeUnknown {
        path: PathBuf,
        marker: FinalizedMarker,
        message: String,
    },
    #[error("unsupported CE local storage schema {actual}")]
    UnsupportedLocalSchema { actual: u32 },
    #[error("invalid CE shard count {actual}")]
    InvalidShardCount { actual: u32 },
    #[error("invalid canonical CE topology in environment identity")]
    InvalidTopologyIdentity,
    #[error("non-canonical CE tree namespace")]
    NonCanonicalTreeNamespace,
    #[error("CE tree namespace shard {shard} is outside the fork-fixed domain topology")]
    InvalidNamespaceShard { shard: u32 },
    #[error("CE batch shard count mismatch: expected {expected}, got {actual}")]
    ShardCountMismatch { expected: u32, actual: u32 },
    #[error("environment identity does not match: expected {expected:?}, actual {actual:?}")]
    EnvironmentIdentityMismatch {
        expected: EnvironmentIdentity,
        actual: EnvironmentIdentity,
    },
    #[error("environment identity and finalized marker are only partially initialized")]
    PartialEnvironmentInitialization,
    #[error(
        "CE MDBX contains tree records without identity/marker: {branches} branches, {leaves} leaves, {shard_roots} shard roots"
    )]
    OrphanTreeRecords {
        branches: usize,
        leaves: usize,
        shard_roots: usize,
    },
    #[error("environment and finalized marker commitment schemes differ")]
    EnvironmentMarkerSchemeMismatch,
    #[error("tree format and vendor revision must be non-empty")]
    EmptyEnvironmentIdentityField,
    #[error("invalid height-0 CE marker for genesis {expected_genesis_hash}: actual {actual:?}")]
    InvalidGenesisMarker {
        expected_genesis_hash: B256,
        actual: FinalizedMarker,
    },
    #[error("invalid height-0 shard top root: expected {expected}, got {actual}")]
    InvalidGenesisShardRoot { expected: B256, actual: B256 },
    #[error("finalized marker is missing")]
    MissingFinalizedMarker,
    #[error("missing persisted shard root {index}")]
    MissingShardRoot { index: u32 },
    #[error("missing persisted tree root for namespace {namespace:?}")]
    MissingTreeRoot { namespace: TreeNamespace },
    #[error("catalog sealed-root wrapper mismatch: expected {expected}, got {actual}")]
    CatalogWrapperMismatch { expected: B256, actual: B256 },
    #[error("persisted parent catalog root differs from candidate")]
    ParentCatalogRootMismatch,
    #[error("orphan records exist behind absent catalog collection {collection:?}")]
    OrphanCollectionRecords { collection: CollectionKey },
    #[error("collection {collection:?} root count mismatch: expected {expected}, got {actual}")]
    CollectionRootCountMismatch {
        collection: CollectionKey,
        expected: usize,
        actual: usize,
    },
    #[error("persisted collection leaf key is malformed")]
    MalformedCollectionLeafKey,
    #[error("persisted collection leaf count overflows usize")]
    CollectionLeafCountOverflow,
    #[error("recomputed collection root differs from candidate")]
    NewCollectionRootMismatch,
    #[error("persisted shard root count mismatch: expected {expected}, got {actual}")]
    ShardRootCountMismatch { expected: usize, actual: usize },
    #[error("persisted parent shard roots differ from candidate vector")]
    ParentShardRootsMismatch,
    #[error("persisted resulting shard roots differ from candidate vector")]
    NewShardRootsMismatch,
    #[error("shard-root aggregate mismatch: expected {expected}, got {actual}")]
    ShardRootAggregateMismatch { expected: B256, actual: B256 },
    #[error("malformed {record}: expected {expected}, got {actual} bytes")]
    MalformedCodec {
        record: &'static str,
        expected: &'static str,
        actual: usize,
    },
    #[error("unknown MergeValue tag {0}")]
    UnknownMergeValueTag(u8),
    #[error("trailing bytes in {record}: {trailing}")]
    TrailingBytes {
        record: &'static str,
        trailing: usize,
    },
    #[error("invalid UTF-8 in {record}")]
    InvalidUtf8 { record: &'static str },
    #[error("deterministic record length overflow")]
    LengthOverflow,
    #[error("non-canonical BN254 field element")]
    NonCanonicalField,
    #[error("CKB/Poseidon HASH_ERROR poison value")]
    HashPoison,
    #[error("zero cannot be persisted as a leaf value")]
    ZeroPersistedLeaf,
    #[error("exact parent mismatch: required {required:?}, marker {actual:?}")]
    ExactParentMismatch {
        required: ExactParentIdentity,
        actual: FinalizedMarker,
    },
    #[error("conflicting finalized marker: current {current:?}, next {next:?}")]
    ConflictingFinalizedMarker {
        current: FinalizedMarker,
        next: FinalizedMarker,
    },
    #[error("non-contiguous finalized apply: current {current:?}, next {next:?}")]
    NonContiguousFinalizedApply {
        current: FinalizedMarker,
        next: FinalizedMarker,
    },
    #[error(
        "retention cursor advance out of order: cursor {cursor}, previous {previous}, committed {committed}"
    )]
    RetentionAdvanceOutOfOrder {
        cursor: u64,
        previous: u64,
        committed: u64,
    },
    #[error("staged batch rejected: {0}")]
    Staging(String),
}

impl From<StagingError> for PersistenceError {
    fn from(error: StagingError) -> Self {
        Self::Staging(error.to_string())
    }
}
