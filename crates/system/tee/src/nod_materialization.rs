//! Authenticated protected materialization over the existing certified root.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    TransportError,
};
use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    nod_materialization::ProtectedNodMaterializationV2, profile::poc_schema_limits,
};
use outbe_primitives::{nod_encryption::EncryptedNodV2, tribute_encryption::EncryptedTributeV2};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodMaterializationAuthorityV2 {
    pub chain_id: u64,
    pub head: Vec<u8>,
    pub subtree_height: u8,
    pub sealed_tribute_root: B256,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodSourceV2 {
    pub tribute: EncryptedTributeV2,
    pub proof: Vec<u8>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrepareEncryptedNodsRequestV2 {
    pub authority: NodMaterializationAuthorityV2,
    pub batch: Vec<u8>,
    pub sources: Vec<NodSourceV2>,
}
pub fn hash<T: Serialize + ?Sized>(domain: &[u8], value: &T) -> Result<B256, serde_json::Error> {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(value)?);
    Ok(keccak256(bytes))
}
pub fn attestation<T: Serialize + ?Sized>(
    domain: &[u8],
    inputs: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(value)?);
    Ok(bytes)
}
pub fn prepare_encrypted_nods(
    request: PrepareEncryptedNodsRequestV2,
) -> Result<ProtectedNodMaterializationV2, TransportError> {
    #[cfg(feature = "test-utils")]
    if let Some(result) = test_support::try_prepare(&request) {
        return result;
    }
    let expected = hash(b"outbe/nod/prepare-inputs/v2", &request).map_err(codec)?;
    let (key, response) = crate::nod_transport::request(EnclaveRequest::PrepareEncryptedNodsV2 {
        request: Box::new(request),
    })?;
    match response {
        EnclaveResponse::EncryptedNodsPreparedV2 {
            carrier,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage =
                attestation(b"outbe/nod/prepare-result/v2", expected, &carrier).map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            ProtectedNodMaterializationV2::decode_canonical(&carrier, &poc_schema_limits())
                .map_err(|e| TransportError::Codec(e.to_string()))
        }
        EnclaveResponse::EncryptedNodsCapacityExceededV2 {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = attestation(b"outbe/nod/materialization-capacity/v2", expected, &reason)
                .map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Err(TransportError::NodMaterializationCapacityExceeded(reason))
        }
        EnclaveResponse::EncryptedNodsRejectedV2 {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = attestation(b"outbe/nod/materialization-rejected/v2", expected, &reason)
                .map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Err(TransportError::NodMaterializationRejected(reason))
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
pub fn open_encrypted_nods(
    authority: &NodMaterializationAuthorityV2,
    carrier: &ProtectedNodMaterializationV2,
) -> Result<Vec<EncryptedNodV2>, TransportError> {
    #[cfg(feature = "test-utils")]
    if let Some(result) = test_support::try_open(authority, carrier) {
        return result;
    }
    let carrier = carrier
        .encode_canonical(&poc_schema_limits())
        .map_err(|e| TransportError::Codec(e.to_string()))?;
    let expected = hash(b"outbe/nod/open-inputs/v2", &(authority, &carrier)).map_err(codec)?;
    let (key, response) = crate::nod_transport::request(EnclaveRequest::OpenEncryptedNodsV2 {
        authority: authority.clone(),
        carrier,
    })?;
    match response {
        EnclaveResponse::EncryptedNodsOpenedV2 {
            nods,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage =
                attestation(b"outbe/nod/open-result/v2", expected, &nods).map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Ok(nods)
        }
        EnclaveResponse::EncryptedNodsRejectedV2 {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            let preimage = attestation(b"outbe/nod/materialization-rejected/v2", expected, &reason)
                .map_err(codec)?;
            crate::tribute_v2::verify_attestation(&key, &preimage, &attestation_tag)?;
            Err(TransportError::NodMaterializationRejected(reason))
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
fn codec(error: serde_json::Error) -> TransportError {
    TransportError::Codec(error.to_string())
}

/// Scoped callbacks for exercising the real enclave engines without a sidecar.
/// Callers provide the engines; the host crate contains no fixture secret.
#[cfg(feature = "test-utils")]
pub mod test_support {
    use super::*;
    use std::{cell::Cell, marker::PhantomData, rc::Rc};
    pub type Prepare =
        fn(&PrepareEncryptedNodsRequestV2) -> Result<ProtectedNodMaterializationV2, TransportError>;
    pub type Open = fn(
        &NodMaterializationAuthorityV2,
        &ProtectedNodMaterializationV2,
    ) -> Result<Vec<EncryptedNodV2>, TransportError>;
    #[derive(Clone, Copy)]
    struct Hooks {
        prepare: Prepare,
        open: Open,
    }
    thread_local! { static HOOKS: Cell<Option<Hooks>> = const { Cell::new(None) }; }
    pub struct Guard {
        previous: Option<Hooks>,
        _thread: PhantomData<Rc<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOKS.set(self.previous);
        }
    }
    pub fn scope(prepare: Prepare, open: Open) -> Guard {
        Context {
            hooks: Some(Hooks { prepare, open }),
        }
        .install()
    }
    /// Explicitly carry fixture callbacks into a spawned test worker.
    #[derive(Clone, Copy)]
    pub struct Context {
        hooks: Option<Hooks>,
    }
    pub fn capture() -> Context {
        Context { hooks: HOOKS.get() }
    }
    impl Context {
        pub fn install(self) -> Guard {
            Guard {
                previous: HOOKS.replace(self.hooks),
                _thread: PhantomData,
            }
        }
    }
    pub(super) fn try_prepare(
        request: &PrepareEncryptedNodsRequestV2,
    ) -> Option<Result<ProtectedNodMaterializationV2, TransportError>> {
        HOOKS.get().map(|hooks| (hooks.prepare)(request))
    }
    pub(super) fn try_open(
        authority: &NodMaterializationAuthorityV2,
        carrier: &ProtectedNodMaterializationV2,
    ) -> Option<Result<Vec<EncryptedNodV2>, TransportError>> {
        HOOKS.get().map(|hooks| (hooks.open)(authority, carrier))
    }
}
