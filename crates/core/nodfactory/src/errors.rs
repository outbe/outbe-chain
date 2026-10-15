use std::fmt::Display;

use alloy_primitives::{Address, U256};
use outbe_common::pow::PowError;
use outbe_common::settlement::RoundingError;
use outbe_primitives::error::PrecompileError;
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

    #[error("nod is not qualified")]
    NodNotQualified,

    #[error("nod is already settled")]
    NodAlreadySettled,

    #[error("nod is not settled")]
    NodNotSettled,

    #[error("settlement asset {asset} has no registered vault")]
    SettlementAssetNotRegistered { asset: Address },

    #[error("settlement asset currency {iso_code} does not match the nod")]
    SettlementCurrencyMismatch { iso_code: u16 },

    #[error("ERC20 operation failed or returned malformed data")]
    TokenOperationFailed,

    #[error("ERC20 payment did not move the exact Nod cost")]
    SettlementAmountMismatch,

    #[error("insufficient proof of work")]
    InsufficientProofOfWork,

    #[error("{0}")]
    Rounding(RoundingError),

    #[error("oracle nominal unavailable")]
    OracleUnavailable,

    #[error("VWAP snapshot {authorized:#x} does not match the required {required:#x}")]
    VwapSnapshotMismatch { authorized: U256, required: U256 },

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

    #[error("nod call settlement deadline has expired")]
    CallDeadlineExpired,

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

    #[error("assetVaultsCount undecodable")]
    AssetVaultsCountUndecodable,

    #[error("nod cost overflow")]
    NodCostOverflow,

    #[error("settlement asset decimals undecodable")]
    SettlementAssetDecimalsUndecodable,

    #[error("isoCode undecodable")]
    IsoCodeUndecodable,

    #[error("{0}")]
    RevertMessage(String),

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
}

impl NodFactoryError {
    pub fn revert_message(err: impl Display) -> Self {
        Self::RevertMessage(err.to_string())
    }

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

impl From<RoundingError> for NodFactoryError {
    fn from(value: RoundingError) -> Self {
        Self::Rounding(value)
    }
}

impl From<PowError> for NodFactoryError {
    fn from(value: PowError) -> Self {
        match value {
            PowError::InsufficientProofOfWork => Self::InsufficientProofOfWork,
        }
    }
}
