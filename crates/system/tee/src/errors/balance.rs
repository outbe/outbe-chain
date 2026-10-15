//! Typed errors for confidential balance operations.

use crate::protocol::EnclaveResponse;
use crate::TransportError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors returned by [`crate::balance_client::execute_confidential_balance_op`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BalanceClientError {
    /// Request variant is not a Gratis or Promis balance operation.
    #[error("request is not a confidential balance operation")]
    NotConfidentialBalanceOperation,

    /// Enclave sidecar is not configured on this node.
    #[error("tee_sidecar_unavailable")]
    SidecarUnavailable,

    /// Enclave sidecar failed with a node-local or enclave-state error.
    #[error("tee_sidecar_unavailable: {0}")]
    SidecarRequestUnavailable(String),

    /// Enclave request failed with a deterministic protocol error.
    #[error("enclave request failed: {0}")]
    RequestFailed(String),

    /// Gratis attestation signature check failed.
    #[error("tee_gratis_attestation_invalid: {0}")]
    GratisAttestationInvalid(String),

    /// Promis attestation signature check failed.
    #[error("tee_promis_attestation_invalid: {0}")]
    PromisAttestationInvalid(String),

    /// Enclave returned an explicit operation error.
    #[error("enclave {operation} error: {message}")]
    EnclaveOperationError {
        /// Name of the balance operation.
        operation: &'static str,
        /// Error message from the enclave.
        message: String,
    },

    /// Enclave returned a response variant that does not match the request.
    #[error("unexpected enclave response: {0}")]
    UnexpectedResponse(String),

    /// Enclave echoed a canonical input hash that differs from the request hash.
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
}

impl BalanceClientError {
    /// Convert a transport failure into the corresponding precompile error.
    pub fn request_failure(error: TransportError) -> PrecompileError {
        if error.is_node_local() || matches!(error, TransportError::EnclaveError(_)) {
            Self::SidecarRequestUnavailable(error.to_string()).into()
        } else {
            Self::RequestFailed(error.to_string()).into()
        }
    }

    /// Convert a Gratis attestation failure into a fatal precompile error.
    pub fn gratis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::GratisAttestationInvalid(error.to_string()).into()
    }

    /// Convert a Promis attestation failure into a fatal precompile error.
    pub fn promis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::PromisAttestationInvalid(error.to_string()).into()
    }

    /// Convert an enclave operation error response into an unavailable precompile error.
    pub fn enclave_operation_error(operation: &'static str, message: &str) -> PrecompileError {
        Self::EnclaveOperationError {
            operation,
            message: message.to_owned(),
        }
        .into()
    }

    /// Convert an unexpected enclave response into a fatal precompile error.
    pub fn unexpected_response(response: &EnclaveResponse) -> PrecompileError {
        Self::UnexpectedResponse(format!("{response:?}")).into()
    }
}

impl From<BalanceClientError> for PrecompileError {
    fn from(error: BalanceClientError) -> Self {
        match error {
            BalanceClientError::SidecarUnavailable
            | BalanceClientError::SidecarRequestUnavailable(_)
            | BalanceClientError::EnclaveOperationError { .. }
            | BalanceClientError::RequestFailed(_)
            | BalanceClientError::GratisAttestationInvalid(_)
            | BalanceClientError::PromisAttestationInvalid(_)
            | BalanceClientError::UnexpectedResponse(_)
            | BalanceClientError::EnclaveNondeterminism => {
                Self::EnclaveUnavailable(error.to_string())
            }
            BalanceClientError::NotConfidentialBalanceOperation => Self::Revert(error.to_string()),
        }
    }
}
