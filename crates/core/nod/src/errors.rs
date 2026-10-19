//! Domain error types for the Nod crate.

use alloy_primitives::B256;
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

    #[error("{0}")]
    EntityIdDerivation(String),

    #[error("Nod item canonical identity mismatch: expected {expected}, found {found}")]
    CanonicalIdentityMismatch {
        expected: WwdEntityId,
        found: WwdEntityId,
    },

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

    #[error("Nod total supply overflow during issuance")]
    TotalSupplyOverflowDuringIssuance,

    #[error("Nod total supply underflow during removal")]
    TotalSupplyUnderflowDuringRemoval,

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
    NodNotIndexedInGroup {
        nod_id: WwdEntityId,
        group_key: B256,
    },

    #[error("Nod group {group_key} member slot {slot} is empty during removal")]
    EmptyGroupMemberSlotDuringRemoval { group_key: B256, slot: u32 },

    #[error("NOD amount read failed: {0}")]
    AmountReadFailed(#[source] outbe_tee::TransportError),
}

impl NodError {
    /// Converts an entity-ID derivation failure into a precompile error.
    pub(crate) fn entity_id_derivation(error: impl std::fmt::Display) -> PrecompileError {
        Self::EntityIdDerivation(error.to_string()).into()
    }
}

impl From<NodError> for PrecompileError {
    fn from(value: NodError) -> Self {
        let message = value.to_string();
        match value {
            NodError::AmountReadFailed(error) if error.is_node_local() => {
                PrecompileError::EnclaveUnavailable(message)
            }
            _ => PrecompileError::Revert(message),
        }
    }
}

/// Wraps a test-enclave decryption error in an enclave transport error.
#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn enclave_transport_error(error: impl std::fmt::Display) -> outbe_tee::TransportError {
    outbe_tee::TransportError::EnclaveError(error.to_string())
}
