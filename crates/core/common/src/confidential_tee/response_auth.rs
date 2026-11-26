//! Canonical attestation preimages, resident Ed25519 tags, and strict verification.

use alloy_primitives::B256;
use ed25519_dalek::{Signature, VerifyingKey};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::{
    tee_call::verify_enclave_attestation_lazy, AppErrorDescriptor, TeeAttestationError,
    TransportError,
};
use serde::Serialize;

use super::errors::inputs_hash_mismatch;
use super::witness_envelope::WitnessSchema;

/// Bind the echoed hash to the request before checking the response's attestation.
pub fn verify_bound_witness_attestation<T: WitnessSchema>(
    expected: B256,
    key: &[u8; 32],
    response: &T,
    verify: impl FnOnce(&[u8; 32], B256, &T, &[u8]) -> Result<(), TransportError>,
) -> Result<(), TransportError> {
    let actual = response.inputs_canonical_hash();
    if actual != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify(key, actual, response, response.attestation_tag())
}

/// Compute `keccak256(domain || serde_json::to_vec(value))`.
pub fn hash<T: Serialize + ?Sized>(domain: &[u8], value: &T) -> Result<B256, serde_json::Error> {
    outbe_tee::tee_call::canonical_domain_hash(domain, value)
}

/// Build `domain || inputs || serde_json::to_vec(value)` for an Ed25519 attestation.
pub fn attestation<T: Serialize + ?Sized>(
    domain: &[u8],
    inputs: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    outbe_tee::tee_call::canonical_attestation_preimage(domain, inputs, value)
}

/// Sign `preimage` with the resident attestation key. The signing key stays inside `context`.
#[must_use]
pub fn sign_attestation_tag(context: &InProcessTeeContext, preimage: &[u8]) -> [u8; 64] {
    context.sign_domain_attestation(preimage)
}

/// Key, descriptor, echoed hash, and tag for one response attestation check.
#[derive(Clone, Copy, Debug)]
pub struct ResponseAuthSpec<'a> {
    /// Resident Ed25519 attestation public key.
    pub attestation_pub: &'a [u8; 32],
    /// Application descriptor attached to a failed check.
    pub descriptor: AppErrorDescriptor,
    /// Inputs hash the caller computed.
    pub expected_inputs_hash: B256,
    /// Inputs hash echoed by the response.
    pub actual_inputs_hash: B256,
    /// 64-byte Ed25519 tag.
    pub tag: &'a [u8],
}

/// Strictly verify a 64-byte Ed25519 tag over an already-built preimage.
pub fn verify_response_attestation(
    spec: &ResponseAuthSpec<'_>,
    preimage: &[u8],
) -> Result<(), TransportError> {
    if spec.expected_inputs_hash != spec.actual_inputs_hash {
        return Err(TransportError::app_attestation(
            spec.descriptor,
            inputs_hash_mismatch(spec.expected_inputs_hash, spec.actual_inputs_hash),
        ));
    }
    verify_strict(spec.attestation_pub, preimage, spec.tag)
        .map_err(|cause| TransportError::app_attestation(spec.descriptor, cause))
}

/// Verify `value` under `domain` and require the echoed inputs hash to match.
pub fn verify_signed_domain<T: Serialize + ?Sized>(
    spec: &ResponseAuthSpec<'_>,
    domain: &[u8],
    value: &T,
) -> Result<(), TransportError> {
    if spec.expected_inputs_hash != spec.actual_inputs_hash {
        return Err(TransportError::app_attestation(
            spec.descriptor,
            inputs_hash_mismatch(spec.expected_inputs_hash, spec.actual_inputs_hash),
        ));
    }
    verify_app_attestation(spec.descriptor, spec.attestation_pub, spec.tag, || {
        attestation(domain, spec.expected_inputs_hash, value)
    })
}

/// Verify a domain preimage lazily, retaining its application descriptor and typed cause.
pub fn verify_app_attestation(
    descriptor: AppErrorDescriptor,
    key: &[u8; 32],
    tag: &[u8],
    preimage: impl FnOnce() -> Result<Vec<u8>, serde_json::Error>,
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(key, tag, preimage)
        .map_err(|cause| TransportError::app_attestation(descriptor, cause))
}

fn verify_strict(
    attestation_pub: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), TeeAttestationError> {
    let verifying_key =
        VerifyingKey::from_bytes(attestation_pub).map_err(TeeAttestationError::InvalidPublicKey)?;
    let signature = tag_signature(tag)?;
    verifying_key
        .verify_strict(preimage, &signature)
        .map_err(TeeAttestationError::SignatureMismatch)
}

