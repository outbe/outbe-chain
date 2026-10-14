//! Domain error types for the Nod crate.

use alloy_primitives::B256;
use outbe_compressed_entities::WwdEntityId;
use outbe_primitives::error::{PrecompileError, SweepFailure};
use thiserror::Error;

use crate::NodRepositoryError;

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

    #[error("bucket not found")]
    BucketNotFound,

    #[error("index out of bounds")]
    IndexOutOfBounds,

    #[error("nod query exceeds the runtime result limit")]
    QueryLimitExceeded,

    #[error("reference currency must be a nonzero ISO 4217 numeric code")]
    ZeroReferenceCurrency,

    #[error("nod floor price overflows")]
    FloorPriceOverflow,

    #[error("nod entry-price snapshot is already frozen")]
    EntryPricesAlreadyFrozen,

    #[error("invalid nod entry-price snapshot")]
    InvalidEntryPriceSnapshot,

    #[error("nod is non-transferable")]
    NonTransferable,

    #[error("unknown nod profile selector: {selector}")]
    UnknownProfileSelector { selector: u8 },

    #[error("Nod bucket {bucket_key} has an empty member slot during forfeit")]
    EmptyBucketMemberSlotDuringForfeit { bucket_key: B256 },

    #[error("Nod bucket {bucket_key} member {nod_id} has no body during forfeit")]
    MissingBucketMemberBodyDuringForfeit {
        bucket_key: B256,
        nod_id: WwdEntityId,
    },

    #[error("Nod bucket {bucket_key} indexes an ineligible member {nod_id}")]
    IneligibleBucketMember {
        bucket_key: B256,
        nod_id: WwdEntityId,
    },

    #[error("Nod bucket {bucket_key} has no body during forfeit")]
    MissingBucketBodyDuringForfeit { bucket_key: B256 },

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

    #[error("Nod bucket identity mismatch: expected {expected}, found {found}")]
    BucketIdentityMismatch { expected: B256, found: B256 },

    #[error("closeCallNoticeForTest: bucket not queued")]
    BucketNotQueuedForTest,

    #[error("absent Nod OCOMP generation has residual state")]
    AbsentOcompGenerationHasResidualState,

    #[error(
        "installed Nod OCOMP generation is malformed: day {day} generation {generation} nod_root {nod_root} bucket_root {bucket_root} manifest {output_manifest_root} job {job_id} bundle {protocol_bundle_hash} semantics {program_semantics_hash}"
    )]
    MalformedOcompGeneration {
        day: u32,
        generation: u64,
        nod_root: B256,
        bucket_root: B256,
        output_manifest_root: B256,
        job_id: B256,
        protocol_bundle_hash: B256,
        program_semantics_hash: B256,
    },

    #[error("Nod materialization FIFO bounds are malformed")]
    MalformedMaterializationFifoBounds,

    #[error("Nod materialization FIFO head entry is missing")]
    MissingMaterializationFifoHeadEntry,

    #[error("Nod materialization FIFO head projection is missing")]
    MissingMaterializationFifoHeadProjection,

    #[error("Nod materialization FIFO head is already complete")]
    CompletedMaterializationFifoHead,

    #[error("installed Nod OCOMP generation metadata is malformed")]
    MalformedOcompGenerationMetadata,

    #[error("Nod call price overflow")]
    CallPriceOverflow,

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("Nod bucket {bucket_id} existence disagrees with member count {member_count}")]
    BucketExistenceDisagreesWithMemberCount {
        bucket_id: WwdEntityId,
        member_count: u32,
    },

    #[error("Nod total supply overflow during issuance")]
    TotalSupplyOverflowDuringIssuance,

    #[error("Nod total supply underflow during removal")]
    TotalSupplyUnderflowDuringRemoval,

    #[error("Nod bucket {bucket_id} settled count underflow")]
    BucketSettledCountUnderflow { bucket_id: WwdEntityId },

    #[error("Nod settlement requires an unpaid item of a called or qualified bucket")]
    SettlementRequiresUnpaidCalledOrQualifiedItem,

    #[error("Nod settled count overflow")]
    SettledCountOverflow,

    #[error("loaded Nod bucket {loaded} does not match item bucket {expected}")]
    LoadedBucketMismatch {
        loaded: WwdEntityId,
        expected: WwdEntityId,
    },

    #[error("Nod bucket {bucket_key} member index overflow")]
    BucketMemberIndexOverflow { bucket_key: B256 },

    #[error("Nod bucket {bucket_key} member count underflow during removal")]
    BucketMemberCountUnderflowDuringRemoval { bucket_key: B256 },

    #[error("Nod {nod_id} is not indexed in bucket {bucket_key}")]
    NodNotIndexedInBucket {
        nod_id: WwdEntityId,
        bucket_key: B256,
    },

    #[error("Nod bucket {bucket_key} member slot {slot} is empty during removal")]
    EmptyBucketMemberSlotDuringRemoval { bucket_key: B256, slot: u32 },

    #[error("compressed-entity read returned a non-Nod-item payload")]
    NonNodItemPayload,

    #[error("compressed-entity read returned a non-Nod-bucket payload")]
    NonNodBucketPayload,
}

