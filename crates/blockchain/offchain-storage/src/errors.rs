//! Off-chain storage error types.

use std::{ops::Range, path::PathBuf};

pub use crate::session::StorageCloseError;
pub use crate::types::{StorageError, StorageErrorKind};

/// Typed validation causes converted into [`StorageError::InvalidArgument`].
#[derive(Debug, thiserror::Error)]
pub enum StorageValidationError {
    #[error("cannot read storage configuration {}: {error}", path.display())]
    CannotReadConfig {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("invalid storage TOML at byte range {span:?}")]
    InvalidToml { span: Option<Range<usize>> },
    #[error("storage configuration must have a parent directory")]
    MissingParentDirectory,
    #[error("cannot encode storage configuration")]
    CannotEncodeConfig,
    #[error("scan limit does not fit i64")]
    ScanLimitExceedsI64,
    #[error("read-only partition session")]
    ReadOnlyPartitionSession,
    #[error("WWD strategy requires a day")]
    WwdStrategyRequiresDay,
    #[error("owner modulo requires an owner")]
    OwnerModuloRequiresOwner,
    #[error("RocksDB secondary directory is already owned or cannot be locked: {error}")]
    SecondaryAlreadyOwnedOrCannotLock {
        #[source]
        error: std::fs::TryLockError,
    },
    #[error("key must not be empty")]
    EmptyKey,
    #[error("key exceeds {max} bytes")]
    KeyExceedsMaxBytes { max: usize },
    #[error("value exceeds {max} bytes")]
    ValueExceedsMaxBytes { max: usize },
    #[error("metadata exceeds {max} entries")]
    MetadataExceedsMaxEntries { max: usize },
    #[error("atomic batch exceeds {max} operations")]
    AtomicBatchExceedsMaxOperations { max: usize },
    #[error("stored record exceeds {max} value and metadata bytes")]
    StoredRecordExceedsMaxBytes { max: usize },
    #[error("atomic batch size overflow")]
    AtomicBatchSizeOverflow,
    #[error("atomic batch exceeds {max} value and metadata bytes")]
    AtomicBatchExceedsMaxBytes { max: usize },
    #[error("scan prefix exceeds {max} bytes")]
    ScanPrefixExceedsMaxBytes { max: usize },
    #[error("scan limit must be between 1 and {max}")]
    ScanLimitOutOfBounds { max: usize },
    #[error("namespace exceeds {max} bytes")]
    NamespaceExceedsMaxBytes { max: usize },
    #[error("reserved MongoDB namespace")]
    ReservedMongoNamespace,
    #[error("metadata key length must be between 1 and {max} bytes")]
    MetadataKeyLengthOutOfBounds { max: usize },
    #[error("metadata value length must be between 1 and {max} bytes")]
    MetadataValueLengthOutOfBounds { max: usize },
}

impl From<StorageValidationError> for StorageError {
    fn from(error: StorageValidationError) -> Self {
        Self::InvalidArgument(error.to_string())
    }
}

