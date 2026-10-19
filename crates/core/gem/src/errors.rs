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

    #[error("gem group {group_id} existence disagrees with member count {member_count}")]
    GroupExistenceMismatch {
        group_id: WwdEntityId,
        member_count: u32,
    },

    #[error("gem total supply overflow")]
    TotalSupplyOverflow,

    #[error("gem settled count overflow")]
    SettledCountOverflow,

    #[error("gem total supply underflow")]
    TotalSupplyUnderflow,

    #[error("gem group {group_id} settled count underflow")]
    SettledCountUnderflow { group_id: WwdEntityId },

    #[error("gem group {group} is empty")]
    EmptyGroup { group: B256 },

    #[error("gem {gem_id} is not in group {group}")]
    GemNotInGroup { gem_id: U256, group: B256 },

    #[error("gem group {group} member slot {slot} is empty during removal")]
    EmptyGroupSlotOnRemoval { group: B256, slot: u32 },
}

/// Fatal errors that the Gem runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GemFatalError {
    #[error("gem {gem_id} has no group body {group_id}")]
    MissingGroupRecord { gem_id: U256, group_id: WwdEntityId },

    #[error("metadata route received an entity call")]
    MetadataRouteEntityCall,

    #[error("entity route received a metadata call")]
    EntityRouteMetadataCall,

    #[error("gem group {group} member index overflow")]
    GroupMemberIndexOverflow { group: B256 },

    #[error("gem group {group} unpaid load overflow")]
    UnpaidLoadOverflow { group: B256 },

    #[error("gem group {group} unpaid load underflow")]
    UnpaidLoadUnderflow { group: B256 },
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
