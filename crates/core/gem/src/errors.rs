//! Error types for the Gem precompile.

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::WwdEntityId;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert errors that the Gem runtime returns.
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

    #[error("invalid owner")]
    InvalidOwner,

    #[error("gem already exists")]
    AlreadyExists,

    #[error("only a genesis gem is issued without a floor")]
    ZeroFloorPrice,

    #[error("unknown gem profile selector: {0}")]
    UnknownProfileSelector(u8),

    #[error("gem bucket {bucket_key} has an empty member slot during forfeit")]
    EmptyMemberSlotDuringForfeit { bucket_key: B256 },

    #[error("gem bucket {bucket_key} member {gem_id} has no body during forfeit")]
    MissingMemberBodyDuringForfeit { bucket_key: B256, gem_id: U256 },

    #[error("gem bucket {bucket_key} indexes an ineligible member {gem_id}")]
    IneligibleBucketMember { bucket_key: B256, gem_id: U256 },

    #[error("gem bucket {bucket_id} existence disagrees with member count {member_count}")]
    BucketExistenceMismatch {
        bucket_id: WwdEntityId,
        member_count: u32,
    },

    #[error("gem total supply overflow")]
    TotalSupplyOverflow,

    #[error("gem settled count overflow")]
    SettledCountOverflow,

    #[error("gem total supply underflow")]
    TotalSupplyUnderflow,

    #[error("gem bucket {bucket_id} settled count underflow")]
    SettledCountUnderflow { bucket_id: WwdEntityId },

    #[error("loaded gem bucket {actual} does not match item bucket {expected}")]
    LoadedBucketMismatch {
        actual: WwdEntityId,
        expected: WwdEntityId,
    },

    #[error("gem bucket {bucket} is empty")]
    EmptyBucket { bucket: B256 },

    #[error("gem {gem_id} is not in bucket {bucket}")]
    GemNotInBucket { gem_id: U256, bucket: B256 },

    #[error("gem bucket {bucket} member slot {slot} is empty during removal")]
    EmptyBucketSlotOnRemoval { bucket: B256, slot: u32 },
}

/// Fatal errors that the Gem runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemFatalError {
    #[error("gem {gem_id} has no bucket body {bucket_id}")]
    MissingBucketBody {
        gem_id: U256,
        bucket_id: WwdEntityId,
    },

    #[error("metadata route received an entity call")]
    MetadataRouteEntityCall,

    #[error("entity route received a metadata call")]
    EntityRouteMetadataCall,

    #[error("gem call price overflow")]
    CallPriceOverflow,

    #[error("gem bucket {bucket} member index overflow")]
    BucketMemberIndexOverflow { bucket: B256 },

    #[error("compressed-entity read returned a non-gem payload")]
    NonGemPayload,

    #[error("corrupt gem body: {0}")]
    CorruptGemBody(GemRepositoryError),

    #[error("compressed-entity read returned a non-gem-bucket payload")]
    NonGemBucketPayload,

    #[error("corrupt gem bucket body: {0}")]
    CorruptGemBucketBody(GemRepositoryError),
}

/// Errors that the gem repository returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemRepositoryError {
    /// Off-chain storage returns an error.
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
    /// An owner-index value is not empty.
    #[error("gem owner index value is not empty")]
    NonEmptyIndexValue,
    /// An owner-index document carries primary metadata.
    #[error("gem owner index unexpectedly carries metadata")]
    IndexMetadata,
    /// An owner index selects a missing primary body.
    #[error("gem owner index points to missing body {gem_id}")]
    DanglingIndex {
        gem_id: outbe_compressed_entities::WwdEntityId,
    },
    /// The primary key and embedded body id do not match.
    #[error("gem primary key/body mismatch: expected {expected}, found {actual}")]
    PrimaryKeyBodyMismatch {
        expected: outbe_compressed_entities::WwdEntityId,
        actual: outbe_compressed_entities::WwdEntityId,
    },
    /// An owner index selects a body with a different owner.
    #[error("gem owner index/body mismatch for {gem_id}")]
    IndexedOwnerMismatch {
        gem_id: outbe_compressed_entities::WwdEntityId,
    },
    /// An id-only repository page is not strictly ascending after its cursor.
    #[error("gem {index} id page is not strictly ascending")]
    NonAscendingIdPage { index: &'static str },
    /// The storage adapter returns a next cursor that is not the last page key.
    #[error("gem {index} id page has an invalid continuation")]
    InvalidPageContinuation { index: &'static str },
    /// The bucket key and embedded body key do not match.
    #[error("gem bucket id/body mismatch: expected {expected}, found {actual}")]
    BucketIdBodyMismatch {
        expected: outbe_compressed_entities::WwdEntityId,
        actual: outbe_compressed_entities::WwdEntityId,
    },
    /// A session mutates an identity that the repository snapshot does not contain.
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

impl From<GemFatalError> for PrecompileError {
    fn from(value: GemFatalError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
