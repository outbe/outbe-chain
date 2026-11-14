//! Typed errors for the host-side TEE integration crate.

mod call_context;
mod client_verification;
mod dcap_fixture;
mod host_collateral;
mod protocol;
mod quote;
mod session;
mod tee_attestation_activation;
mod tee_crypto;
mod transport;

pub use call_context::{CallContextError, CallContextProviderError};
pub use client_verification::{NodeHostRecordError, TeeClientVerificationError};
pub use dcap_fixture::{DcapFixtureError, NativeQvlError};
pub use host_collateral::HostCollateralError;
pub use protocol::{
    FinalizedAdmissionMalformedReason, TeeDkgMalformedWireReason, TeeDkgUnexpectedResponseReason,
    UpgradeTransferError,
};
pub use quote::QuoteParseError;
pub use session::DevIdentityMismatchError;
pub use tee_attestation_activation::TeeAttestationActivationError;
pub use tee_crypto::TeeCryptoError;
pub use transport::{AppErrorDescriptor, TransportError};

#[cfg(test)]
mod tests {
    use super::{AppErrorDescriptor, TeeClientVerificationError, TransportError};
    use crate::tee_call::TeeAttestationError;

    const TEST_ATTESTATION_DESCRIPTOR: AppErrorDescriptor =
        AppErrorDescriptor::new("test_app_attestation", "test attestation signature invalid");
    const TEST_REJECTION_DESCRIPTOR: AppErrorDescriptor =
        AppErrorDescriptor::new("test_app_rejected", "test operation rejected");

    #[test]
    fn a_missing_sidecar_is_this_nodes_fault() {
        let error: TransportError = TeeClientVerificationError::TeeSidecarNotConfigured.into();
        assert!(matches!(error, TransportError::Unavailable(_)), "{error}");
        assert!(error.is_node_local());
        assert_eq!(error.metric_class(), "unavailable");
    }

    #[test]
    fn an_enclave_answer_or_a_bad_result_is_not_node_local() {
        for error in [
            TransportError::EnclaveError("x".into()),
            TransportError::UnexpectedResponse,
            TransportError::Codec("x".into()),
            TransportError::Attestation("x".into()),
            TransportError::app_attestation(
                TEST_ATTESTATION_DESCRIPTOR,
                TeeAttestationError::InvalidSignatureLength { actual: 1 },
            ),
            TransportError::app_rejected(TEST_REJECTION_DESCRIPTOR, "x"),
        ] {
            assert!(!error.is_node_local(), "{error}");
            assert!(!error.is_connection_fault(), "{error}");
        }
        for error in [
            TransportError::IoTimeout {
                operation: "x",
                timeout_secs: 1,
            },
            TransportError::Noise("x".into()),
            TransportError::Handshake("x".into()),
            TransportError::SessionRevoked("x"),
            TransportError::IdentityMismatch("x".into()),
        ] {
            assert!(error.is_node_local(), "{error}");
        }
    }
}
