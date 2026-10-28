//! Error types for the Fidelity module.

use crate::precompile::IFidelity;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_tee::protocol::EnclaveResponse;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityError {
    #[error("enclave QueryFidelityIndex error: {reason}")]
    EnclaveQueryError { reason: String },

    #[error("{reason}")]
    QueryFailed { reason: String },
}

impl FidelityError {
    pub fn enclave_query_error(err: impl core::fmt::Display) -> Self {
        Self::EnclaveQueryError {
            reason: err.to_string(),
        }
    }
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<FidelityError> for PrecompileError {
    fn from(err: FidelityError) -> Self {
        use IFidelity as I;
        match err {
            FidelityError::EnclaveQueryError { reason } => {
                revert_with(I::EnclaveQueryError { reason })
            }
            FidelityError::QueryFailed { reason } => revert_with(I::QueryFailed { reason }),
        }
    }
}

/// Converts user-facing query errors into reverts and preserves revert bytes.
pub(crate) fn surface_query_error(err: PrecompileError) -> PrecompileError {
    match err {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => err,
        PrecompileError::EnclaveUnavailable(msg)
            if msg.starts_with("enclave QueryFidelityIndex error:") =>
        {
            FidelityError::QueryFailed { reason: msg }.into()
        }
        err if err.is_node_local() => err,
        other => FidelityError::QueryFailed {
            reason: other.to_string(),
        }
        .into(),
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
    #[error("tee_fidelity_snapshot_unbound")]
    SnapshotUnbound,
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
        let message = err.to_string();
        match err {
            FidelityFatalError::SidecarUnavailable { .. }
            | FidelityFatalError::SidecarRequestFailed { .. }
            | FidelityFatalError::EnclaveOpError { .. }
            | FidelityFatalError::UnexpectedEnclaveResponse(_)
            | FidelityFatalError::EnclaveNondeterminism
            | FidelityFatalError::AttestationInvalid(_)
            | FidelityFatalError::SnapshotUnbound => PrecompileError::EnclaveUnavailable(message),
            FidelityFatalError::SnapshotReturnedNoLeague => PrecompileError::Revert(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_query_error_preserves_node_local_errors() {
        let enclave_err: PrecompileError = FidelityFatalError::SidecarUnavailable {
            context: "QueryFidelityIndex",
        }
        .into();
        assert!(matches!(
            surface_query_error(enclave_err),
            PrecompileError::EnclaveUnavailable(_)
        ));

        let storage_err = PrecompileError::Storage("db read failed".into());
        assert!(matches!(
            surface_query_error(storage_err),
            PrecompileError::Storage(_)
        ));

        let fatal_err = PrecompileError::Fatal("auth rejected".into());
        assert_eq!(
            surface_query_error(fatal_err).to_string(),
            PrecompileError::RevertBytes(Bytes::from(
                IFidelity::QueryFailed {
                    reason: "fatal: auth rejected".into(),
                }
                .abi_encode()
            ))
            .to_string()
        );
    }
}
