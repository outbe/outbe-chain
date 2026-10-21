//! Error types for the confidential Promis token module.

use crate::precompile::IPromis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_tee::protocol::EnclaveResponse;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisError {
    #[error("invalid op nonce: expected {expected}, got {provided}")]
    InvalidOpNonce { expected: u64, provided: u64 },
    #[error("{0}")]
    Rejected(String),
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<PromisError> for PrecompileError {
    fn from(err: PromisError) -> Self {
        use IPromis as I;
        match err {
            PromisError::InvalidOpNonce { expected, provided } => {
                revert_with(I::InvalidOpNonce { expected, provided })
            }
            PromisError::Rejected(value) => revert_with(I::Rejected { value }),
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisFatalError {
    #[error("tee_sidecar_unavailable")]
    SidecarUnavailable,
    #[error("tee_sidecar_unavailable: {0}")]
    SidecarRequestFailed(String),
    #[error("enclave ApplyPromisOp error: {0}")]
    EnclaveOpError(String),
    #[error("unexpected enclave response: {0:?}")]
    UnexpectedEnclaveResponse(Box<EnclaveResponse>),
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
    #[error("tee_promis_attestation_invalid: {0}")]
    AttestationInvalid(String),
    #[error("promis total_supply overflow")]
    TotalSupplyOverflow,
    #[error("promis total_supply underflow")]
    TotalSupplyUnderflow,
}

impl PromisFatalError {
    pub fn sidecar_request_failed(err: impl std::fmt::Display) -> Self {
        Self::SidecarRequestFailed(err.to_string())
    }

    pub fn attestation_invalid(err: impl std::fmt::Display) -> Self {
        Self::AttestationInvalid(err.to_string())
    }
}

impl From<PromisFatalError> for PrecompileError {
    fn from(err: PromisFatalError) -> Self {
        let message = err.to_string();
        match err {
            PromisFatalError::SidecarUnavailable
            | PromisFatalError::SidecarRequestFailed(_)
            | PromisFatalError::EnclaveOpError(_)
            | PromisFatalError::UnexpectedEnclaveResponse(_)
            | PromisFatalError::EnclaveNondeterminism
            | PromisFatalError::AttestationInvalid(_) => {
                PrecompileError::EnclaveUnavailable(message)
            }
            PromisFatalError::TotalSupplyOverflow | PromisFatalError::TotalSupplyUnderflow => {
                PrecompileError::Revert(message)
            }
        }
    }
}
