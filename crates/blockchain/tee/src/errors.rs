//! Typed errors for the host-side TEE integration crate.

mod call_context;
mod host_collateral;
mod protocol;
mod tee_attestation_activation;
mod tee_crypto;
mod transport;
mod verification;

pub use call_context::{CallContextError, CallContextProviderError};
pub(crate) use call_context::{
    EnclaveResponseError, TypedCallRouteError, WitnessAvailabilityError, WitnessCandidateRejected,
};
pub use outbe_dcap::errors::HostCollateralError;
pub use outbe_dcap::errors::QuoteParseError;
pub use outbe_dcap::errors::{DcapFixtureError, NativeQvlError};
pub use protocol::{
    FinalizedAdmissionMalformedReason, TeeDkgMalformedWireReason, TeeDkgUnexpectedResponseReason,
    UpgradeTransferError,
};
pub use tee_attestation_activation::{ActivationCause, TeeAttestationActivationError};
pub use tee_crypto::TeeCryptoError;
pub use transport::{AppErrorDescriptor, TransportError};
pub use verification::{NodeHostRecordError, TeeVerificationError};

#[cfg(test)]
mod tests {
    use super::{AppErrorDescriptor, TransportError};
    use crate::tee_call::TeeAttestationError;

    const TEST_ATTESTATION_DESCRIPTOR: AppErrorDescriptor =
        AppErrorDescriptor::new("test_app_attestation", "test attestation signature invalid");
    const TEST_REJECTION_DESCRIPTOR: AppErrorDescriptor =
        AppErrorDescriptor::new("test_app_rejected", "test operation rejected");

    #[test]
    fn a_missing_resident_runtime_is_this_nodes_fault() {
        let error: TransportError = crate::InProcessDispatchError::ContextNotInstalled.into();
        assert!(
            matches!(
                error,
                TransportError::InProcess(crate::InProcessDispatchError::ContextNotInstalled)
            ),
            "{error}"
        );
        assert!(error.is_node_local());
        assert_eq!(error.metric_class(), "unavailable");
    }

    #[test]
    fn an_enclave_answer_or_a_bad_result_is_not_node_local() {
        for error in [
            TransportError::enclave_error(std::io::Error::other("x")),
            TransportError::UnexpectedResponse,
            TransportError::codec(std::io::Error::other("x")),
            TransportError::attestation(std::io::Error::other("x")),
            TransportError::app_attestation(
                TEST_ATTESTATION_DESCRIPTOR,
                TeeAttestationError::InvalidSignatureLength { actual: 1 },
            ),
            TransportError::app_rejected(TEST_REJECTION_DESCRIPTOR, "x"),
        ] {
            assert!(!error.is_node_local(), "{error}");
        }
        assert!(TransportError::Io(std::io::ErrorKind::NotFound.into()).is_node_local());
    }
}
