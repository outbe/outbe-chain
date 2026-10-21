//! Error types for the confidential Gratis token module.

use crate::precompile::IGratis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GratisError> for PrecompileError {
    fn from(err: GratisError) -> Self {
        use IGratis as I;
        match err {
            GratisError::TransferNotAllowed => revert_with(I::TransferNotAllowed {}),
            GratisError::InvalidOpNonce { expected, provided } => {
                revert_with(I::InvalidOpNonce { expected, provided })
            }
            GratisError::Rejected(value) => revert_with(I::Rejected { value }),
            GratisError::PledgedSupplyOverflow => revert_with(I::PledgedSupplyOverflow {}),
            GratisError::NodChainMismatch => revert_with(I::NodChainMismatch {}),
        }
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
    #[error("owner movement result mismatch")]
    OwnerResultMismatch,
    #[error("pledged supply underflow")]
    PledgedSupplyUnderflow,
}

impl From<GratisFatalError> for PrecompileError {
    fn from(err: GratisFatalError) -> Self {
        let message = err.to_string();
        match err {
            GratisFatalError::EncryptedNodEnclaveFailed(_)
            | GratisFatalError::UnexpectedEnclaveResponse(_) => {
                PrecompileError::EnclaveUnavailable(message)
            }
            GratisFatalError::PledgedSupplyUnderflow
            | GratisFatalError::TestEnclaveError(_)
            | GratisFatalError::MissingFidelityOutcome
            | GratisFatalError::InvalidOwnerMovement
            | GratisFatalError::WrongOpNonce
            | GratisFatalError::CollateralResultMismatch
            | GratisFatalError::OwnerResultMismatch => PrecompileError::Revert(message),
        }
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
