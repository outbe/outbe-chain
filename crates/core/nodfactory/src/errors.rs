use alloy_primitives::{Address, U256};
use outbe_common::pow::PowError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodFactoryError {
    #[error("invalid owner")]
    InvalidOwner,

    #[error("nod already exists")]
    NodAlreadyExists,

    #[error("nod not found")]
    NodNotFound,

    #[error("not the owner")]
    NotOwner,

    #[error("nod is not qualified")]
    NodNotQualified,

    #[error("paynote proof names spender {actual}, expected {expected}")]
    PaynoteSpenderMismatch { expected: Address, actual: Address },

    #[error("paynote proof carries asset {actual}, expected {expected}")]
    PaynoteAssetMismatch { expected: Address, actual: Address },

    #[error("paynote covers {covered}, nod cost is {required}")]
    PaynoteUndercoversCost { covered: u128, required: u128 },

    #[error("a zero-cost nod takes no paynote proof")]
    UnexpectedPaynoteProof,

    #[error("nod cost {cost} exceeds the maximum a paynote can cover")]
    SettlementCostTooLarge { cost: U256 },

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

    #[error("no settlement asset registered for reference currency {reference_currency}")]
    NoSettlementAsset { reference_currency: u16 },

    #[error("nod call settlement deadline has expired")]
    CallDeadlineExpired,
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
