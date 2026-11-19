//! Error types for the confidential Promis token module.
//!
//! Wrapping another error uses `#[source]`. Display text that a revert must
//! carry is rendered at that boundary.

use std::fmt::{self, Write as _};

use crate::precompile::IPromis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_tee::TransportError;
use thiserror::Error;

/// User-facing Promis failure. Rejections become Solidity revert bytes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisError {
    /// The ABI transfer entrypoints are closed.
    #[error("promis is non-transferable")]
    TransferNotAllowed,
    /// The caller nonce does not match the account.
    #[error("invalid op nonce: expected {expected}, got {provided}")]
    InvalidOpNonce {
        /// Nonce stored for the account.
        expected: u64,
        /// Nonce the caller presented.
        provided: u64,
    },
    /// A verified business rejection.
    #[error("{0}")]
    Rejected(String),
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

/// Render `value` for a boundary that stores a `String`.
pub(crate) fn display_string(value: &impl fmt::Display) -> String {
    let mut out = String::new();
    let _ = write!(&mut out, "{value}");
    out
}

impl From<PromisError> for PrecompileError {
    fn from(err: PromisError) -> Self {
        use IPromis as I;
        match err {
            PromisError::TransferNotAllowed => revert_with(I::TransferNotAllowed {}),
            PromisError::InvalidOpNonce { expected, provided } => {
                revert_with(I::InvalidOpNonce { expected, provided })
            }
            PromisError::Rejected(value) => revert_with(I::Rejected { value }),
        }
    }
}

/// Display of an enclave transport error that keeps the historical host sentence.
#[derive(Debug)]
pub struct OpText(pub(crate) TransportError);

impl fmt::Display for OpText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            TransportError::EnclaveError(message) => formatter.write_str(message),
            TransportError::EnclaveCause { source } => write!(formatter, "{source}"),
            other => write!(formatter, "{other}"),
        }
    }
}

impl std::error::Error for OpText {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Fatal Promis failure. Consensus ops surface these as unavailable, except the
/// local result checks, which revert.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisFatalError {
    /// No in-process runtime is installed.
    #[error("tee_sidecar_unavailable: {context}")]
    SidecarUnavailable {
        /// Operation label.
        context: &'static str,
    },
    /// The runtime returned a transport failure other than hash or attestation.
    #[error("tee_sidecar_unavailable: {context}: {source}")]
    SidecarRequestFailed {
        /// Operation label.
        context: &'static str,
        /// Transport failure.
        #[source]
        source: TransportError,
    },
    /// The enclave rejected the operation.
    #[error("enclave {context} error: {message}")]
    EnclaveOpError {
        /// Operation label.
        context: &'static str,
        /// Inner enclave sentence. The transport error is its source.
        #[source]
        message: OpText,
    },
    /// The echoed inputs hash does not match the host's canonical hash.
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
    /// The Ed25519 attestation tag did not verify.
    #[error("tee_promis_attestation_invalid: {source}")]
    AttestationInvalid {
        /// Attestation failure.
        #[source]
        source: TransportError,
    },
    /// Owner blobs did not match the request.
    #[error("owner movement result mismatch")]
    OwnerResultMismatch,
    /// The enclave nonce was not the caller's nonce plus one.
    #[error("enclave returned a wrong op nonce")]
    WrongOpNonce,
}

fn reverts(err: &PromisFatalError) -> bool {
    matches!(
        err,
        PromisFatalError::OwnerResultMismatch | PromisFatalError::WrongOpNonce
    )
}

impl From<PromisFatalError> for PrecompileError {
    fn from(err: PromisFatalError) -> Self {
        let message = display_string(&err);
        if reverts(&err) {
            PrecompileError::Revert(message)
        } else {
            PrecompileError::EnclaveUnavailable(message)
        }
    }
}

/// Map a witness transport error onto the historical host sentences.
///
/// `UnexpectedResponse` is the hash mismatch returned by `authenticate` before
/// it looks at the signature. That sentence is `tee_enclave_nondeterminism`.
pub(crate) fn map_consensus_error(
    operation: &'static str,
    error: TransportError,
) -> PromisFatalError {
    match error {
        TransportError::UnexpectedResponse => PromisFatalError::EnclaveNondeterminism,
        TransportError::EnclaveError(_) | TransportError::EnclaveCause { .. } => {
            PromisFatalError::EnclaveOpError {
                context: operation,
                message: OpText(error),
            }
        }
        TransportError::AppAttestation { .. } => {
            PromisFatalError::AttestationInvalid { source: error }
        }
        other => PromisFatalError::SidecarRequestFailed {
            context: operation,
            source: other,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_consensus_error_keeps_the_historical_sentences() {
        let rows = [
            (
                TransportError::UnexpectedResponse,
                "tee_enclave_nondeterminism",
                true,
            ),
            (
                TransportError::app_attestation(
                    crate::PROMIS_OP_ATTESTATION,
                    outbe_tee::TeeAttestationError::InvalidSignatureLength { actual: 1 },
                ),
                "tee_promis_attestation_invalid:",
                false,
            ),
            (
                TransportError::EnclaveError("denied".into()),
                "enclave ApplyPromisOp error: denied",
                true,
            ),
        ];
        for (error, sentence, exact) in rows {
            let PrecompileError::EnclaveUnavailable(message) =
                PrecompileError::from(map_consensus_error("ApplyPromisOp", error))
            else {
                panic!("expected EnclaveUnavailable");
            };
            assert_eq!(exact, message == sentence, "{message}");
            assert!(exact || message.starts_with(sentence), "{message}");
        }
    }
}
