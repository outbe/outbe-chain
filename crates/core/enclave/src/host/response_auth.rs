//! Canonical hashes and authenticated enclave responses.

use alloy_primitives::B256;
use outbe_tee::{
    tee_call::{
        canonical_attestation_preimage, canonical_domain_hash, verify_signed_rejection,
        SignedRejectionRef,
    },
    TeeAttestationError, TransportError,
};
use serde::Serialize;

use super::{errors::EnclaveClientError, protocol::EnclaveResponse};

/// Compute `keccak256(domain || serde_json::to_vec(value))`.
pub fn hash<T: Serialize + ?Sized>(domain: &[u8], value: &T) -> Result<B256, serde_json::Error> {
    canonical_domain_hash(domain, value)
}

/// Build `domain || inputs || serde_json::to_vec(value)` for Ed25519 attestation signing.
pub fn attestation<T: Serialize + ?Sized>(
    domain: &[u8],
    inputs: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    canonical_attestation_preimage(domain, inputs, value)
}

/// The reason and the enclave tag of one rejection response.
struct SignedRejection {
    actual_inputs_hash: B256,
    reason: String,
    attestation_tag: Vec<u8>,
}

impl SignedRejection {
    fn new(actual_inputs_hash: B256, reason: String, attestation_tag: Vec<u8>) -> Self {
        Self {
            actual_inputs_hash,
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
    error: fn(String) -> EnclaveClientError,
}

impl NodRejectionKind {
    const fn new(domain: &'static [u8], error: fn(String) -> EnclaveClientError) -> Self {
        Self { domain, error }
    }

    /// Verifies the enclave tag of `rejection` for the request hash `expected`.
    /// Then returns the typed error of this kind with the rejection reason.
    fn reject<T>(
        self,
        key: &[u8; 32],
        expected: B256,
        rejection: SignedRejection,
    ) -> Result<T, EnclaveClientError> {
        verify_signed_rejection(
            key,
            SignedRejectionRef {
                domain: self.domain,
                expected_inputs_hash: expected,
                actual_inputs_hash: rejection.actual_inputs_hash,
                reason: &rejection.reason,
                tag: &rejection.attestation_tag,
            },
        )
        .map_err(|cause| match cause {
            TeeAttestationError::InputsHashMismatch { .. } => {
                EnclaveClientError::Transport(TransportError::UnexpectedResponse)
            }
            other => EnclaveClientError::NodAttestation(other),
        })?;
        Err((self.error)(rejection.reason))
    }
}

fn nod_mint_rejected(reason: String) -> EnclaveClientError {
    EnclaveClientError::NodMintRejected { reason }
}

const MINT_REJECTED: NodRejectionKind =
    NodRejectionKind::new(b"outbe/nod/mint-rejected/v1", nod_mint_rejected);

/// A NOD operation that the enclave answers with a signed result or with a
/// signed rejection. Each operation accepts only its own rejection responses.
#[derive(Clone, Copy)]
pub(crate) enum NodOperation {
    Mine,
}

impl NodOperation {
    /// Ends the response match of this operation after its success arm. A
    /// rejection that this operation accepts and that answers `expected` is
    /// verified and returned as its typed error. Every other response is unexpected.
    pub(crate) fn reject_or_unexpected<T>(
        self,
        key: &[u8; 32],
        expected: B256,
        response: EnclaveResponse,
    ) -> Result<T, EnclaveClientError> {
        let (kind, rejection) = match (self, response) {
            (
                Self::Mine,
                EnclaveResponse::EncryptedNodMintRejectedV1 {
                    reason,
                    inputs_canonical_hash,
                    attestation_tag,
                },
            ) => (
                MINT_REJECTED,
                SignedRejection::new(inputs_canonical_hash, reason, attestation_tag),
            ),
            _ => return Err(TransportError::UnexpectedResponse.into()),
        };
        kind.reject(key, expected, rejection)
    }
}

pub(crate) fn codec(error: serde_json::Error) -> TransportError {
    TransportError::codec(error)
}

#[cfg(test)]
mod rejection_tests {
    use alloy_primitives::B256;
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_tee::{
        tee_call::{verify_signed_rejection, SignedRejectionRef},
        TeeAttestationError,
    };

    use super::attestation;

    const LYSIS: &[u8] = b"outbe/lysis/rejected";
    const MINT: &[u8] = b"outbe/nod/mint-rejected/v1";

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

        let base = SignedRejectionRef {
            domain: LYSIS,
            expected_inputs_hash: expected,
            actual_inputs_hash: expected,
            reason,
            tag: &lysis_tag,
        };
        assert!(verify_signed_rejection(&public, base).is_ok());
        assert!(verify_signed_rejection(
            &public,
            SignedRejectionRef {
                domain: MINT,
                tag: &mint_tag,
                ..base
            }
        )
        .is_ok());
        assert!(matches!(
            verify_signed_rejection(
                &public,
                SignedRejectionRef {
                    domain: MINT,
                    ..base
                }
            ),
            Err(TeeAttestationError::SignatureMismatch(_))
        ));
        assert!(matches!(
            verify_signed_rejection(
                &public,
                SignedRejectionRef {
                    tag: &mint_tag,
                    ..base
                }
            ),
            Err(TeeAttestationError::SignatureMismatch(_))
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
            let valid = SignedRejectionRef {
                domain,
                expected_inputs_hash: signed_hash,
                actual_inputs_hash: signed_hash,
                reason,
                tag: &tag,
            };

            assert!(verify_signed_rejection(&public, valid).is_ok());
            assert!(matches!(
                verify_signed_rejection(
                    &public,
                    SignedRejectionRef {
                        expected_inputs_hash: different_hash,
                        ..valid
                    }
                ),
                Err(TeeAttestationError::InputsHashMismatch { .. })
            ));
            assert!(matches!(
                verify_signed_rejection(
                    &public,
                    SignedRejectionRef {
                        expected_inputs_hash: different_hash,
                        actual_inputs_hash: different_hash,
                        ..valid
                    }
                ),
                Err(TeeAttestationError::SignatureMismatch(_))
            ));
        }
    }
}

#[cfg(test)]
mod response_tests {
    use alloy_primitives::B256;
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_tee::TransportError;

    use super::{attestation, NodOperation};
    use crate::host::{errors::EnclaveClientError, protocol::EnclaveResponse};

    #[test]
    fn mint_rejection_requires_the_exact_request_and_domain() {
        let signer = SigningKey::from_bytes(&[0x55; 32]);
        let key = signer.verifying_key().to_bytes();
        let expected = B256::repeat_byte(0x71);
        let other = B256::repeat_byte(0x72);
        for (hash, domain, wrong_hash, wrong_domain) in [
            (
                expected,
                b"outbe/nod/mint-rejected/v1".as_slice(),
                false,
                false,
            ),
            (other, b"outbe/nod/mint-rejected/v1".as_slice(), true, false),
            (expected, b"outbe/lysis/rejected".as_slice(), false, true),
        ] {
            let reason = "mint rejected".to_string();
            let tag = signer
                .sign(&attestation(domain, hash, &reason).unwrap())
                .to_bytes();
            let result = NodOperation::Mine.reject_or_unexpected::<()>(
                &key,
                expected,
                EnclaveResponse::EncryptedNodMintRejectedV1 {
                    reason,
                    inputs_canonical_hash: hash,
                    attestation_tag: tag.to_vec(),
                },
            );
            if wrong_hash {
                assert!(matches!(
                    result,
                    Err(EnclaveClientError::Transport(
                        TransportError::UnexpectedResponse
                    ))
                ));
            } else if wrong_domain {
                assert!(matches!(result, Err(EnclaveClientError::NodAttestation(_))));
            } else {
                assert!(matches!(
                    result,
                    Err(EnclaveClientError::NodMintRejected { .. })
                ));
            }
        }
        assert!(matches!(
            NodOperation::Mine.reject_or_unexpected::<()>(
                &key,
                expected,
                EnclaveResponse::NodTransferDiscardedV1 { id: B256::ZERO },
            ),
            Err(EnclaveClientError::Transport(
                TransportError::UnexpectedResponse
            ))
        ));
    }
}
