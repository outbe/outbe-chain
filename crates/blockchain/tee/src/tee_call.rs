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

/// Execute a witness-recorded consensus enclave call, failing closed if `!meta.recordable`.
pub fn tee_consensus_call(
    meta: CallMeta,
    request: &crate::protocol::EnclaveRequest,
    missing_sidecar_err: impl FnOnce() -> crate::TransportError,
    authenticate: impl Fn(
        &[u8; 32],
        &crate::protocol::EnclaveResponse,
    ) -> Result<(), crate::TransportError>,
) -> Result<([u8; 32], crate::protocol::EnclaveResponse), crate::TransportError> {
    if !meta.recordable {
        return Err(crate::TransportError::Codec(
            "non-recordable call routed to tee_consensus_call".into(),
        ));
    }
    consensus_call(
        meta.label,
        || {
            let (session_key, wire_result) = crate::try_with_enclave(|session| {
                (
                    session.attestation_pub(),
                    session.request_with_meta(meta, request),
                )
            })
            .ok_or_else(missing_sidecar_err)?;
            Ok((session_key, wire_result?))
        },
        authenticate,
    )
}

/// Execute a non-consensus session operation on `session` and authenticate the response.
///
/// Fails closed if `meta.recordable` is `true` so consensus-recorded calls never
/// bypass the block witness tape.
pub fn tee_session_call_on(
    session: &mut crate::EnclaveSession,
    meta: CallMeta,
    request: &crate::protocol::EnclaveRequest,
    authenticate: impl FnOnce(
        &[u8; 32],
        &crate::protocol::EnclaveResponse,
    ) -> Result<(), crate::TransportError>,
) -> Result<([u8; 32], crate::protocol::EnclaveResponse), crate::TransportError> {
    if meta.recordable {
        return Err(crate::TransportError::Codec(
            "recordable call routed to non-consensus tee_session_call".into(),
        ));
    }
    let key = session.attestation_pub();
    let response = session.request_with_meta(meta, request)?;
    authenticate(&key, &response)?;
    Ok((key, response))
}

/// Execute a non-consensus session operation against the process-global execution session
/// and authenticate the response before returning it.
///
/// Returns `None` only when no enclave session is installed.
pub fn tee_session_call(
    meta: CallMeta,
    request: &crate::protocol::EnclaveRequest,
    authenticate: impl FnOnce(
        &[u8; 32],
        &crate::protocol::EnclaveResponse,
    ) -> Result<(), crate::TransportError>,
) -> Option<Result<([u8; 32], crate::protocol::EnclaveResponse), crate::TransportError>> {
    if meta.recordable {
        return Some(Err(crate::TransportError::Codec(
            "recordable call routed to non-consensus tee_session_call".into(),
        )));
    }
    crate::try_with_enclave(|session| tee_session_call_on(session, meta, request, authenticate))
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

/// Borrowed parameters for verifying an authenticated enclave rejection via [`verify_signed_rejection`].
#[derive(Clone, Copy, Debug)]
pub struct SignedRejectionRef<'a> {
    /// Domain separation tag prefix of the signed rejection preimage.
    pub domain: &'a [u8],
    /// Canonical commitment hash computed locally from the request.
    pub expected_inputs_hash: B256,
    /// Canonical commitment hash echoed by the enclave rejection response.
    pub actual_inputs_hash: B256,
    /// Deterministic rejection reason string signed by the enclave.
    pub reason: &'a str,
    /// 64-byte Ed25519 signature tag attached to the rejection response.
    pub tag: &'a [u8],
}

/// Verify an authenticated rejection response bound to `rejection.expected_inputs_hash` and `rejection.domain`.
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
    verify_enclave_attestation_lazy(attestation_pub, rejection.tag, || {
        canonical_attestation_preimage(
            rejection.domain,
            rejection.expected_inputs_hash,
            rejection.reason,
        )
    })
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
                tag: &tag,
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
                    tag: &tag,
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
                    tag: &tag,
                },
            ),
            Err(TeeAttestationError::InputsHashMismatch { .. })
        ));

        assert!(matches!(
            verify_enclave_attestation(&pubkey, &preimage, &tag[..63]),
            Err(TeeAttestationError::InvalidSignatureLength { actual: 63 })
        ));
    }

    #[test]
    fn tee_consensus_call_and_session_call_fail_closed_on_mismatched_recordable_flag() {
        let req = crate::protocol::EnclaveRequest::Health;
        let err = tee_consensus_call(
            CallMeta::session_only("health"),
            &req,
            || crate::TransportError::Unavailable("none".into()),
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(err, crate::TransportError::Codec(_)));

        let session_err = tee_session_call(CallMeta::recorded("recorded_op"), &req, |_, _| Ok(()))
            .expect("returns fail-closed error even without installed session")
            .unwrap_err();
        assert!(matches!(session_err, crate::TransportError::Codec(_)));
    }
}
