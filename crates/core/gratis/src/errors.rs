//! Error types for the confidential Gratis token module.
//!
//! Wrapping another error uses `#[source]`. Display text that a revert must
//! carry is rendered at that boundary.

use std::fmt::{self, Write as _};

use crate::precompile::IGratis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_tee::TransportError;
use thiserror::Error;

/// User-facing Gratis failure. Rejections become Solidity revert bytes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisError {
    /// The ABI transfer entrypoints are closed.
    #[error("gratis token transfers are not allowed")]
    TransferNotAllowed,
    /// The caller nonce does not match the account.
    #[error("invalid op nonce: expected {expected}, got {provided}")]
    InvalidOpNonce {
        /// Nonce stored for the account.
        expected: u64,
        /// Nonce the caller presented.
        provided: u64,
    },
    /// A verified business rejection, including a signed NOD mint refusal.
    #[error("{0}")]
    Rejected(String),
    /// The public pledged-supply counter overflowed.
    #[error("pledged supply overflow")]
    PledgedSupplyOverflow,
    /// The encrypted NOD names a different chain than storage.
    #[error("NOD chain mismatch")]
    NodChainMismatch,
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

/// Display of an enclave transport error that keeps the historical host sentence.
#[derive(Debug)]
pub struct OpText(pub(crate) TransportError);

impl fmt::Display for OpText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
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

outbe_core_common::define_confidential_fatal_error! {
    /// Fatal Gratis failure. Consensus ops surface these as unavailable, except the
    /// stand-in and the local result checks, which revert.
    GratisFatalError, OpText, "tee_gratis_attestation_invalid: {source}";
    /// The in-process stand-in failed before a witness round-trip.
    #[error("{source}")]
    TestEnclave {
        /// Engine or cryptography failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The collateral result did not match the requested movement.
    #[error("invalid owner movement")]
    InvalidOwnerMovement,
    /// The enclave nonce was not the caller's nonce plus one.
    #[error("enclave returned a wrong op nonce")]
    WrongOpNonce,
    /// Pledged credit and debit did not match the request.
    #[error("collateral result mismatch")]
    CollateralResultMismatch,
    /// Owner blobs did not match the request.
    #[error("owner movement result mismatch")]
    OwnerResultMismatch,
    /// The public pledged-supply counter underflowed.
    #[error("pledged supply underflow")]
    PledgedSupplyUnderflow,
}

fn reverts(err: &GratisFatalError) -> bool {
    matches!(
        err,
        GratisFatalError::TestEnclave { .. }
            | GratisFatalError::InvalidOwnerMovement
            | GratisFatalError::WrongOpNonce
            | GratisFatalError::CollateralResultMismatch
            | GratisFatalError::OwnerResultMismatch
            | GratisFatalError::PledgedSupplyUnderflow
    )
}

impl From<GratisFatalError> for PrecompileError {
    fn from(err: GratisFatalError) -> Self {
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
) -> GratisFatalError {
    GratisFatalError::from_consensus_transport(operation, error)
}

/// A verified NOD mint refusal. The reason is already the signed consensus string.
pub(crate) fn nod_mint_rejection(reason: String) -> PrecompileError {
    GratisError::Rejected(reason).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::{AppErrorDescriptor, TeeAttestationError, TransportError};

    #[test]
    fn nod_rejection_is_revert_bytes_and_transport_failures_stay_unavailable() {
        let rejected = nod_mint_rejection("bad nod proof".into());
        let PrecompileError::RevertBytes(rejected_bytes) = rejected else {
            panic!("expected RevertBytes, got {rejected:?}");
        };
        assert_eq!(
            rejected_bytes,
            Bytes::from(
                IGratis::Rejected {
                    value: "bad nod proof".into(),
                }
                .abi_encode()
            )
        );

        let attestation = PrecompileError::from(map_consensus_error(
            "MineEncryptedNodV1",
            TransportError::app_attestation(
                AppErrorDescriptor::new("nod_attestation", "nod attestation signature invalid"),
                TeeAttestationError::InvalidSignatureLength { actual: 7 },
            ),
        ));
        assert!(matches!(
            attestation,
            PrecompileError::EnclaveUnavailable(message)
                if message.contains("nod attestation signature invalid")
        ));

        let transport = PrecompileError::from(map_consensus_error(
            "MineEncryptedNodV1",
            TransportError::unavailable(std::io::Error::other("sidecar offline")),
        ));
        assert!(matches!(
            transport,
            PrecompileError::EnclaveUnavailable(message) if message.contains("sidecar offline")
        ));
    }

    #[test]
    fn hash_mismatch_is_nondeterminism_and_a_bad_answer_is_unavailable() {
        let mismatch = PrecompileError::from(map_consensus_error(
            "ApplyGratisOp",
            TransportError::UnexpectedResponse,
        ));
        assert!(matches!(
            mismatch,
            PrecompileError::EnclaveUnavailable(message) if message == "tee_enclave_nondeterminism"
        ));

        let denied = PrecompileError::from(map_consensus_error(
            "ApplyGratisOp",
            TransportError::enclave_error(std::io::Error::other("denied")),
        ));
        assert!(matches!(
            denied,
            PrecompileError::EnclaveUnavailable(message)
                if message == "enclave ApplyGratisOp error: denied"
        ));

        let wrong = PrecompileError::from(map_consensus_error(
            "ApplyGratisOp",
            TransportError::codec(WrongAnswer),
        ));
        assert!(matches!(
            wrong,
            PrecompileError::EnclaveUnavailable(message)
                if message.contains("unexpected response from enclave")
        ));

        let bad_tag = PrecompileError::from(map_consensus_error(
            "ApplyGratisOp",
            TransportError::app_attestation(
                crate::attest::GRATIS_OP_ATTESTATION,
                TeeAttestationError::InvalidSignatureLength { actual: 1 },
            ),
        ));
        assert!(matches!(
            bad_tag,
            PrecompileError::EnclaveUnavailable(message)
                if message.starts_with("tee_gratis_attestation_invalid:")
        ));
    }

    #[derive(Debug)]
    struct WrongAnswer;

    impl fmt::Display for WrongAnswer {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("unexpected response from enclave for ApplyGratisOp")
        }
    }

    impl std::error::Error for WrongAnswer {}
}
