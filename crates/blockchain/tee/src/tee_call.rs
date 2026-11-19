//! Domain-neutral enclave call metadata, canonical preimage builders, and
//! Ed25519 attestation verification primitives.
//!
//! Domain crates construct request hashes and
//! response preimages over these primitives while `outbe-tee` owns dispatch,
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
    /// Whether the operation is safe to execute again.
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
    missing_context_err: impl FnOnce() -> crate::TransportError,
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
            crate::in_process_dispatch::request_with_attestation(meta, request)
                .ok_or_else(missing_context_err)?
        },
        authenticate,
    )
}

/// Execute a witness-recorded typed consensus call, failing closed if `!meta.recordable`.
///
/// Live execution invokes `execute` on the resident context with no request
/// serialization. `encode` runs only when the witness tape is recording or a
/// verifier is comparing a live answer. `VerifierReplay` returns
/// `decode_and_auth` of the recorded [`crate::witness::EnclaveWitnessEntryV1::Payload`]
/// fields and does not call `execute`.
pub fn tee_consensus_typed_call<T>(
    meta: CallMeta,
    execute: impl FnOnce(&crate::in_process::InProcessTeeContext) -> Result<T, crate::TransportError>,
    encode: impl FnOnce(&T) -> Result<crate::witness::TypedWitnessPayload, crate::TransportError>,
    decode_and_auth: impl Fn(
        &[u8; 32],
        crate::witness::TypedWitnessPayloadRef<'_>,
    ) -> Result<T, crate::TransportError>,
) -> Result<T, crate::TransportError> {
    if !meta.recordable {
        return Err(crate::TransportError::codec(
            crate::errors::TypedCallRouteError::NonRecordableConsensus,
        ));
    }
    crate::witness::consensus_typed_call(
        || {
            let context = crate::in_process_dispatch::in_process_tee_context()
                .ok_or(crate::InProcessDispatchError::ContextNotInstalled)?;
            let started = std::time::Instant::now();
            let value = execute(&context);
            crate::metrics::record_request_duration(meta.label, started.elapsed());
            if let Err(error) = &value {
                crate::metrics::record_request_error(meta.label, error.metric_class());
            }
            Ok((context.attestation_public(), value?))
        },
        encode,
        decode_and_auth,
    )
}

/// Execute a non-consensus typed closure in memory.
///
/// Returns `None` only when no resident runtime is installed. A recordable
/// call fails closed. Argument and result bytes are not serialized.
pub fn tee_session_typed_call<T>(
    meta: CallMeta,
    execute: impl FnOnce(&crate::in_process::InProcessTeeContext) -> Result<T, crate::TransportError>,
) -> Option<Result<T, crate::TransportError>> {
    if meta.recordable {
        return Some(Err(crate::TransportError::codec(
            crate::errors::TypedCallRouteError::RecordableSession,
        )));
    }
    crate::in_process_dispatch::try_session_typed(meta, execute)
}

/// Execute a non-consensus operation against the resident runtime
/// and authenticate the response before returning it.
///
/// Returns `None` only when no resident runtime is installed.
/// The request and its attestation key come from one runtime snapshot.
/// Lane occupancy, duration, and the error counter are recorded once.
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
    let dispatched = crate::in_process_dispatch::dispatch_session_snapshot(meta, request)?;
    Some(dispatched.and_then(|(key, response)| {
        authenticate(&key, &response)?;
        Ok((key, response))
    }))
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
#[path = "tee_call_tests.rs"]
mod tests;

#[cfg(test)]
mod witness_parts {
    use super::*;

