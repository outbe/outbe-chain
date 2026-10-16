use std::fmt::Display;

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

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("nod not found")]
    NodNotFound,

    #[error("nod entry price is out of bounds")]
    EntryPriceOutOfBounds,

    #[error("nod bucket not found")]
    BucketNotFound,

    #[error("nod is already settled")]
    NodAlreadySettled,

    #[error("nod is not settled")]
    NodNotSettled,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("caller is not an active OCOMP materializer")]
    UnauthorizedMaterializer,

    #[error("Nod materialization attempt limit reached for this block")]
    MaterializationAttemptLimit,

    #[error("stale Nod materialization queue sequence")]
    StaleMaterializationQueue,

    #[error("stale Nod materialization cursor")]
    StaleMaterializationCursor,

    #[error("invalid Nod materialization batch shape")]
    InvalidMaterializationBatchShape,

    #[error("invalid Nod materialization proof")]
    InvalidMaterializationProof,

    #[error("certified Nod already exists")]
    DuplicateMaterializedNod,

    #[error("certified Nod generation is not fully materialized")]
    NodGenerationNotMaterialized,

    #[error("settlement deadline has passed")]
    SettlementDeadlinePassed,

    #[error("certified Nod generation overflow")]
    CertifiedGenerationOverflow,

    #[error("a different certified Nod generation already exists for this WWD")]
    DifferentCertifiedGenerationExists,

    #[error("certified Nod target precondition changed")]
    CertifiedTargetPreconditionChanged,

    #[error("certified Nod activation binding mismatch")]
    CertifiedActivationBindingMismatch,

    #[error("certified Nod logical issuance time is zero")]
    CertifiedLogicalIssuanceTimeZero,

    #[error("certified Nod materialization binding is zero")]
    CertifiedMaterializationBindingZero,

    #[error("certified Nod generation contains a zero root")]
    CertifiedGenerationZeroRoot,

    #[error("certified Nod generation count mismatch")]
    CertifiedGenerationCountMismatch,

    #[error(transparent)]
    CreateNodForTest(#[from] outbe_tee::TransportError),

    #[error("Nod materialization FIFO bounds are malformed")]
    MaterializationFifoBoundsMalformed,

    #[error("Nod materialization FIFO tail entry is occupied")]
    MaterializationFifoTailOccupied,

    #[error("Nod materialization FIFO tail overflow")]
    MaterializationFifoTailOverflow,

    #[error("invalid certified Nod protocol value: {0}")]
    InvalidCertifiedProtocolValue(#[source] outbe_ocomp_protocol::ProtocolError),

    #[error("Nod materialization attempt counter overflow")]
    MaterializationAttemptCounterOverflow,

    #[error("Nod materialization head projection is missing")]
    MaterializationHeadProjectionMissing,

    #[error("Nod materialization test load does not match the batch")]
    MaterializationTestLoadMismatch,

    #[error("Nod materialization enclave: {0}")]
    MaterializationEnclave(String),

    #[error("Nod materialization action count does not fit u32")]
    MaterializationActionCountOverflow,

    #[error("Nod materialization cursor overflow")]
    MaterializationCursorOverflow,

    #[error("Nod materialization cursor exceeds certified count")]
    MaterializationCursorExceedsCount,

    #[error("Nod materialization FIFO changed during execution")]
    MaterializationFifoChanged,

    #[error("Nod materialization FIFO head overflow")]
    MaterializationFifoHeadOverflow,

    #[error("encode Nod materialization head: {0}")]
    EncodeMaterializationHead(#[source] outbe_ocomp_protocol::ProtocolError),

    #[error("Nod materialization bypassed its bounded dispatcher")]
    MaterializationBypassedDispatcher,

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

    #[error("closeCallNoticeForTest: bucket not queued")]
    BucketNotQueuedForTest,

    #[error("Nod call price overflow")]
    CallPriceOverflow,
}

impl NodFactoryError {
    pub fn materialization_enclave(err: impl Display) -> Self {
        Self::MaterializationEnclave(err.to_string())
    }

    pub(crate) fn revert_reason(error: &PrecompileError) -> Option<&str> {
        match error {
            PrecompileError::Revert(reason) => Some(reason.as_str()),
            _ => None,
        }
    }
}

impl From<NodFactoryError> for PrecompileError {
    fn from(value: NodFactoryError) -> Self {
        match value {
            enclave @ NodFactoryError::MaterializationEnclave(_) => {
                PrecompileError::EnclaveUnavailable(enclave.to_string())
            }
            other => PrecompileError::Revert(other.to_string()),
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
