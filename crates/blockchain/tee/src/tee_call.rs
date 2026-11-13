//! Domain-neutral enclave call metadata, canonical preimage builders, and
//! Ed25519 attestation verification primitives.
//!
//! Higher-layer crates (`outbe-enclave`) construct domain request hashes and
//! response preimages over these primitives while `outbe-tee` owns transport,
//! witness recording/replay, and Ed25519 verification.

use alloy_primitives::{keccak256, B256};
use serde::Serialize;
use thiserror::Error;

pub use crate::witness::consensus_call;

/// Metadata describing an enclave operation's telemetry label, retry policy,
/// and block-witness recordability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallMeta {
    /// Stable telemetry label (`op` label in metrics and witness traces).
    pub label: &'static str,
    /// Whether transport may retry once on a broken socket.
    pub idempotent: bool,
    /// Whether consensus execution records this call on the block witness tape.
    pub recordable: bool,
}

impl CallMeta {
    /// Metadata for a deterministic, witness-recorded consensus operation.
    #[must_use]
    pub const fn recorded(label: &'static str) -> Self {
        Self {
            label,
            idempotent: true,
            recordable: true,
        }
    }

    /// Metadata for an idempotent non-consensus session operation.
    #[must_use]
    pub const fn session_only(label: &'static str) -> Self {
        Self {
            label,
            idempotent: true,
            recordable: false,
        }
    }
}

/// Typed attestation and canonical-binding verification errors.
#[derive(Debug, Error)]
pub enum TeeAttestationError {
    /// Peer Ed25519 attestation public key is not a valid curve point.
    #[error("bad attestation key: {0}")]
    InvalidPublicKey(#[source] ed25519_dalek::SignatureError),

    /// Signature slice does not have the 64-byte Ed25519 length.
    #[error("bad tag length {actual}")]
    InvalidSignatureLength {
        /// Actual byte length of the tag slice.
        actual: usize,
    },

    /// Ed25519 strict verification rejected the signature over the preimage.
    #[error("signature invalid: {0}")]
    SignatureMismatch(#[source] ed25519_dalek::SignatureError),

    /// Canonical JSON preimage serialization failed.
    #[error("attestation preimage serialization failed: {0}")]
    CanonicalJson(#[from] serde_json::Error),

    /// Response `inputs_canonical_hash` did not match the request commitment.
    #[error("inputs canonical hash mismatch: expected {expected}, got {actual}")]
    InputsHashMismatch {
        /// Expected request canonical hash.
        expected: B256,
        /// Actual hash echoed by the enclave response.
        actual: B256,
    },

    /// Inner payload bytes failed canonical decoding or round-trip check.
    #[error("non-canonical enclave payload")]
    NonCanonicalPayload,

    /// Enclave returned a response variant that does not match the request.
    #[error("unexpected enclave response variant")]
    UnexpectedResponseVariant,
}

/// Borrowed inputs for [`verify_signed_rejection`].
#[derive(Clone, Copy, Debug)]
pub struct SignedRejectionRef<'a> {
    /// Domain-separation prefix for this rejection kind.
    pub domain: &'a [u8],
    /// Expected canonical hash of the request inputs.
    pub expected_inputs_hash: B256,
    /// Actual canonical hash echoed in the rejection response.
    pub actual_inputs_hash: B256,
    /// Human-readable rejection reason signed by the enclave.
    pub reason: &'a str,
    /// 64-byte Ed25519 signature over `(domain || expected_inputs_hash || json(reason))`.
    pub attestation_tag: &'a [u8],
}

/// Compute `keccak256(domain || serde_json::to_vec(value))`.
pub fn canonical_domain_hash<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<B256, serde_json::Error> {
    let json = serde_json::to_vec(value)?;
    let mut bytes = Vec::with_capacity(domain.len() + json.len());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&json);
    Ok(keccak256(bytes))
}

/// Build the canonical attestation preimage `domain || inputs_hash || serde_json::to_vec(value)`.
pub fn canonical_attestation_preimage<T: Serialize + ?Sized>(
    domain: &[u8],
    inputs_hash: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(value)?;
    let mut bytes = Vec::with_capacity(domain.len() + 32 + json.len());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&json);
    Ok(bytes)
}

