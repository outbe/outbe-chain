//! Error types for the Fidelity module.

use outbe_primitives::error::PrecompileError;
use outbe_tee::protocol::EnclaveResponse;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityError {
    #[error("{0}")]
    QueryFailed(String),
}

impl From<FidelityError> for PrecompileError {
    fn from(err: FidelityError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

/// Converts user-facing query errors into reverts and preserves revert bytes.
pub(crate) fn surface_query_error(err: PrecompileError) -> PrecompileError {
    match err {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => err,
        other => FidelityError::QueryFailed(other.to_string()).into(),
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityFatalError {
    #[error("tee_sidecar_unavailable: {context}")]
    SidecarUnavailable { context: &'static str },
    #[error("tee_sidecar_unavailable: {context}: {reason}")]
    SidecarRequestFailed {
        context: &'static str,
        reason: String,
    },
    #[error("enclave {context} error: {message}")]
    EnclaveOpError {
        context: &'static str,
        message: String,
    },
    #[error("unexpected enclave response: {0:?}")]
    UnexpectedEnclaveResponse(Box<EnclaveResponse>),
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
    #[error("tee_fidelity_attestation_invalid: {0}")]
    AttestationInvalid(String),
    #[error("fidelity snapshot returned no league")]
    SnapshotReturnedNoLeague,
}

impl FidelityFatalError {
    pub fn sidecar_request_failed(context: &'static str, err: impl core::fmt::Display) -> Self {
        Self::SidecarRequestFailed {
            context,
            reason: err.to_string(),
        }
    }

    pub fn enclave_op_error(context: &'static str, err: impl core::fmt::Display) -> Self {
        Self::EnclaveOpError {
            context,
            message: err.to_string(),
        }
    }

    pub fn attestation_invalid(err: impl core::fmt::Display) -> Self {
        Self::AttestationInvalid(err.to_string())
    }
}

impl From<FidelityFatalError> for PrecompileError {
    fn from(err: FidelityFatalError) -> Self {
        PrecompileError::Fatal(err.to_string())
    }
}
