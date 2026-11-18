//! Error types for the Fidelity module.
//!
//! Wrapping another error uses `#[source]`. Display text that a revert or a
//! signed query rejection must carry is rendered at that boundary.

use std::fmt;

use crate::precompile::IFidelity;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_core_common::confidential_tee::TeeError;
use outbe_primitives::error::PrecompileError;
use outbe_tee::TransportError;
use thiserror::Error;

/// Cohort decode, evaluation, and query-authorization failures.
///
/// The `fidelity:` prefix is part of the signed query-rejection reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CohortError {
    /// Interior is shorter than the 16-byte header.
    #[error("cohort interior shorter than header")]
    CohortInteriorShorterThanHeader,
    /// Active or sold byte length overflowed `usize`.
    #[error("cohort counts overflow")]
    CohortCountsOverflow,
    /// Declared entries do not fit in the interior.
    #[error("cohort interior truncated")]
    CohortInteriorTruncated,
    /// Active count does not fit in `u32`.
    #[error("active cohort count overflow")]
    ActiveCohortCountOverflow,
    /// Sold count does not fit in `u32`.
    #[error("sold cohort count overflow")]
    SoldCohortCountOverflow,
    /// RCFI fixed-point arithmetic overflowed.
    #[error("rcfi arithmetic overflow")]
    RcfiArithmeticOverflow,
    /// League arithmetic overflowed.
    #[error("league arithmetic overflow")]
    LeagueArithmeticOverflow,
    /// Authorization was signed for a different chain.
    #[error("query authorization is for a different chain")]
    QueryAuthDifferentChain,
    /// Authorization expiry is before the block timestamp.
    #[error("query authorization expired")]
    QueryAuthExpired,
    /// Owner signature is not 65 bytes.
    #[error("owner signature must be 65 bytes")]
    OwnerSignatureLength,
    /// Recovered signer is not the account.
    #[error("owner signature does not control account")]
    OwnerSignatureAccountMismatch,
}

/// Engine failure. Cohort errors keep the historical `fidelity:` prefix.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Cohort or query-authorization failure.
    #[error("fidelity: {0}")]
    Cohort(#[from] CohortError),
    /// Envelope cryptography failed.
    #[error(transparent)]
    Crypto(#[from] TeeError),
}

/// User-facing query error. A signed rejection has only its reason string.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityError {
    /// The in-process stand-in rejected the query before a witness round-trip.
    #[error("enclave QueryFidelityIndex error: {source}")]
    EnclaveQueryError {
        /// Engine failure. The revert string is this error's display.
        #[source]
        source: EngineError,
    },

    /// A verified `FidelityQueryRejected` answer, or a host-local query failure.
    #[error("{reason}")]
    QueryFailed {
        /// Solidity revert string.
        reason: String,
    },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

/// Render `value` for a boundary that stores a `String`.
pub(crate) fn display_string(value: &impl fmt::Display) -> String {
    let mut out = String::new();
    let _ = fmt::Write::write_fmt(&mut out, format_args!("{value}"));
    out
}

impl From<FidelityError> for PrecompileError {
    fn from(err: FidelityError) -> Self {
        use IFidelity as I;
        match err {
            FidelityError::EnclaveQueryError { source } => revert_with(I::EnclaveQueryError {
                reason: display_string(&source),
            }),
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
            reason: display_string(&other),
        }
        .into(),
    }
}

/// Display of an enclave transport error that keeps the historical host sentence.
///
/// `EnclaveError("boom")` prints `boom`. The transport error stays the source.
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

/// Fatal fidelity failure. Consensus and cohort ops surface these as unavailable.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityFatalError {
    /// No in-process runtime is installed.
    #[error("tee_sidecar_unavailable: {context}")]
    SidecarUnavailable {
        /// Operation label.
        context: &'static str,
    },
    /// The runtime returned a transport failure other than an enclave rejection.
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
    #[error("tee_fidelity_attestation_invalid: {source}")]
    AttestationInvalid {
        /// Attestation failure.
        #[source]
        source: TransportError,
    },
    /// A single-owner snapshot returned no league.
    #[error("fidelity snapshot returned no league")]
    SnapshotReturnedNoLeague,
    /// A batch snapshot did not answer each requested owner in order.
    #[error("tee_fidelity_snapshot_unbound")]
    SnapshotUnbound,
}

impl From<FidelityFatalError> for PrecompileError {
    fn from(err: FidelityFatalError) -> Self {
        let revert = matches!(err, FidelityFatalError::SnapshotReturnedNoLeague);
        let message = display_string(&err);
        if revert {
            PrecompileError::Revert(message)
        } else {
            PrecompileError::EnclaveUnavailable(message)
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
        let actual = surface_query_error(fatal_err);
        let expected = PrecompileError::RevertBytes(Bytes::from(
            IFidelity::QueryFailed {
                reason: "fatal: auth rejected".into(),
            }
            .abi_encode(),
        ));
        assert_eq!(display_string(&actual), display_string(&expected));
    }
}