impl NodError {
    /// Keeps node-local hashing failures distinct from invalid entity identities.
    pub(crate) fn entity_id_derivation(
        error: outbe_compressed_entities::CommitmentError,
    ) -> PrecompileError {
        if error.is_infrastructure() {
            PrecompileError::CryptoBackendUnavailable(error.to_string())
        } else {
            Self::EntityIdDerivation(error.to_string()).into()
        }
    }
}

impl From<NodError> for PrecompileError {
    fn from(value: NodError) -> Self {
        let message = value.to_string();
        match value {
            NodError::EntityIdDerivation(_)
            | NodError::CanonicalIdentityMismatch { .. }
            | NodError::BucketIdentityMismatch { .. }
            | NodError::AbsentOcompGenerationHasResidualState
            | NodError::MalformedOcompGeneration { .. }
            | NodError::MalformedMaterializationFifoBounds
            | NodError::MissingMaterializationFifoHeadEntry
            | NodError::MissingMaterializationFifoHeadProjection
            | NodError::CompletedMaterializationFifoHead
            | NodError::MalformedOcompGenerationMetadata
            | NodError::CallPriceOverflow
            | NodError::BucketMemberIndexOverflow { .. }
            | NodError::NonNodItemPayload
            | NodError::NonNodBucketPayload => PrecompileError::Fatal(message),
            NodError::InvalidNodIdLength
            | NodError::InvalidNodIdHex
            | NodError::NodNotFound
            | NodError::BucketNotFound
            | NodError::IndexOutOfBounds
            | NodError::QueryLimitExceeded
            | NodError::ZeroReferenceCurrency
            | NodError::FloorPriceOverflow
            | NodError::EntryPricesAlreadyFrozen
            | NodError::InvalidEntryPriceSnapshot
            | NodError::NonTransferable
            | NodError::UnknownProfileSelector { .. }
            | NodError::EmptyBucketMemberSlotDuringForfeit { .. }
            | NodError::MissingBucketMemberBodyDuringForfeit { .. }
            | NodError::IneligibleBucketMember { .. }
            | NodError::MissingBucketBodyDuringForfeit { .. }
            | NodError::EncryptedTermsMismatch
            | NodError::CannotIssueSettledNod
            | NodError::BucketNotQueuedForTest
            | NodError::NodAlreadyExists
            | NodError::BucketExistenceDisagreesWithMemberCount { .. }
            | NodError::TotalSupplyOverflowDuringIssuance
            | NodError::TotalSupplyUnderflowDuringRemoval
            | NodError::BucketSettledCountUnderflow { .. }
            | NodError::SettlementRequiresUnpaidCalledOrQualifiedItem
            | NodError::SettledCountOverflow
            | NodError::LoadedBucketMismatch { .. }
            | NodError::BucketMemberCountUnderflowDuringRemoval { .. }
            | NodError::NodNotIndexedInBucket { .. }
            | NodError::EmptyBucketMemberSlotDuringRemoval { .. } => {
                PrecompileError::Revert(message)
            }
        }
    }
}

/// Maps an enclave transport error from a NOD amount read into a precompile error.
pub(crate) fn map_amount_read_error(error: outbe_tee::TransportError) -> PrecompileError {
    let message = format!("NOD amount read failed: {error}");
    if error.is_node_local() {
        PrecompileError::EnclaveUnavailable(message)
    } else {
        PrecompileError::Fatal(message)
    }
}

/// Classifies a sweep error and propagates compressed-body corruption.
pub(crate) fn sweep_failure(error: &PrecompileError) -> SweepFailure {
    match error {
        PrecompileError::BodyReadCorruption(_) => SweepFailure::Propagate,
        other => other.sweep_failure(),
    }
}

/// Wraps a test-enclave decryption error in an enclave transport error.
#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn enclave_transport_error(error: impl std::fmt::Display) -> outbe_tee::TransportError {
    outbe_tee::TransportError::EnclaveError(error.to_string())
}

impl From<NodRepositoryError> for PrecompileError {
    fn from(value: NodRepositoryError) -> Self {
        use outbe_offchain_storage::StorageErrorKind;

        if value.is_infrastructure() {
            return PrecompileError::CryptoBackendUnavailable(value.to_string());
        }

        match value {
            NodRepositoryError::Storage(error)
                if error.kind() == StorageErrorKind::RequestDeadline =>
            {
                PrecompileError::BodyReadRequestDeadline
            }
            NodRepositoryError::Storage(error) if error.kind() == StorageErrorKind::Unavailable => {
                PrecompileError::BodyReadUnavailable(error.to_string())
            }
            other => PrecompileError::BodyReadCorruption(other.to_string()),
        }
    }
}
