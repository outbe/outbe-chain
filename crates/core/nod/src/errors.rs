//! Domain error types for the Nod crate.

use crate::precompile::INod;
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use thiserror::Error;

/// Represents domain errors in the Nod precompile and runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodError {
    #[error("invalid nodId length")]
    InvalidNodIdLength,

    #[error("invalid nodId hex")]
    InvalidNodIdHex,

    #[error("nod not found")]
    NodNotFound,

    #[error("group not found")]
    GroupNotFound,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("nod query exceeds the runtime result limit")]
    QueryLimitExceeded,

    #[error("reference currency must be a nonzero ISO 4217 numeric code")]
    ZeroReferenceCurrency,

    #[error("nod is non-transferable")]
    NonTransferable,

    #[error("unknown nod profile selector: {selector}")]
    UnknownProfileSelector { selector: u8 },

    #[error("Nod item canonical identity mismatch: expected {expected}, found {found}")]
    CanonicalIdentityMismatch { expected: U256, found: U256 },

    #[error("encrypted NOD terms mismatch")]
    EncryptedTermsMismatch,

    #[error("cannot issue a settled Nod")]
    CannotIssueSettledNod,

    #[error("Nod group identity mismatch: expected {expected}, found {found}")]
    GroupIdentityMismatch { expected: B256, found: B256 },

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("Nod group {group_id} existence disagrees with member count {member_count}")]
    GroupExistenceDisagreesWithMemberCount {
        group_id: WwdEntityId,
        member_count: u32,
    },

    #[error("Nod group {group_key} unpaid load overflow")]
    GroupUnpaidLoadOverflow { group_key: alloy_primitives::B256 },

    #[error("Nod group {group_key} unpaid load underflow")]
    GroupUnpaidLoadUnderflow { group_key: alloy_primitives::B256 },

    #[error("Nod group {group_id} settled count underflow")]
    GroupSettledCountUnderflow { group_id: WwdEntityId },

    #[error("nod is already settled")]
    AlreadySettled,

    #[error("Nod settled count overflow")]
    SettledCountOverflow,

    #[error("Nod group {group_key} member index overflow")]
    GroupMemberIndexOverflow { group_key: B256 },

    #[error("Nod group {group_key} member count underflow during removal")]
    GroupMemberCountUnderflowDuringRemoval { group_key: B256 },

    #[error("Nod {nod_id} is not indexed in group {group_key}")]
    NodNotIndexedInGroup { nod_id: U256, group_key: B256 },

    #[error("Nod group {group_key} member slot {slot} is empty during removal")]
    EmptyGroupMemberSlotDuringRemoval { group_key: B256, slot: u32 },

    #[error("NOD amount read failed: {0}")]
    AmountReadFailed(#[source] outbe_tee::TransportError),

    #[error("Nod state changed after loading")]
    NodStateChangedAfterLoading,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },

    #[error("nod is not forfeited")]
    NodNotForfeited,

    #[error("nod is already settled")]
    NodAlreadySettled,
}

pub(crate) fn date_step_error(err: outbe_primitives::time::TimeError) -> PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => NodError::DateStep { kind, date_key }.into(),
        None => PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<NodError> for PrecompileError {
    fn from(value: NodError) -> Self {
        use INod as I;
        let message = value.to_string();
        match value {
            NodError::AmountReadFailed(error) if error.is_node_local() => {
                PrecompileError::EnclaveUnavailable(message)
            }
            NodError::InvalidNodIdLength => revert_with(I::InvalidNodIdLength {}),
            NodError::InvalidNodIdHex => revert_with(I::InvalidNodIdHex {}),
            NodError::NodNotFound => revert_with(I::NodNotFound {}),
            NodError::GroupNotFound => revert_with(I::GroupNotFound {}),
            NodError::IndexOutOfBounds => revert_with(I::IndexOutOfBounds {}),
            NodError::QueryLimitExceeded => revert_with(I::QueryLimitExceeded {}),
            NodError::ZeroReferenceCurrency => revert_with(I::ZeroReferenceCurrency {}),
            NodError::NonTransferable => revert_with(I::NonTransferable {}),
            NodError::UnknownProfileSelector { selector } => {
                revert_with(I::UnknownProfileSelector { selector })
            }
            NodError::CanonicalIdentityMismatch { expected, found } => {
                revert_with(I::CanonicalIdentityMismatch { expected, found })
            }
            NodError::EncryptedTermsMismatch => revert_with(I::EncryptedTermsMismatch {}),
            NodError::CannotIssueSettledNod => revert_with(I::CannotIssueSettledNod {}),
            NodError::GroupIdentityMismatch { expected, found } => {
                revert_with(I::GroupIdentityMismatch { expected, found })
            }
            NodError::NodAlreadyExists => revert_with(I::NodAlreadyExists {}),
            NodError::GroupExistenceDisagreesWithMemberCount {
                group_id,
                member_count,
            } => revert_with(I::GroupExistenceDisagreesWithMemberCount {
                groupId: group_id.to_u256(),
                memberCount: member_count,
            }),
            NodError::GroupUnpaidLoadOverflow { group_key } => {
                revert_with(I::GroupUnpaidLoadOverflow {
                    groupKey: group_key,
                })
            }
            NodError::GroupUnpaidLoadUnderflow { group_key } => {
                revert_with(I::GroupUnpaidLoadUnderflow {
                    groupKey: group_key,
                })
            }
            NodError::GroupSettledCountUnderflow { group_id } => {
                revert_with(I::GroupSettledCountUnderflow {
                    groupId: group_id.to_u256(),
                })
            }
            NodError::AlreadySettled => revert_with(I::AlreadySettled {}),
            NodError::SettledCountOverflow => revert_with(I::SettledCountOverflow {}),
            NodError::GroupMemberIndexOverflow { group_key } => {
                revert_with(I::GroupMemberIndexOverflow {
                    groupKey: group_key,
                })
            }
            NodError::GroupMemberCountUnderflowDuringRemoval { group_key } => {
                revert_with(I::GroupMemberCountUnderflowDuringRemoval {
                    groupKey: group_key,
                })
            }
            NodError::NodNotIndexedInGroup { nod_id, group_key } => {
                revert_with(I::NodNotIndexedInGroup {
                    nodId: nod_id,
                    groupKey: group_key,
                })
            }
            NodError::EmptyGroupMemberSlotDuringRemoval { group_key, slot } => {
                revert_with(I::EmptyGroupMemberSlotDuringRemoval {
                    groupKey: group_key,
                    slot,
                })
            }
            NodError::AmountReadFailed(error) => revert_with(I::AmountReadFailed {
                reason: error.to_string(),
            }),
            NodError::NodStateChangedAfterLoading => revert_with(I::NodStateChangedAfterLoading {}),
            NodError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
            NodError::NodNotForfeited => revert_with(I::NodNotForfeited {}),
            NodError::NodAlreadySettled => revert_with(I::NodAlreadySettled {}),
        }
    }
}

/// Wraps a test-enclave decryption error in an enclave transport error.
#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn enclave_transport_error(error: impl std::fmt::Display) -> outbe_tee::TransportError {
    outbe_tee::TransportError::EnclaveError(error.to_string())
}