/// Typed corruption causes converted into [`StorageError::Corruption`].
#[derive(Debug, thiserror::Error)]
pub enum StorageCorruptionError {
    #[error("unscoped MongoDB layout: use a fresh database for schema 3; automatic migration is disabled")]
    UnscopedMongoLayout,
    #[error("MongoDB storage document has a non-string _id")]
    MongoNonStringId,
    #[error("MongoDB storage document has invalid key encoding")]
    MongoInvalidKeyEncoding,
    #[error("MongoDB storage document contains an invalid key")]
    MongoInvalidKey,
    #[error("MongoDB storage document has a non-binary value")]
    MongoNonBinaryValue,
    #[error("MongoDB storage document contains an oversized value")]
    MongoOversizedValue,
    #[error("invalid MongoDB _projection metadata: {error}")]
    MongoInvalidProjectionMetadata {
        #[source]
        error: Box<StorageError>,
    },
    #[error("missing scoped collection namespace")]
    MissingScopedCollectionNamespace,
    #[error("invalid numbered collection")]
    InvalidNumberedCollection,
    #[error("invalid collection partition number")]
    InvalidCollectionPartitionNumber,
    #[error("malformed partition journal")]
    MalformedPartitionJournal,
    #[error("unsupported partition journal version")]
    UnsupportedPartitionJournalVersion,
    #[error("truncated partition journal checksum")]
    TruncatedPartitionJournalChecksum,
    #[error(
        "legacy entity layout: schema 3 requires a fresh root; automatic migration is disabled"
    )]
    LegacyEntityLayout,
    #[error("non-UTF8 partition directory")]
    NonUtf8PartitionDirectory,
    #[error("non-numeric entity partition")]
    NonNumericEntityPartition,
    #[error("malformed entity locator")]
    MalformedEntityLocator,
    #[error("invalid entity locator")]
    InvalidEntityLocator,
    #[error("missing WWD routing prefix")]
    MissingWwdRoutingPrefix,
    #[error("invalid WWD routing prefix")]
    InvalidWwdRoutingPrefix,
    #[error("missing owner routing prefix")]
    MissingOwnerRoutingPrefix,
    #[error("invalid owner routing prefix")]
    InvalidOwnerRoutingPrefix,
    #[error("pending generation overflow")]
    PendingGenerationOverflow,
    #[error("invalid RocksDB storage key")]
    InvalidRocksDbStorageKey,
    #[error("record exceeds scan page bound")]
    RecordExceedsScanPageBound,
}

impl From<StorageCorruptionError> for StorageError {
    fn from(error: StorageCorruptionError) -> Self {
        Self::Corruption(error.to_string())
    }
}

/// Typed RocksDB record codec corruption causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RocksDbCodecError {
    #[error("RocksDB storage codec: unknown record version")]
    UnknownRecordVersion,
    #[error("RocksDB storage codec: value length exceeds bound")]
    ValueLengthExceedsBound,
    #[error("RocksDB storage codec: invalid value")]
    InvalidValue,
    #[error("RocksDB storage codec: trailing record bytes")]
    TrailingRecordBytes,
    #[error("RocksDB storage codec: record exceeds scan page bound")]
    RecordExceedsScanPageBound,
    #[error("RocksDB storage codec: metadata presence tag")]
    MetadataPresenceTag,
    #[error("RocksDB storage codec: too many metadata entries")]
    TooManyMetadataEntries,
    #[error("RocksDB storage codec: metadata keys are not strictly ordered")]
    MetadataKeysNotOrdered,
    #[error("RocksDB storage codec: invalid metadata")]
    InvalidMetadata,
    #[error("RocksDB storage codec: metadata key length")]
    MetadataKeyLength,
    #[error("RocksDB storage codec: metadata key encoding")]
    MetadataKeyEncoding,
    #[error("RocksDB storage codec: metadata value length")]
    MetadataValueLength,
    #[error("RocksDB storage codec: metadata value encoding")]
    MetadataValueEncoding,
    #[error("RocksDB storage codec: truncated record")]
    TruncatedRecord,
}

impl From<RocksDbCodecError> for StorageError {
    fn from(error: RocksDbCodecError) -> Self {
        Self::Corruption(error.to_string())
    }
}

/// Typed MongoDB lifecycle failure causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MongoLifecycleError {
    #[error("Mongo lease renewer panicked")]
    RenewerPanicked,
}

/// Typed corruption causes detected during partitioned prefix scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PartitionScanCorruptionError {
    #[error("duplicate entity key across partitions")]
    DuplicateEntityKeyAcrossPartitions,
    #[error("partition record exceeds page bound")]
    PartitionRecordExceedsPageBound,
    #[error("partition scan exceeds entry bound")]
    PartitionScanExceedsEntryBound,
    #[error("partition scan violates prefix or ordering")]
    PartitionScanViolatesPrefixOrOrdering,
    #[error("partition scan exceeds page byte bound")]
    PartitionScanExceedsPageByteBound,
    #[error("partition scan continuation is not its last returned key")]
    PartitionScanContinuationNotLastReturnedKey,
}

impl From<PartitionScanCorruptionError> for StorageError {
    fn from(error: PartitionScanCorruptionError) -> Self {
        Self::Corruption(error.to_string())
    }
}
