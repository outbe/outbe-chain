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

pub(crate) struct RejectionSubject<'a> {
    domain: &'a [u8],
    expected: B256,
    reason: &'a str,
}

impl<'a> RejectionSubject<'a> {
    pub(crate) fn new(domain: &'a [u8], expected: B256, reason: &'a str) -> Self {
        Self {
            domain,
            expected,
            reason,
        }
    }
}

pub(crate) fn verify_rejection(
    key: &[u8; 32],
    subject: RejectionSubject<'_>,
    tag: &[u8],
) -> Result<(), TransportError> {
    let preimage = attestation(subject.domain, subject.expected, subject.reason).map_err(codec)?;
    crate::tribute_v2::verify_attestation(key, &preimage, tag)
}

/// The reason and the enclave tag of one rejection response.
struct SignedRejection {
    reason: String,
    attestation_tag: Vec<u8>,
}

impl SignedRejection {
    fn new(reason: String, attestation_tag: Vec<u8>) -> Self {
        Self {
            reason,
            attestation_tag,
        }
    }
}

/// One kind of authenticated NOD rejection: the attestation domain of the
/// enclave tag and the typed error that the client returns.
#[derive(Clone, Copy)]
struct NodRejectionKind {
    domain: &'static [u8],
    error: fn(String) -> TransportError,
}

impl NodRejectionKind {
    const fn new(domain: &'static [u8], error: fn(String) -> TransportError) -> Self {
        Self { domain, error }
    }

    /// Verifies the enclave tag of `rejection` for the request hash `expected`.
    /// Then returns the typed error of this kind with the rejection reason.
    fn reject<T>(
        self,
        key: &[u8; 32],
        expected: B256,
        rejection: SignedRejection,
    ) -> Result<T, TransportError> {
        verify_rejection(
            key,
            RejectionSubject::new(self.domain, expected, &rejection.reason),
            &rejection.attestation_tag,
        )?;
        Err((self.error)(rejection.reason))
    }
}

const MATERIALIZATION_REJECTED: NodRejectionKind = NodRejectionKind::new(
    b"outbe/nod/materialization-rejected/v2",
    TransportError::NodMaterializationRejected,
);

const MATERIALIZATION_CAPACITY: NodRejectionKind = NodRejectionKind::new(
    b"outbe/nod/materialization-capacity/v2",
    TransportError::NodMaterializationCapacityExceeded,
);

const MINT_REJECTED: NodRejectionKind = NodRejectionKind::new(
    b"outbe/nod/mint-rejected/v2",
    TransportError::NodMintRejected,
);

/// A NOD operation that the enclave answers with a signed result or with a
/// signed rejection. Each operation accepts only its own rejection responses.
#[derive(Clone, Copy)]
pub(crate) enum NodOperation {
    Prepare,
    Open,
    Mine,
}