/// Strictly verify a 64-byte Ed25519 `tag` over `preimage` under `attestation_pub`.
pub fn verify_enclave_attestation(
    attestation_pub: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), TeeAttestationError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, || Ok(preimage.to_vec()))
}

/// Strictly verify a 64-byte Ed25519 `tag` over a lazily serialized preimage.
///
/// Public-key and tag-length checks run before invoking `build_preimage`, matching
/// the established host-client verification order.
pub fn verify_enclave_attestation_lazy(
    attestation_pub: &[u8; 32],
    tag: &[u8],
    build_preimage: impl FnOnce() -> Result<Vec<u8>, serde_json::Error>,
) -> Result<(), TeeAttestationError> {
    use ed25519_dalek::{Signature, VerifyingKey};

    let verifying_key =
        VerifyingKey::from_bytes(attestation_pub).map_err(TeeAttestationError::InvalidPublicKey)?;
    let sig_bytes: [u8; 64] = tag
        .try_into()
        .map_err(|_| TeeAttestationError::InvalidSignatureLength { actual: tag.len() })?;
    let signature = Signature::from_bytes(&sig_bytes);
    let preimage = build_preimage()?;
    verifying_key
        .verify_strict(&preimage, &signature)
        .map_err(TeeAttestationError::SignatureMismatch)
}

/// Verify an authenticated rejection response bound to `expected_inputs_hash` and `domain`.
pub fn verify_signed_rejection(
    attestation_pub: &[u8; 32],
    rejection: SignedRejectionRef<'_>,
) -> Result<(), TeeAttestationError> {
    if rejection.actual_inputs_hash != rejection.expected_inputs_hash {
        return Err(TeeAttestationError::InputsHashMismatch {
            expected: rejection.expected_inputs_hash,
            actual: rejection.actual_inputs_hash,
        });
    }
    let preimage = canonical_attestation_preimage(
        rejection.domain,
        rejection.expected_inputs_hash,
        rejection.reason,
    )?;
    verify_enclave_attestation(attestation_pub, &preimage, rejection.attestation_tag)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    #[test]
    fn signed_rejection_and_lazy_attestation_enforce_domain_hash_and_tag_len() {
        let signer = SigningKey::from_bytes(&[0x5a; 32]);
        let pubkey = signer.verifying_key().to_bytes();
        let hash = canonical_domain_hash(b"outbe/test/input/v1", &"payload").unwrap();
        let reason = "rejected-for-test";
        let preimage =
            canonical_attestation_preimage(b"outbe/test/reject/v1", hash, reason).unwrap();
        let tag = signer.sign(&preimage).to_bytes();

        verify_signed_rejection(
            &pubkey,
            SignedRejectionRef {
                domain: b"outbe/test/reject/v1",
                expected_inputs_hash: hash,
                actual_inputs_hash: hash,
                reason,
                attestation_tag: &tag,
            },
        )
        .expect("matching domain and hash verify");

        assert!(matches!(
            verify_signed_rejection(
                &pubkey,
                SignedRejectionRef {
                    domain: b"outbe/test/other/v1",
                    expected_inputs_hash: hash,
                    actual_inputs_hash: hash,
                    reason,
                    attestation_tag: &tag,
                },
            ),
            Err(TeeAttestationError::SignatureMismatch(_))
        ));

        let other_hash = B256::repeat_byte(0x99);
        assert!(matches!(
            verify_signed_rejection(
                &pubkey,
                SignedRejectionRef {
                    domain: b"outbe/test/reject/v1",
                    expected_inputs_hash: hash,
                    actual_inputs_hash: other_hash,
                    reason,
                    attestation_tag: &tag,
                },
            ),
            Err(TeeAttestationError::InputsHashMismatch { .. })
        ));

        assert!(matches!(
            verify_enclave_attestation(&pubkey, &preimage, &tag[..63]),
            Err(TeeAttestationError::InvalidSignatureLength { actual: 63 })
        ));
    }
}
