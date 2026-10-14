//! Error types for the confidential Gratis token module.

use outbe_primitives::error::PrecompileError;
use outbe_tee::protocol::EnclaveResponse;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisError {
    #[error("gratis token transfers are not allowed")]
    TransferNotAllowed,
    #[error("invalid op nonce: expected {expected}, got {provided}")]
    InvalidOpNonce { expected: u64, provided: u64 },
    #[error("{0}")]
    Rejected(String),
    #[error("pledged supply overflow")]
    PledgedSupplyOverflow,
    #[error("NOD chain mismatch")]
    NodChainMismatch,
}

impl From<GratisError> for PrecompileError {
    fn from(err: GratisError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisFatalError {
    #[error("unexpected enclave response: {0:?}")]
    UnexpectedEnclaveResponse(Box<EnclaveResponse>),
    #[error("encrypted NOD enclave operation failed: {0}")]
    EncryptedNodEnclaveFailed(String),
    #[error("{0}")]
    TestEnclaveError(String),
    #[error("enclave dropped the fidelity section outcome")]
    MissingFidelityOutcome,
    #[error("invalid owner movement")]
    InvalidOwnerMovement,
    #[error("enclave returned a wrong op nonce")]
    WrongOpNonce,
    #[error("collateral result mismatch")]
    CollateralResultMismatch,
    #[error("pledged supply underflow")]
    PledgedSupplyUnderflow,
}

impl From<GratisFatalError> for PrecompileError {
    fn from(err: GratisFatalError) -> Self {
        PrecompileError::Fatal(err.to_string())
    }
}

pub(crate) fn map_nod_transport_error(err: outbe_tee::TransportError) -> PrecompileError {
    match err {
        outbe_tee::TransportError::NodMintRejected(reason) => GratisError::Rejected(reason).into(),
        other => GratisFatalError::EncryptedNodEnclaveFailed(other.to_string()).into(),
    }
}

#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn map_test_enclave_error(err: outbe_tee_enclave::errors::TeeError) -> PrecompileError {
    match err {
        outbe_tee_enclave::errors::TeeError::TributeOfferReject(reason) => {
            GratisError::Rejected(reason).into()
        }
        other => GratisFatalError::TestEnclaveError(other.to_string()).into(),
    }
}