impl NodOperation {
    /// Ends the response match of this operation after its success arm. A
    /// rejection that this operation accepts and that answers `expected` is
    /// verified and returned as its typed error. An enclave error is returned
    /// unchanged. Every other response is unexpected.
    pub(crate) fn reject_or_unexpected<T>(
        self,
        key: &[u8; 32],
        expected: B256,
        response: EnclaveResponse,
    ) -> Result<T, TransportError> {
        let (kind, inputs_canonical_hash, rejection) = match (self, response) {
            (
                Self::Prepare,
                EnclaveResponse::EncryptedNodsCapacityExceededV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
            ) => (
                MATERIALIZATION_CAPACITY,
                inputs_canonical_hash,
                SignedRejection::new(reason, attestation_tag),
            ),
            (
                Self::Prepare | Self::Open,
                EnclaveResponse::EncryptedNodsRejectedV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
            ) => (
                MATERIALIZATION_REJECTED,
                inputs_canonical_hash,
                SignedRejection::new(reason, attestation_tag),
            ),
            (
                Self::Mine,
                EnclaveResponse::EncryptedNodMintRejectedV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
            ) => (
                MINT_REJECTED,
                inputs_canonical_hash,
                SignedRejection::new(reason, attestation_tag),
            ),
            (_, EnclaveResponse::Error { message }) => {
                return Err(TransportError::EnclaveError(message))
            }
            _ => return Err(TransportError::UnexpectedResponse),
        };
        if inputs_canonical_hash != expected {
            return Err(TransportError::UnexpectedResponse);
        }
        kind.reject(key, expected, rejection)
    }
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
        response => NodOperation::Prepare.reject_or_unexpected(&key, expected, response),
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
        response => NodOperation::Open.reject_or_unexpected(&key, expected, response),
    }
}
pub(crate) fn codec(error: serde_json::Error) -> TransportError {
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

#[cfg(test)]
mod rejection_tests {
    use super::{attestation, verify_rejection, RejectionSubject};
    use crate::TransportError;
    use alloy_primitives::B256;
    use ed25519_dalek::{Signer, SigningKey};

    const MATERIALIZATION: &[u8] = b"outbe/nod/materialization-rejected/v2";
    const MINT: &[u8] = b"outbe/nod/mint-rejected/v2";

    #[test]
    fn materialization_and_mint_rejection_tags_cannot_cross_domains() {
        let signer = SigningKey::from_bytes(&[0x53; 32]);
        let public = signer.verifying_key().to_bytes();
        let expected = B256::repeat_byte(0x61);
        let reason = "bad \"quote\"\nline";
        let materialization = attestation(MATERIALIZATION, expected, reason).unwrap();
        let mint = attestation(MINT, expected, reason).unwrap();
        let materialization_tag = signer.sign(&materialization).to_bytes();
        let mint_tag = signer.sign(&mint).to_bytes();

        let mut expected_materialization = MATERIALIZATION.to_vec();
        expected_materialization.extend_from_slice(expected.as_slice());
        expected_materialization.extend_from_slice(br#""bad \"quote\"\nline""#);
        assert_eq!(materialization, expected_materialization);
        assert_eq!(
            attestation(MATERIALIZATION, expected, &reason.to_string()).unwrap(),
            expected_materialization
        );

        assert!(verify_rejection(
            &public,
            RejectionSubject::new(MATERIALIZATION, expected, reason),
            &materialization_tag
        )
        .is_ok());
        assert!(verify_rejection(
            &public,
            RejectionSubject::new(MINT, expected, reason),
            &mint_tag
        )
        .is_ok());
        assert!(matches!(
            verify_rejection(
                &public,
                RejectionSubject::new(MINT, expected, reason),
                &materialization_tag
            ),
            Err(TransportError::TributeOfferAttestation(_))
        ));
        assert!(matches!(
            verify_rejection(
                &public,
                RejectionSubject::new(MATERIALIZATION, expected, reason),
                &mint_tag
            ),
            Err(TransportError::TributeOfferAttestation(_))
        ));
    }

    #[test]
    fn rejection_tag_rejects_a_different_expected_input_hash() {
        let signer = SigningKey::from_bytes(&[0x54; 32]);
        let public = signer.verifying_key().to_bytes();
        let reason = "fixed rejection";
        let signed_hash = B256::repeat_byte(0x62);
        let different_hash = B256::repeat_byte(0x63);

        for domain in [MATERIALIZATION, MINT] {
            let signed = attestation(domain, signed_hash, reason).unwrap();
            let tag = signer.sign(&signed).to_bytes();

            assert!(verify_rejection(
                &public,
                RejectionSubject::new(domain, signed_hash, reason),
                &tag
            )
            .is_ok());
            assert!(matches!(
                verify_rejection(
                    &public,
                    RejectionSubject::new(domain, different_hash, reason),
                    &tag
                ),
                Err(TransportError::TributeOfferAttestation(_))
            ));
        }
    }
}

#[cfg(test)]
mod response_tests {
    use super::{attestation, NodOperation};
    use crate::{protocol::EnclaveResponse, TransportError};
    use alloy_primitives::B256;
    use ed25519_dalek::{Signer, SigningKey};

    const EXPECTED: B256 = B256::repeat_byte(0x71);
    const REASON: &str = "fixed rejection";

    #[derive(Clone, Copy, Debug)]
    enum Rejection {
        Capacity,
        Materialization,
        Mint,
    }

    impl Rejection {
        const ALL: [Self; 3] = [Self::Capacity, Self::Materialization, Self::Mint];

        fn domain(self) -> &'static [u8] {
            match self {
                Self::Capacity => b"outbe/nod/materialization-capacity/v2",
                Self::Materialization => b"outbe/nod/materialization-rejected/v2",
                Self::Mint => b"outbe/nod/mint-rejected/v2",
            }
        }

        /// The response of this kind for `hash`, tagged over `tag_domain`.
        fn response(
            self,
            signer: &SigningKey,
            hash: B256,
            tag_domain: &[u8],
        ) -> Result<EnclaveResponse, serde_json::Error> {
            let reason = REASON.to_string();
            let inputs_canonical_hash = hash;
            let attestation_tag = signer
                .sign(&attestation(tag_domain, hash, REASON)?)
                .to_bytes()
                .to_vec();
            Ok(match self {
                Self::Capacity => EnclaveResponse::EncryptedNodsCapacityExceededV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
                Self::Materialization => EnclaveResponse::EncryptedNodsRejectedV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
                Self::Mint => EnclaveResponse::EncryptedNodMintRejectedV2 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
            })
        }
    }

    fn accepted(operation: NodOperation, rejection: Rejection) -> bool {
        matches!(
            (operation, rejection),
            (
                NodOperation::Prepare,
                Rejection::Capacity | Rejection::Materialization
            ) | (NodOperation::Open, Rejection::Materialization)
                | (NodOperation::Mine, Rejection::Mint)
        )
    }

    const OPERATIONS: [NodOperation; 3] = [
        NodOperation::Prepare,
        NodOperation::Open,
        NodOperation::Mine,
    ];

    #[test]
    fn each_operation_accepts_only_its_own_signed_rejections() -> Result<(), serde_json::Error> {
        let signer = SigningKey::from_bytes(&[0x55; 32]);
        let key = signer.verifying_key().to_bytes();
        for operation in OPERATIONS {
            for rejection in Rejection::ALL {
                let response = rejection.response(&signer, EXPECTED, rejection.domain())?;
                let result = operation.reject_or_unexpected::<()>(&key, EXPECTED, response);
                let typed = match (rejection, &result) {
                    (
                        Rejection::Capacity,
                        Err(TransportError::NodMaterializationCapacityExceeded(reason)),
                    )
                    | (
                        Rejection::Materialization,
                        Err(TransportError::NodMaterializationRejected(reason)),
                    )
                    | (Rejection::Mint, Err(TransportError::NodMintRejected(reason))) => {
                        reason == REASON
                    }
                    _ => false,
                };
                if accepted(operation, rejection) {
                    assert!(typed, "{rejection:?}: {result:?}");
                } else {
                    assert!(
                        matches!(result, Err(TransportError::UnexpectedResponse)),
                        "{rejection:?}: {result:?}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn accepted_rejection_for_another_request_hash_is_unexpected() -> Result<(), serde_json::Error>
    {
        let signer = SigningKey::from_bytes(&[0x56; 32]);
        let key = signer.verifying_key().to_bytes();
        let other = B256::repeat_byte(0x72);
        for operation in OPERATIONS {
            for rejection in Rejection::ALL
                .into_iter()
                .filter(|r| accepted(operation, *r))
            {
                let response = rejection.response(&signer, other, rejection.domain())?;
                assert!(matches!(
                    operation.reject_or_unexpected::<()>(&key, EXPECTED, response),
                    Err(TransportError::UnexpectedResponse)
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn accepted_rejection_tagged_for_another_domain_fails_attestation(
    ) -> Result<(), serde_json::Error> {
        let signer = SigningKey::from_bytes(&[0x57; 32]);
        let key = signer.verifying_key().to_bytes();
        for operation in OPERATIONS {
            for rejection in Rejection::ALL
                .into_iter()
                .filter(|r| accepted(operation, *r))
            {
                let wrong = match rejection {
                    Rejection::Mint => Rejection::Materialization,
                    _ => Rejection::Mint,
                };
                let response = rejection.response(&signer, EXPECTED, wrong.domain())?;
                assert!(matches!(
                    operation.reject_or_unexpected::<()>(&key, EXPECTED, response),
                    Err(TransportError::TributeOfferAttestation(_))
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn hash_and_operation_are_checked_before_the_signer() -> Result<(), serde_json::Error> {
        let signer = SigningKey::from_bytes(&[0x59; 32]);
        let wrong_key = SigningKey::from_bytes(&[0x5A; 32])
            .verifying_key()
            .to_bytes();
        let other_hash = B256::repeat_byte(0x72);
        for operation in OPERATIONS {
            for rejection in Rejection::ALL {
                let response = rejection.response(&signer, other_hash, rejection.domain())?;
                assert!(matches!(
                    operation.reject_or_unexpected::<()>(&wrong_key, EXPECTED, response),
                    Err(TransportError::UnexpectedResponse)
                ));
                let response = rejection.response(&signer, EXPECTED, rejection.domain())?;
                let result = operation.reject_or_unexpected::<()>(&wrong_key, EXPECTED, response);
                if accepted(operation, rejection) {
                    assert!(matches!(
                        result,
                        Err(TransportError::TributeOfferAttestation(_))
                    ));
                } else {
                    assert!(matches!(result, Err(TransportError::UnexpectedResponse)));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn enclave_error_passes_through_and_other_responses_are_unexpected() {
        let key = SigningKey::from_bytes(&[0x58; 32])
            .verifying_key()
            .to_bytes();
        for operation in OPERATIONS {
            let error = EnclaveResponse::Error {
                message: "enclave said no".into(),
            };
            assert!(matches!(
                operation.reject_or_unexpected::<()>(&key, EXPECTED, error),
                Err(TransportError::EnclaveError(message)) if message == "enclave said no"
            ));
            assert!(matches!(
                operation.reject_or_unexpected::<()>(&key, EXPECTED, EnclaveResponse::Ack),
                Err(TransportError::UnexpectedResponse)
            ));
        }
    }
}
