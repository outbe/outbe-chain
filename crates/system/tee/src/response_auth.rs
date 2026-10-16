//! Canonical hashes and authenticated enclave responses.
use crate::{protocol::EnclaveResponse, TransportError};
use alloy_primitives::{keccak256, B256};
use serde::Serialize;
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

const MINT_REJECTED: NodRejectionKind = NodRejectionKind::new(
    b"outbe/nod/mint-rejected/v2",
    TransportError::NodMintRejected,
);

/// A NOD operation that the enclave answers with a signed result or with a
/// signed rejection. Each operation accepts only its own rejection responses.
#[derive(Clone, Copy)]
pub(crate) enum NodOperation {
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

pub(crate) fn codec(error: serde_json::Error) -> TransportError {
    TransportError::codec(error)
}

#[cfg(test)]
mod rejection_tests {
    use super::{attestation, verify_rejection, RejectionSubject};
    use crate::TransportError;
    use alloy_primitives::B256;
    use ed25519_dalek::{Signer, SigningKey};

    const LYSIS: &[u8] = b"outbe/lysis/rejected";
    const MINT: &[u8] = b"outbe/nod/mint-rejected/v2";

    #[test]
    fn lysis_and_mint_rejection_tags_cannot_cross_domains() {
        let signer = SigningKey::from_bytes(&[0x53; 32]);
        let public = signer.verifying_key().to_bytes();
        let expected = B256::repeat_byte(0x61);
        let reason = "bad \"quote\"\nline";
        let lysis = attestation(LYSIS, expected, reason).unwrap();
        let mint = attestation(MINT, expected, reason).unwrap();
        let lysis_tag = signer.sign(&lysis).to_bytes();
        let mint_tag = signer.sign(&mint).to_bytes();

        let mut expected_lysis = LYSIS.to_vec();
        expected_lysis.extend_from_slice(expected.as_slice());
        expected_lysis.extend_from_slice(br#""bad \"quote\"\nline""#);
        assert_eq!(lysis, expected_lysis);
        assert_eq!(
            attestation(LYSIS, expected, &reason.to_string()).unwrap(),
            expected_lysis
        );

        assert!(verify_rejection(
            &public,
            RejectionSubject::new(LYSIS, expected, reason),
            &lysis_tag
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
                &lysis_tag
            ),
            Err(TransportError::TributeOfferAttestation(_))
        ));
        assert!(matches!(
            verify_rejection(
                &public,
                RejectionSubject::new(LYSIS, expected, reason),
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

        for domain in [LYSIS, MINT] {
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

    #[test]
    fn mint_rejection_requires_the_exact_request_and_domain() {
        let signer = SigningKey::from_bytes(&[0x55; 32]);
        let key = signer.verifying_key().to_bytes();
        let expected = B256::repeat_byte(0x71);
        let other = B256::repeat_byte(0x72);
        for (hash, domain, wrong_hash, wrong_domain) in [
            (
                expected,
                b"outbe/nod/mint-rejected/v2".as_slice(),
                false,
                false,
            ),
            (other, b"outbe/nod/mint-rejected/v2".as_slice(), true, false),
            (expected, b"outbe/lysis/rejected".as_slice(), false, true),
        ] {
            let reason = "mint rejected".to_string();
            let tag = signer
                .sign(&attestation(domain, hash, &reason).unwrap())
                .to_bytes();
            let result = NodOperation::Mine.reject_or_unexpected::<()>(
                &key,
                expected,
                EnclaveResponse::EncryptedNodMintRejectedV2 {
                    reason,
                    inputs_canonical_hash: hash,
                    attestation_tag: tag.to_vec(),
                },
            );
            if wrong_hash {
                assert!(matches!(result, Err(TransportError::UnexpectedResponse)));
            } else if wrong_domain {
                assert!(matches!(
                    result,
                    Err(TransportError::TributeOfferAttestation(_))
                ));
            } else {
                assert!(matches!(result, Err(TransportError::NodMintRejected(_))));
            }
        }
        assert!(matches!(
            NodOperation::Mine.reject_or_unexpected::<()>(&key, expected, EnclaveResponse::Ack,),
            Err(TransportError::UnexpectedResponse)
        ));
        assert!(matches!(NodOperation::Mine.reject_or_unexpected::<()>(
            &key, expected, EnclaveResponse::Error { message: "local failure".into() },
        ), Err(TransportError::EnclaveError(message)) if message == "local failure"));
    }
}
