use crate::precompile::INodFactory;
use alloy_primitives::{Bytes, B256};
use alloy_sol_types::SolError;
use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use thiserror::Error;

/// Errors that the Nod factory precompile returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodFactoryError {
    #[error("invalid owner")]
    InvalidOwner,

    #[error("invalid encrypted Nod identity or encoding")]
    InvalidEncryptedNod,

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("nod not found")]
    NodNotFound,

    #[error("nod entry price is out of bounds")]
    EntryPriceOutOfBounds,

    #[error("nod group not found")]
    GroupNotFound,

    #[error("nod is already settled")]
    NodAlreadySettled,

    #[error("nod is not settled")]
    NodNotSettled,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,

    #[error(transparent)]
    CreateNodForTest(#[from] outbe_tee::TransportError),

    #[error("Nod group {group_key} has an empty member slot during cleanup")]
    EmptyGroupMemberSlotDuringCleanup { group_key: B256 },

    #[error("Nod group {group_key} member {nod_id} has no body during cleanup")]
    MissingGroupMemberRecordDuringCleanup {
        group_key: B256,
        nod_id: WwdEntityId,
    },

    #[error("Nod group {group_key} indexes an ineligible member {nod_id}")]
    IneligibleGroupMember {
        group_key: B256,
        nod_id: WwdEntityId,
    },

    #[error("Nod group {group_key} has no body during cleanup")]
    MissingGroupRecordDuringCleanup { group_key: B256 },

    #[error("Nod group {group_key} has no record to call")]
    MissingGroupForCall { group_key: B256 },

    #[error("closeCallNoticeForTest: group not queued")]
    GroupNotQueuedForTest,

    #[error("Nod call price overflow")]
    CallPriceOverflow,

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

pub(crate) fn date_step_error(err: outbe_primitives::time::TimeError) -> PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => NodFactoryError::DateStep { kind, date_key }.into(),
        None => PrecompileError::Fatal(err.to_string()),
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<NodFactoryError> for PrecompileError {
    fn from(value: NodFactoryError) -> Self {
        use INodFactory as I;
        match value {
            NodFactoryError::InvalidOwner => revert_with(I::InvalidOwner {}),
            NodFactoryError::InvalidEncryptedNod => revert_with(I::InvalidEncryptedNod {}),
            NodFactoryError::NodAlreadyExists => revert_with(I::NodAlreadyExists {}),
            NodFactoryError::NodNotFound => revert_with(I::NodNotFound {}),
            NodFactoryError::EntryPriceOutOfBounds => revert_with(I::EntryPriceOutOfBounds {}),
            NodFactoryError::GroupNotFound => revert_with(I::GroupNotFound {}),
            NodFactoryError::NodAlreadySettled => revert_with(I::NodAlreadySettled {}),
            NodFactoryError::NodNotSettled => revert_with(I::NodNotSettled {}),
            NodFactoryError::InsufficientProofOfWork => revert_with(I::InsufficientProofOfWork {}),
            NodFactoryError::SettlementDeadlinePassed => {
                revert_with(I::SettlementDeadlinePassed {})
            }
            NodFactoryError::CreateNodForTest(err) => revert_with(I::CreateNodForTest {
                reason: err.to_string(),
            }),
            NodFactoryError::EmptyGroupMemberSlotDuringCleanup { group_key } => {
                revert_with(I::EmptyGroupMemberSlotDuringCleanup {
                    groupKey: group_key,
                })
            }
            NodFactoryError::MissingGroupMemberRecordDuringCleanup { group_key, nod_id } => {
                revert_with(I::MissingGroupMemberRecordDuringCleanup {
                    groupKey: group_key,
                    nodId: nod_id.to_u256(),
                })
            }
            NodFactoryError::IneligibleGroupMember { group_key, nod_id } => {
                revert_with(I::IneligibleGroupMember {
                    groupKey: group_key,
                    nodId: nod_id.to_u256(),
                })
            }
            NodFactoryError::MissingGroupRecordDuringCleanup { group_key } => {
                revert_with(I::MissingGroupRecordDuringCleanup {
                    groupKey: group_key,
                })
            }
            NodFactoryError::MissingGroupForCall { group_key } => {
                revert_with(I::MissingGroupForCall {
                    groupKey: group_key,
                })
            }
            NodFactoryError::GroupNotQueuedForTest => revert_with(I::GroupNotQueuedForTest {}),
            NodFactoryError::CallPriceOverflow => revert_with(I::CallPriceOverflow {}),
            NodFactoryError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}

impl From<PowError> for NodFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
