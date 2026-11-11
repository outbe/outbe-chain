use super::codec_error;
use super::path_exists;
use super::replace_bytes_atomically;
use crate::TransportError;
use alloy_primitives::{keccak256, B256};
use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, MAX_ATTESTATION_EVIDENCE_BYTES};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct JournalSubmissionPayload {
    pub evidence: Vec<u8>,
    pub node_signature: [u8; 65],
    pub enclave_signature: [u8; 64],
}

impl JournalSubmissionPayload {
    pub(super) fn new(
        evidence: Vec<u8>,
        node_signature: [u8; 65],
        enclave_signature: [u8; 64],
    ) -> Self {
        Self {
            evidence,
            node_signature,
            enclave_signature,
        }
    }
}

pub(super) struct JournalSubmissionErrors {
    pub node_signature: &'static str,
    pub enclave_signature: &'static str,
}

pub(super) struct JournalSubmissionValidation {
    pub min_len: usize,
    pub max_bytes: u64,
    pub version: u8,
    pub framing_error: &'static str,
    pub length_base: usize,
    pub length_overflow_error: &'static str,
    pub noncanonical_length_error: &'static str,
    pub encode_evidence_length_error: &'static str,
    pub encode_allocation_error: &'static str,
    pub encode_cap_error: Option<&'static str>,
}

pub(super) fn validate_submission_frame(
    input: &[u8],
    validation: &JournalSubmissionValidation,
) -> Result<(), TransportError> {
    if input.len() < validation.min_len
        || u64::try_from(input.len()).unwrap_or(u64::MAX) > validation.max_bytes
        || input[0] != validation.version
    {
        return Err(TransportError::Codec(validation.framing_error.into()));
    }
    Ok(())
}

pub(super) fn validate_submission_evidence_length(
    input_len: usize,
    evidence_len: usize,
    validation: &JournalSubmissionValidation,
) -> Result<(), TransportError> {
    let expected_len = validation
        .length_base
        .checked_add(evidence_len)
        .ok_or_else(|| TransportError::Codec(validation.length_overflow_error.into()))?;
    if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES || input_len != expected_len {
        return Err(TransportError::Codec(
            validation.noncanonical_length_error.into(),
        ));
    }
    Ok(())
}

/// Encode one submission record: the version, `prefix`, the evidence length,
/// the evidence and both signatures. The allocation uses `length_base` plus
/// the evidence length. With `encode_cap_error`, a record longer than
/// `max_bytes` gives that error.
pub(super) fn encode_submission_fields(
    prefix: &[u8],
    payload: &JournalSubmissionPayload,
    validation: &JournalSubmissionValidation,
) -> Result<Vec<u8>, TransportError> {
    let evidence_len = u32::try_from(payload.evidence.len())
        .map_err(|_| TransportError::Codec(validation.encode_evidence_length_error.into()))?;
    let capacity = validation
        .length_base
        .checked_add(payload.evidence.len())
        .ok_or_else(|| TransportError::Codec(validation.encode_allocation_error.into()))?;
    let mut out = Vec::with_capacity(capacity);
    out.push(validation.version);
    out.extend_from_slice(prefix);
    append_submission_payload(&mut out, evidence_len, payload);
    if let Some(cap_error) = validation.encode_cap_error {
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > validation.max_bytes {
            return Err(TransportError::Codec(cap_error.into()));
        }
    }
    Ok(out)
}

fn append_submission_payload(
    out: &mut Vec<u8>,
    evidence_len: u32,
    payload: &JournalSubmissionPayload,
) {
    out.extend_from_slice(&evidence_len.to_be_bytes());
    out.extend_from_slice(&payload.evidence);
    out.extend_from_slice(&payload.node_signature);
    out.extend_from_slice(&payload.enclave_signature);
}

pub(super) fn decode_submission_payload(
    input: &[u8],
    evidence_start: usize,
    evidence_len: usize,
    errors: JournalSubmissionErrors,
) -> Result<JournalSubmissionPayload, TransportError> {
    let evidence_end = evidence_start + evidence_len;
    let evidence = input[evidence_start..evidence_end].to_vec();
    AttestationEvidenceV1::decode_canonical(&evidence).map_err(codec_error)?;
    let node_signature = input[evidence_end..evidence_end + 65]
        .try_into()
        .map_err(|_| TransportError::Codec(errors.node_signature.into()))?;
    let enclave_signature = input[evidence_end + 65..]
        .try_into()
        .map_err(|_| TransportError::Codec(errors.enclave_signature.into()))?;
    Ok(JournalSubmissionPayload::new(
        evidence,
        node_signature,
        enclave_signature,
    ))
}

pub(super) struct JournalRelayValidation {
    pub header_len: usize,
    pub max_bytes: u64,
    pub version: u8,
    pub encode_length_overflow_error: &'static str,
    pub encode_cap_error: &'static str,
    pub raw_len_offset: usize,
    pub raw_len_error: &'static str,
    pub from_block_offset: Option<usize>,
    pub from_block_error: &'static str,
    pub framing_error: &'static str,
    pub raw_length_error: &'static str,
    pub commitments_error: &'static str,
}

