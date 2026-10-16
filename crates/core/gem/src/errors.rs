//! Error types for the Gem precompile.

use alloy_primitives::{B256, U256};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use thiserror::Error;

/// Revert errors that the Gem runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemError {
    #[error("gem is non-transferable")]
    NonTransferable,

    #[error("gem not found")]
    GemNotFound,

    #[error("gem is already settled")]
    AlreadySettled,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("gem already exists")]
    AlreadyExists,

    #[error("gem owner is the zero address")]
    ZeroOwner,

    #[error("unknown gem profile selector: {0}")]
    UnknownProfileSelector(u8),

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

    #[error("gem bucket {bucket} member index overflow")]
    BucketMemberIndexOverflow { bucket: B256 },
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
