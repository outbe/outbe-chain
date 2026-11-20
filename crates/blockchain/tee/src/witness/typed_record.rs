//! Typed witness payload recording. The three `Payload` fields stay distinct.

use super::state::{fault_error, locked, typed_execute, BlockWitnessFault, VerifierCallContext};
use super::{EnclaveWitnessEntryV1, WitnessState};
use crate::{errors::WitnessAvailabilityError, TransportError};
use alloy_primitives::B256;
use std::sync::{Arc, Mutex};

/// Tag-free witness bytes, the canonical input digest, and the Ed25519 tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedWitnessPayload {
    /// `tag || canonical body` with the attestation signature removed from the body.
    pub payload: Vec<u8>,
    /// Canonical digest of the operation inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature of the block leader or the local verifier.
    pub attestation_tag: Vec<u8>,
}

/// Borrowed [`TypedWitnessPayload`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedWitnessPayloadRef<'a> {
    /// `tag || canonical body` with the attestation signature removed from the body.
    pub payload: &'a [u8],
    /// Canonical digest of the operation inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature of the block leader or the local verifier.
    pub attestation_tag: &'a [u8],
}

impl TypedWitnessPayload {
    /// Borrow the three witness fields.
    #[must_use]
    pub fn as_ref(&self) -> TypedWitnessPayloadRef<'_> {
        TypedWitnessPayloadRef {
            payload: &self.payload,
            inputs_canonical_hash: self.inputs_canonical_hash,
            attestation_tag: &self.attestation_tag,
        }
    }
}

fn payload_entry(parts: TypedWitnessPayload) -> EnclaveWitnessEntryV1 {
    EnclaveWitnessEntryV1::Payload {
        payload: parts.payload,
        inputs_canonical_hash: parts.inputs_canonical_hash,
        attestation_tag: parts.attestation_tag,
    }
}

pub(super) fn proposer_record_typed<T>(
    handle: &Arc<Mutex<WitnessState>>,
    leader_key: [u8; 32],
    transport: impl FnOnce() -> Result<([u8; 32], T), TransportError>,
    encode: impl FnOnce(&T) -> Result<TypedWitnessPayload, TransportError>,
    decode_and_auth: impl Fn(&[u8; 32], TypedWitnessPayloadRef<'_>) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    let (key, value) = typed_execute(handle, transport)?;
    if key != leader_key {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        return Err(fault_error(BlockWitnessFault::LocalUnavailable));
    }
    let parts = match encode(&value) {
        Ok(parts) => parts,
        Err(error) => {
            locked(handle)?.latch(BlockWitnessFault::TapeInvalid);
            return Err(error);
        }
    };
    if let Err(error) = decode_and_auth(&key, parts.as_ref()) {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        return Err(error);
    }
    locked(handle)?.staged.push(payload_entry(parts));
    Ok(value)
}

pub(super) fn verifier_typed<T>(
    handle: &Arc<Mutex<WitnessState>>,
    ctx: VerifierCallContext,
    transport: impl FnOnce() -> Result<([u8; 32], T), TransportError>,
    encode: impl FnOnce(&T) -> Result<TypedWitnessPayload, TransportError>,
    decode_and_auth: impl Fn(&[u8; 32], TypedWitnessPayloadRef<'_>) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &ctx.entry
    else {
        locked(handle)?.latch(BlockWitnessFault::TapeInvalid);
        return Err(TransportError::UnexpectedResponse);
    };
    let value = match decode_and_auth(
        &ctx.leader_key,
        TypedWitnessPayloadRef {
            payload,
            inputs_canonical_hash: *inputs_canonical_hash,
            attestation_tag,
        },
    ) {
        Ok(value) => value,
        Err(error) => {
            locked(handle)?.latch(BlockWitnessFault::TapeInvalid);
            return Err(error);
        }
    };
    if ctx.with_enclave {
        let (local_key, local_value) = typed_execute(handle, transport)?;
        let local_parts = match encode(&local_value) {
            Ok(local_parts) => local_parts,
            Err(error) => {
                locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
                return Err(error);
            }
        };
        if let Err(error) = decode_and_auth(&local_key, local_parts.as_ref()) {
            locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
            return Err(error);
        }
        if !ctx
            .entry
            .semantic_eq_ignoring_attestation_tag(&payload_entry(local_parts))
        {
            locked(handle)?.latch(BlockWitnessFault::SemanticMismatch);
            return Err(TransportError::unavailable(
                WitnessAvailabilityError::SemanticMismatch,
            ));
        }
    }
    locked(handle)?.consume_entry();
    Ok(value)
}
