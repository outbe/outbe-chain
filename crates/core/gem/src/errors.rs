//! Error types for the Gem precompile.

use crate::precompile::IGem;
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolError;
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

    #[error("gem settled count overflow")]
    SettledCountOverflow,

    #[error("gem group {group_id} settled count underflow")]
    SettledCountUnderflow { group_id: WwdEntityId },

    #[error("gem group {group} is empty")]
    EmptyGroup { group: B256 },

    #[error("gem {gem_id} is not in group {group}")]
    GemNotInGroup { gem_id: U256, group: B256 },

    #[error("gem group {group} member slot {slot} is empty during removal")]
    EmptyGroupSlotOnRemoval { group: B256, slot: u32 },

    #[error("Gem state changed after loading")]
    GemStateChangedAfterLoading,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },

    #[error("gem currencies spell no series id")]
    InvalidSeriesId,

    #[error("gem series has no free sub-series number")]
    SubSeriesOverflow,
}

impl From<outbe_primitives::series_id::InvalidSeriesId> for GemError {
    fn from(_: outbe_primitives::series_id::InvalidSeriesId) -> Self {
        Self::InvalidSeriesId
    }
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => GemError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
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

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GemError> for PrecompileError {
    fn from(value: GemError) -> Self {
        use IGem as I;
        match value {
            GemError::NonTransferable => revert_with(I::NonTransferable {}),
            GemError::GemNotFound => revert_with(I::GemNotFound {}),
            GemError::AlreadySettled => revert_with(I::AlreadySettled {}),
            GemError::AlreadyExists => revert_with(I::AlreadyExists {}),
            GemError::ZeroOwner => revert_with(I::ZeroOwner {}),
            GemError::UnknownProfileSelector(value) => {
                revert_with(I::UnknownProfileSelector { value })
            }
            GemError::GroupExistenceMismatch {
                group_id,
                member_count,
            } => revert_with(I::GroupExistenceMismatch {
                groupId: group_id.to_u256(),
                memberCount: member_count,
            }),
            GemError::SettledCountOverflow => revert_with(I::SettledCountOverflow {}),
            GemError::SettledCountUnderflow { group_id } => revert_with(I::SettledCountUnderflow {
                groupId: group_id.to_u256(),
            }),
            GemError::EmptyGroup { group } => revert_with(I::EmptyGroup { group }),
            GemError::GemNotInGroup { gem_id, group } => revert_with(I::GemNotInGroup {
                gemId: gem_id,
                group,
            }),
            GemError::EmptyGroupSlotOnRemoval { group, slot } => {
                revert_with(I::EmptyGroupSlotOnRemoval { group, slot })
            }
            GemError::GemStateChangedAfterLoading => revert_with(I::GemStateChangedAfterLoading {}),
            GemError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
            GemError::InvalidSeriesId => revert_with(I::InvalidSeriesId {}),
            GemError::SubSeriesOverflow => revert_with(I::SubSeriesOverflow {}),
        }
    }
}

impl From<GemFatalError> for PrecompileError {
    fn from(value: GemFatalError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