    fn binding() -> outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
        use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
        NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(676).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        }
    }

    fn assert_witness_unavailable(
        error: &crate::TransportError,
        expected: crate::errors::WitnessAvailabilityError,
    ) {
        assert!(error.is_node_local());
        assert_eq!(error.metric_class(), "unavailable");
        assert!(matches!(
            std::error::Error::source(error)
                .and_then(|cause| cause.downcast_ref::<crate::errors::WitnessAvailabilityError>()),
            Some(cause) if std::mem::discriminant(cause) == std::mem::discriminant(&expected)
        ));
    }

    fn signed_parts(payload: Vec<u8>, inputs: B256) -> crate::witness::TypedWitnessPayload {
        let context = crate::in_process_dispatch::in_process_tee_context().expect("context");
        let mut preimage = payload.clone();
        preimage.extend_from_slice(inputs.as_slice());
        crate::witness::TypedWitnessPayload {
            payload,
            inputs_canonical_hash: inputs,
            attestation_tag: context.sign_domain_attestation(&preimage).to_vec(),
        }
    }

    fn verify_signed_u64(
        key: &[u8; 32],
        parts: crate::witness::TypedWitnessPayloadRef<'_>,
    ) -> Result<u64, crate::TransportError> {
        let mut preimage = Vec::with_capacity(parts.payload.len() + 32);
        preimage.extend_from_slice(parts.payload);
        preimage.extend_from_slice(parts.inputs_canonical_hash.as_slice());
        verify_enclave_attestation(key, &preimage, parts.attestation_tag)
            .map_err(crate::TransportError::attestation)?;
        let bytes: [u8; 8] = parts
            .payload
            .try_into()
            .map_err(|_| crate::TransportError::UnexpectedResponse)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn leader_signed_entry(
        leader: &crate::in_process::InProcessTeeContext,
        payload: Vec<u8>,
        inputs: B256,
    ) -> crate::witness::EnclaveWitnessEntryV1 {
        let mut preimage = payload.clone();
        preimage.extend_from_slice(inputs.as_slice());
        crate::witness::EnclaveWitnessEntryV1::Payload {
            payload,
            inputs_canonical_hash: inputs,
            attestation_tag: leader.sign_domain_attestation(&preimage).to_vec(),
        }
    }

    fn verifier_scope(
        mode: crate::witness::WitnessExecutionMode,
        leader_key: [u8; 32],
        entry: crate::witness::EnclaveWitnessEntryV1,
    ) -> (
        crate::in_process_dispatch::InProcessTestGuard,
        std::sync::Arc<std::sync::Mutex<crate::witness::WitnessState>>,
        crate::witness::WitnessExecutionScope,
    ) {
        let verifier = std::sync::Arc::new(crate::test_utils::resident_context(9, binding()));
        assert_ne!(leader_key, verifier.attestation_public());
        let guard = crate::in_process_dispatch::set_in_process_tee_context_for_test(Some(verifier));
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::witness::WitnessState::new(
            mode,
            Some(leader_key),
            crate::witness::EnclaveWitnessTapeV1 {
                prefix: vec![entry],
                txs: Vec::new(),
            },
        )));
        let scope = crate::witness::WitnessExecutionScope::enter(state.clone());
        (guard, state, scope)
    }

    #[test]
    fn verifier_with_distinct_ed25519_keys_accepts_matching_payload_and_hash() {
        use crate::witness::EnclaveWitnessEntryV1;
        let leader = std::sync::Arc::new(crate::test_utils::resident_context(4, binding()));
        let inputs = B256::repeat_byte(0x42);
        let entry = leader_signed_entry(&leader, 11u64.to_be_bytes().to_vec(), inputs);
        let leader_tag = match &entry {
            EnclaveWitnessEntryV1::Payload {
                payload,
                inputs_canonical_hash,
                attestation_tag,
            } => {
                assert_eq!(payload.as_slice(), 11u64.to_be_bytes());
                assert_eq!(*inputs_canonical_hash, inputs);
                assert_ne!(*inputs_canonical_hash, B256::ZERO);
                attestation_tag.clone()
            }
            _ => panic!("leader entry is not Payload"),
        };
        let (_guard, state, _scope) = verifier_scope(
            crate::witness::WitnessExecutionMode::VerifierWithEnclave,
            leader.attestation_public(),
            entry,
        );
        let value = tee_consensus_typed_call(
            CallMeta::recorded("distinct_keys"),
            |_| Ok(11u64),
            |value| {
                let parts = signed_parts(value.to_be_bytes().to_vec(), inputs);
                assert_ne!(parts.attestation_tag, leader_tag);
                assert_eq!(parts.payload, 11u64.to_be_bytes());
                assert_eq!(parts.inputs_canonical_hash, inputs);
                Ok(parts)
            },
            verify_signed_u64,
        )
        .unwrap();
        assert_eq!(value, 11);
        assert_eq!(state.lock().unwrap().prefix_cursor, 1);
        assert_eq!(state.lock().unwrap().first_witness_fault, None);
    }

    #[test]
    fn verifier_with_enclave_rejects_divergent_hash_or_payload() {
        let leader = std::sync::Arc::new(crate::test_utils::resident_context(4, binding()));
        let honest = B256::repeat_byte(0x42);
        for (live_value, live_inputs) in [(11u64, B256::repeat_byte(0x43)), (12u64, honest)] {
            let entry = leader_signed_entry(&leader, 11u64.to_be_bytes().to_vec(), honest);
            let (_guard, state, _scope) = verifier_scope(
                crate::witness::WitnessExecutionMode::VerifierWithEnclave,
                leader.attestation_public(),
                entry,
            );
            let error = tee_consensus_typed_call(
                CallMeta::recorded("divergent_parts"),
                move |_| Ok(live_value),
                move |value| Ok(signed_parts(value.to_be_bytes().to_vec(), live_inputs)),
                verify_signed_u64,
            )
            .unwrap_err();
            assert_witness_unavailable(
                &error,
                crate::errors::WitnessAvailabilityError::SemanticMismatch,
            );
            let witness = state.lock().unwrap();
            assert_eq!(witness.prefix_cursor, 0);
            assert_eq!(
                witness.first_witness_fault,
                Some(crate::witness::BlockWitnessFault::SemanticMismatch)
            );
        }
    }

    #[test]
    fn verifier_replay_rejects_corrupt_attestation_tag_or_inputs_hash() {
        use crate::witness::{BlockWitnessFault, EnclaveWitnessEntryV1, WitnessExecutionMode};
        let leader = std::sync::Arc::new(crate::test_utils::resident_context(4, binding()));
        let inputs = B256::repeat_byte(0x42);
        let entry = leader_signed_entry(&leader, 11u64.to_be_bytes().to_vec(), inputs);
        for corrupt_tag in [true, false] {
            let mut bad = entry.clone();
            match &mut bad {
                EnclaveWitnessEntryV1::Payload {
                    attestation_tag,
                    inputs_canonical_hash,
                    ..
                } => {
                    if corrupt_tag {
                        attestation_tag[0] ^= 0x01;
                    } else {
                        let mut bytes = inputs_canonical_hash.0;
                        bytes[0] ^= 0x01;
                        *inputs_canonical_hash = B256::from(bytes);
                    }
                }
                _ => panic!("leader entry is not Payload"),
            }
            let state =
                std::sync::Arc::new(std::sync::Mutex::new(crate::witness::WitnessState::new(
                    WitnessExecutionMode::VerifierReplay,
                    Some(leader.attestation_public()),
                    crate::witness::EnclaveWitnessTapeV1 {
                        prefix: vec![bad],
                        txs: Vec::new(),
                    },
                )));
            let _scope = crate::witness::WitnessExecutionScope::enter(state.clone());
            let error = tee_consensus_typed_call(
                CallMeta::recorded("corrupt_replay"),
                |_| -> Result<u64, crate::TransportError> { panic!("replay executed") },
                |_| panic!("replay encoded"),
                verify_signed_u64,
            )
            .unwrap_err();
            assert!(matches!(
                std::error::Error::source(&error)
                    .and_then(|cause| cause.downcast_ref::<TeeAttestationError>()),
                Some(TeeAttestationError::SignatureMismatch(_))
            ));
            let witness = state.lock().unwrap();
            assert_eq!(witness.prefix_cursor, 0);
            assert_eq!(
                witness.first_witness_fault,
                Some(BlockWitnessFault::TapeInvalid)
            );
        }
    }
}
