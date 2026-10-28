//! Private, authenticated enclave reads for existing calculation consumers.

use outbe_primitives::tribute_encryption::{EncryptedTributeV2, TributeAmountsV2};

use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_v2, TransportError,
};

/// The caller must authenticate the original canonical encrypted body before
/// using this transient calculation view. Public projections retain ciphertext.
pub fn read_tribute_amounts(
    tributes: &[EncryptedTributeV2],
) -> Result<Vec<TributeAmountsV2>, TransportError> {
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
                ));
            }
            let preimage = tribute_v2::tribute_read_attestation_preimage(inputs_hash, &amounts)
                .map_err(TransportError::codec)?;
            tribute_v2::verify_attestation(&attestation_pub, &preimage, &attestation_tag)?;
            Ok(amounts)
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub type TestLocalEnclaveHook = std::sync::Arc<
    dyn Fn(&EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError> + Send + Sync,
>;

#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    static TEST_LOCAL_ENCLAVE_HOOK: std::cell::RefCell<Option<TestLocalEnclaveHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-utils"))]
pub fn set_test_local_enclave_hook(hook: Option<TestLocalEnclaveHook>) {
    TEST_LOCAL_ENCLAVE_HOOK.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

pub(crate) fn request_local_enclave(
    request: EnclaveRequest,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    crate::witness::consensus_call(
        request.label(),
        || {
            #[cfg(any(test, feature = "test-utils"))]
            if let Some(hook) = TEST_LOCAL_ENCLAVE_HOOK.with(|slot| slot.borrow().clone()) {
                return hook(&request);
            }
            let (key, response) = crate::try_with_enclave(|session| {
                (session.attestation_pub(), session.request(&request))
            })
            .ok_or(crate::errors::TeeClientVerificationError::TeeSidecarNotConfigured)?;
            Ok((key, response?))
        },
        |key, response| crate::witness::authenticate_request(&request, key, response),
    )
}