pub(super) struct JournalRelayFields {
    pub payload: JournalRelayPayload,
    pub from_block: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct JournalRelayPayload {
    pub submission_hash: B256,
    pub calldata_hash: B256,
    pub transaction_hash: B256,
    pub raw_transaction: Vec<u8>,
}

impl JournalRelayPayload {
    pub(super) fn new(submission_hash: B256, material: RelayMaterial) -> Self {
        Self {
            submission_hash,
            calldata_hash: material.calldata_hash,
            transaction_hash: material.transaction_hash,
            raw_transaction: material.raw_transaction,
        }
    }
}

pub(super) fn encode_relay_fields(
    payload: &JournalRelayPayload,
    from_block: Option<u64>,
    validation: &JournalRelayValidation,
) -> Result<Vec<u8>, TransportError> {
    let raw_len = u32::try_from(payload.raw_transaction.len())
        .map_err(|_| TransportError::Codec(validation.encode_length_overflow_error.into()))?;
    let mut out = Vec::with_capacity(validation.header_len + payload.raw_transaction.len());
    out.push(validation.version);
    out.extend_from_slice(payload.submission_hash.as_slice());
    out.extend_from_slice(payload.calldata_hash.as_slice());
    out.extend_from_slice(payload.transaction_hash.as_slice());
    if let Some(from_block) = from_block {
        out.extend_from_slice(&from_block.to_be_bytes());
    }
    out.extend_from_slice(&raw_len.to_be_bytes());
    out.extend_from_slice(&payload.raw_transaction);
    if u64::try_from(out.len()).unwrap_or(u64::MAX) > validation.max_bytes {
        return Err(TransportError::Codec(validation.encode_cap_error.into()));
    }
    Ok(out)
}

pub(super) fn decode_relay_fields(
    input: &[u8],
    validation: &JournalRelayValidation,
) -> Result<JournalRelayFields, TransportError> {
    validate_relay_frame(input, validation)?;
    let raw_len = u32::from_be_bytes(
        input[validation.raw_len_offset..validation.header_len]
            .try_into()
            .map_err(|_| TransportError::Codec(validation.raw_len_error.into()))?,
    ) as usize;
    validate_relay_raw_length(input.len(), raw_len, validation)?;
    let submission_hash = B256::from_slice(&input[1..33]);
    let calldata_hash = B256::from_slice(&input[33..65]);
    let transaction_hash = B256::from_slice(&input[65..97]);
    let from_block = if let Some(offset) = validation.from_block_offset {
        u64::from_be_bytes(
            input[offset..validation.raw_len_offset]
                .try_into()
                .map_err(|_| TransportError::Codec(validation.from_block_error.into()))?,
        )
    } else {
        0
    };
    let fields = JournalRelayFields {
        payload: JournalRelayPayload {
            submission_hash,
            calldata_hash,
            transaction_hash,
            raw_transaction: input[validation.header_len..].to_vec(),
        },
        from_block,
    };
    validate_relay_commitments(&fields.payload, validation)?;
    Ok(fields)
}

fn validate_relay_frame(
    input: &[u8],
    validation: &JournalRelayValidation,
) -> Result<(), TransportError> {
    if input.len() < validation.header_len
        || u64::try_from(input.len()).unwrap_or(u64::MAX) > validation.max_bytes
        || input[0] != validation.version
    {
        return Err(TransportError::Codec(validation.framing_error.into()));
    }
    Ok(())
}

fn validate_relay_raw_length(
    input_len: usize,
    raw_len: usize,
    validation: &JournalRelayValidation,
) -> Result<(), TransportError> {
    if input_len != validation.header_len + raw_len || raw_len == 0 {
        return Err(TransportError::Codec(validation.raw_length_error.into()));
    }
    Ok(())
}

fn validate_relay_commitments(
    payload: &JournalRelayPayload,
    validation: &JournalRelayValidation,
) -> Result<(), TransportError> {
    if payload.submission_hash.is_zero()
        || payload.calldata_hash.is_zero()
        || payload.transaction_hash != keccak256(&payload.raw_transaction)
    {
        return Err(TransportError::Codec(validation.commitments_error.into()));
    }
    Ok(())
}

pub(super) struct ExactCheckpoint<'a, T> {
    pub path: &'a Path,
    pub next: &'a Path,
    pub scratch: &'a Path,
    pub root: &'a Path,
    pub read: fn(&Path) -> Result<T, TransportError>,
    pub conflict_error: &'static str,
}

pub(super) fn persist_exact_checkpoint<T: Eq>(
    checkpoint: ExactCheckpoint<'_, T>,
    requested: T,
    bytes: &[u8],
) -> Result<T, TransportError> {
    if path_exists(checkpoint.path)? {
        let durable = (checkpoint.read)(checkpoint.path)?;
        if durable == requested {
            return Ok(durable);
        }
        return Err(TransportError::Codec(checkpoint.conflict_error.into()));
    }
    replace_bytes_atomically(
        checkpoint.path,
        checkpoint.next,
        checkpoint.scratch,
        bytes,
        checkpoint.root,
    )?;
    Ok(requested)
}

pub(super) struct CheckedRelayInput<'a> {
    calldata_hash: B256,
    raw_transaction: &'a [u8],
}

pub(super) struct RelayMaterial {
    pub calldata_hash: B256,
    pub transaction_hash: B256,
    pub raw_transaction: Vec<u8>,
}

impl<'a> CheckedRelayInput<'a> {
    pub(super) fn new(
        calldata_hash: B256,
        raw_transaction: &'a [u8],
        incomplete_error: &'static str,
    ) -> Result<Self, TransportError> {
        if calldata_hash.is_zero() || raw_transaction.is_empty() {
            return Err(TransportError::Codec(incomplete_error.into()));
        }
        Ok(Self {
            calldata_hash,
            raw_transaction,
        })
    }

    pub(super) fn into_material(self) -> RelayMaterial {
        RelayMaterial {
            calldata_hash: self.calldata_hash,
            transaction_hash: keccak256(self.raw_transaction),
            raw_transaction: self.raw_transaction.to_vec(),
        }
    }
}
