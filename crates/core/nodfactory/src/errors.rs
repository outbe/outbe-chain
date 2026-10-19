use alloy_primitives::B256;
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
}

impl From<NodFactoryError> for PrecompileError {
    fn from(value: NodFactoryError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

impl From<PowError> for NodFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
