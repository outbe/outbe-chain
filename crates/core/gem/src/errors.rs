use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemError {
    #[error("gem is non-transferable")]
    NonTransferable,

    #[error("gem not found")]
    GemNotFound,

    #[error("invalid state transition")]
    InvalidState,

    #[error("floor price not met")]
    FloorPriceNotMet,

    #[error("oracle nominal unavailable")]
    OracleUnavailable,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("gem already exists")]
    AlreadyExists,
}

/// Failure at the typed gem persistence boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemRepositoryError {
    /// Backend-neutral storage failure.
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] outbe_offchain_storage::StorageError),
    /// StoredBody or typed payload violates the canonical profile.
    #[error("invalid canonical gem body: {0}")]
    CanonicalBody(#[from] outbe_compressed_entities::CanonicalBodyError),
    /// A canonical integer does not fit the runtime field.
    #[error("canonical gem field {field} is outside the runtime range")]
    CanonicalFieldOutOfRange { field: &'static str },
    /// Page bounds are outside the shared storage contract.
    #[error("page limit {limit} is outside 1..={max}")]
    InvalidPageLimit { limit: usize, max: usize },
    /// A primary item key is not one 32-byte identity.
    #[error("malformed gem primary key")]
    MalformedPrimaryKey,
    /// An owner-index key violates its fixed binary layout.
    #[error("malformed gem owner index key")]
    MalformedIndexKey,
    /// Owner-index values must be exactly empty.
    #[error("gem owner index value is not empty")]
    NonEmptyIndexValue,
    /// Owner-index documents must not carry primary provenance.
    #[error("gem owner index unexpectedly carries metadata")]
    IndexMetadata,
    /// An owner index selects a missing primary body.
    #[error("gem owner index points to missing body {gem_id}")]
    DanglingIndex {
        gem_id: outbe_compressed_entities::WwdEntityId,
    },
    /// The selecting primary key and embedded body id disagree.
    #[error("gem primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch {
        expected: outbe_compressed_entities::WwdEntityId,
        actual: outbe_compressed_entities::WwdEntityId,
    },
    /// An owner index selected a body owned by someone else.
    #[error("gem owner index/body mismatch for {gem_id}")]
    IndexedOwnerMismatch {
        gem_id: outbe_compressed_entities::WwdEntityId,
    },
    /// An id-only repository page is not strictly ascending after its cursor.
    #[error("gem {index} id page is not strictly ascending")]
    NonAscendingIdPage { index: &'static str },
    /// The storage adapter returned a continuation that is not the last page key.
    #[error("gem {index} id page has an invalid continuation")]
    InvalidPageContinuation { index: &'static str },
    /// The selecting bucket key and embedded body key disagree.
    #[error("gem bucket id/body mismatch: expected {expected}, found {actual}")]
    BucketIdBodyMismatch {
        expected: outbe_compressed_entities::WwdEntityId,
        actual: outbe_compressed_entities::WwdEntityId,
    },
    /// A projection session may mutate only identities loaded into its repository snapshot.
    #[error("{entity} projection identity {identity} was not loaded")]
    UntrackedProjectionIdentity {
        entity: &'static str,
        identity: outbe_compressed_entities::WwdEntityId,
    },
}

impl From<GemError> for PrecompileError {
    fn from(value: GemError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