fn tag_signature(tag: &[u8]) -> Result<Signature, TeeAttestationError> {
    let bytes: [u8; 64] = tag
        .try_into()
        .map_err(|_| TeeAttestationError::InvalidSignatureLength { actual: tag.len() })?;
    Ok(Signature::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use std::error::Error as _;

    use super::*;

    const ISSUE: &[u8] = b"outbe/nod/issue-at-offer-result/v1";
    const MINT: &[u8] = b"outbe/nod/mint-rejected/v1";

    #[test]
    fn issue_and_mint_rejection_tags_cannot_cross_domains() {
        let signer = SigningKey::from_bytes(&[0x53; 32]);
        let public = signer.verifying_key().to_bytes();
        let expected = B256::repeat_byte(0x61);
        let reason = "bad \"quote\"\nline";
        let issue = attestation(ISSUE, expected, reason).unwrap();
        let mint = attestation(MINT, expected, reason).unwrap();
        let issue_tag = signer.sign(&issue).to_bytes();
        let mint_tag = signer.sign(&mint).to_bytes();
        let descriptor = super::super::errors::NOD_MINT_REJECTED;

        let issue_spec = ResponseAuthSpec {
            attestation_pub: &public,
            descriptor,
            expected_inputs_hash: expected,
            actual_inputs_hash: expected,
            tag: &issue_tag,
        };
        let mint_spec = ResponseAuthSpec {
            attestation_pub: &public,
            descriptor,
            expected_inputs_hash: expected,
            actual_inputs_hash: expected,
            tag: &mint_tag,
        };
        verify_signed_domain(&issue_spec, ISSUE, reason).unwrap();
        verify_signed_domain(&mint_spec, MINT, reason).unwrap();
        for (spec, domain) in [(&issue_spec, MINT), (&mint_spec, ISSUE)] {
            let crossed = verify_signed_domain(spec, domain, reason).unwrap_err();
            assert!(matches!(
                crossed
                    .source()
                    .and_then(|cause| cause.downcast_ref::<TeeAttestationError>()),
                Some(TeeAttestationError::SignatureMismatch(_))
            ));
        }
    }

    #[test]
    fn rejection_tag_rejects_a_different_expected_input_hash() {
        let signer = SigningKey::from_bytes(&[0x54; 32]);
        let public = signer.verifying_key().to_bytes();
        let reason = "fixed rejection";
        let signed_hash = B256::repeat_byte(0x62);
        let different_hash = B256::repeat_byte(0x63);
        let descriptor = super::super::errors::NOD_MINT_REJECTED;
        for domain in [ISSUE, MINT] {
            let signed = attestation(domain, signed_hash, reason).unwrap();
            let tag = signer.sign(&signed).to_bytes();
            let spec = ResponseAuthSpec {
                attestation_pub: &public,
                descriptor,
                expected_inputs_hash: signed_hash,
                actual_inputs_hash: signed_hash,
                tag: &tag,
            };
            verify_signed_domain(&spec, domain, reason).unwrap();
            let mismatched = verify_signed_domain(
                &ResponseAuthSpec {
                    expected_inputs_hash: different_hash,
                    ..spec
                },
                domain,
                reason,
            )
            .unwrap_err();
            assert!(matches!(
                mismatched
                    .source()
                    .and_then(|cause| cause.downcast_ref::<TeeAttestationError>()),
                Some(TeeAttestationError::InputsHashMismatch { .. })
            ));
            let moved = verify_signed_domain(
                &ResponseAuthSpec {
                    expected_inputs_hash: different_hash,
                    actual_inputs_hash: different_hash,
                    ..spec
                },
                domain,
                reason,
            )
            .unwrap_err();
            assert!(matches!(
                moved
                    .source()
                    .and_then(|cause| cause.downcast_ref::<TeeAttestationError>()),
                Some(TeeAttestationError::SignatureMismatch(_))
            ));
        }
    }

    #[test]
    fn response_attestation_rejects_a_short_tag_without_dropping_the_cause() {
        let signer = SigningKey::from_bytes(&[0x54; 32]);
        let public = signer.verifying_key().to_bytes();
        let hash = B256::repeat_byte(0x62);
        let error = verify_response_attestation(
            &ResponseAuthSpec {
                attestation_pub: &public,
                descriptor: super::super::errors::GRATIS_OP_ATTESTATION,
                expected_inputs_hash: hash,
                actual_inputs_hash: hash,
                tag: &[0; 63],
            },
            b"preimage",
        )
        .unwrap_err();
        assert!(matches!(
            error
                .source()
                .and_then(|cause| cause.downcast_ref::<TeeAttestationError>()),
            Some(TeeAttestationError::InvalidSignatureLength { actual: 63 })
        ));
    }
}
