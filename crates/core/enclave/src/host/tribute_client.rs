//! Private, authenticated enclave reads for existing calculation consumers.

use outbe_core_common::tribute_encryption::{EncryptedTributeV2, TributeAmountsV2};
use outbe_tee::{tee_call::verify_enclave_attestation, TransportError};

use super::{
    authenticate::authenticate_request,
    consensus_domain_call,
    errors::EnclaveClientError,
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_v2,
};

/// The caller must authenticate the original canonical encrypted body before
/// using this transient calculation view. Public projections retain ciphertext.
pub fn read_tribute_amounts(
    tributes: &[EncryptedTributeV2],
) -> Result<Vec<TributeAmountsV2>, EnclaveClientError> {
    let inputs_hash =
        tribute_v2::tribute_read_inputs_hash(tributes).map_err(TransportError::codec)?;
    let (attestation_pub, response) =
        request_local_enclave(EnclaveRequest::ReadTributeAmountsV2 {
            tributes: tributes.to_vec(),
        })?;
    match response {
        EnclaveResponse::TributeAmountsReadV2 {
            amounts,
            inputs_canonical_hash,
            attestation_tag,
        } => {
            if inputs_canonical_hash != inputs_hash || amounts.len() != tributes.len() {
                return Err(TransportError::EnclaveError(
                    "Tribute read response does not match request".into(),
                )
                .into());
            }
            let preimage = tribute_v2::tribute_read_attestation_preimage(inputs_hash, &amounts)
                .map_err(TransportError::codec)?;
            verify_enclave_attestation(&attestation_pub, &preimage, &attestation_tag)
                .map_err(EnclaveClientError::TributeOfferAttestation)?;
            Ok(amounts)
        }
        _ => Err(TransportError::UnexpectedResponse.into()),
    }
}

/// Thread-local mock enclave hook for deterministic unit tests without a sidecar.
#[cfg(any(test, feature = "test-utils"))]
pub type TestLocalEnclaveHook = std::sync::Arc<
    dyn Fn(&EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError> + Send + Sync,
>;

#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    static TEST_LOCAL_ENCLAVE_HOOK: std::cell::RefCell<Option<TestLocalEnclaveHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Install or clear a thread-local mock enclave hook for tests.
#[cfg(any(test, feature = "test-utils"))]
pub fn set_test_local_enclave_hook(hook: Option<TestLocalEnclaveHook>) {
    TEST_LOCAL_ENCLAVE_HOOK.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn has_test_local_enclave_hook() -> bool {
    TEST_LOCAL_ENCLAVE_HOOK.with(|slot| slot.borrow().is_some())
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn try_test_local_enclave_hook(
    request: &EnclaveRequest,
) -> Option<Result<([u8; 32], EnclaveResponse), TransportError>> {
    TEST_LOCAL_ENCLAVE_HOOK.with(|slot| slot.borrow().clone().map(|hook| hook(request)))
}

pub(crate) fn request_local_enclave(
    request: EnclaveRequest,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    consensus_domain_call(
        request.label(),
        &request,
        || outbe_tee::errors::TeeClientVerificationError::TeeSidecarNotConfigured.into(),
        |key, response| authenticate_request(&request, key, response),
    )
}
