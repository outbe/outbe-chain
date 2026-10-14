//! Typed errors for the host-side TEE integration crate.

mod balance;
mod call_context;
mod crypto;
mod dcap_fixture;
mod quote;
mod session;
mod transport;

pub use balance::BalanceClientError;
pub use call_context::{CallContextError, CallContextProviderError};
pub use crypto::LocalCryptoError;
pub use dcap_fixture::DcapFixtureError;
pub use quote::QuoteParseError;
pub use session::DevIdentityMismatchError;
pub use transport::TransportError;

#[cfg(test)]
mod tests {
    use super::TransportError;

    #[test]
    fn a_missing_session_is_this_nodes_fault() {
        let error = crate::tribute_client::read_tribute_amounts(&[]).unwrap_err();
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
            TransportError::NodMintRejected("x".into()),
        ] {
            assert!(!error.is_node_local(), "{error}");
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
