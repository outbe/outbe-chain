//! Error types for the confidential Promis token module.

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

impl From<PromisError> for PrecompileError {
    fn from(err: PromisError) -> Self {
        PrecompileError::Revert(err.to_string())
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
