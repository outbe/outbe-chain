//! Encrypted day-state transforms through the pinned local NodeHost session.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_core_common::tribute_day_encryption::{
    EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1,
};
use outbe_tee::{tee_call::verify_enclave_attestation, TransportError};

use super::{
    errors::EnclaveClientError,
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_client::request_local_enclave,
    tribute_day::{self, LysisFractionsRequestV1, LysisFractionsResultV1, TributeDayOpRequestV2},
};

fn next_predecessor_version(previous_version: Option<u64>) -> Result<u64, EnclaveClientError> {
    let prev = previous_version.ok_or(EnclaveClientError::InvalidTributeDayPredecessorVersion {
        expected: 1,
        actual: 0,
    })?;
    prev.checked_add(1)
        .ok_or(EnclaveClientError::InvalidTributeDayPredecessorVersion {
            expected: 1,
            actual: u8::try_from(prev).unwrap_or(u8::MAX),
        })
}

fn verify_day_attestation(
    key: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), EnclaveClientError> {
    verify_enclave_attestation(key, preimage, tag)
        .map_err(EnclaveClientError::TributeDayAttestation)
}

/// Apply an arithmetic or freeze transition to an encrypted Tribute day accumulator.
pub fn apply_day_operation(
    request: TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2, EnclaveClientError> {
    let expected = tribute_day::day_operation_inputs_hash(&request).map_err(codec)?;
    let chain_id = request.chain_id;
    let day = request.worldwide_day;
    let frozen = matches!(
        request.operation,
        tribute_day::TributeDayOperationV2::Freeze
    );
    let version = match request.previous.as_ref() {
        None => 1,
        Some(previous) => next_predecessor_version(previous.version())?,
    };
    let (key, response) = request_local_enclave(EnclaveRequest::ApplyTributeDayOpV2 {
        request: Box::new(request),
    })?;
    let EnclaveResponse::TributeDayOpAppliedV2 {
        record,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse.into());
    };
    let expected_identity = (chain_id, day, frozen, Some(version));
    let returned_identity = (
        record.chain_id,
        record.worldwide_day,
        record.frozen,
        record.version(),
    );
    if expected != inputs_canonical_hash || returned_identity != expected_identity {
        return Err(TransportError::EnclaveError(
            "Tribute day response does not match request".into(),
        )
        .into());
    }
    let preimage =
        tribute_day::day_operation_attestation_preimage(expected, &record).map_err(codec)?;
    verify_day_attestation(&key, &preimage, &attestation_tag)?;
    Ok(record)
}

fn request_amount_read(
    expected: Result<B256, serde_json::Error>,
    request: EnclaveRequest,
    preimage_fn: fn(B256, U256) -> Vec<u8>,
    mismatch_msg: &'static str,
) -> Result<U256, EnclaveClientError> {
    let expected = expected.map_err(codec)?;
    let (key, response) = request_local_enclave(request)?;
    let (amount, inputs_canonical_hash, attestation_tag) = match response {
        EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        } => (amount, inputs_canonical_hash, attestation_tag),
        _ => return Err(TransportError::UnexpectedResponse.into()),
    };
    if expected != inputs_canonical_hash {
        return Err(TransportError::EnclaveError(mismatch_msg.into()).into());
    }
    let preimage = preimage_fn(expected, amount);
    verify_day_attestation(&key, &preimage, &attestation_tag)?;
    Ok(amount)
}

/// Read the decrypted nominal total of an encrypted Tribute day accumulator.
pub fn read_day_amount(record: &EncryptedTributeDayAmountV2) -> Result<U256, EnclaveClientError> {
    request_amount_read(
        tribute_day::day_read_inputs_hash(record),
        EnclaveRequest::ReadTributeDayAmountV2 {
            record: record.clone(),
        },
        tribute_day::day_read_attestation_preimage,
        "Tribute day read response does not match request",
    )
}

/// Execute confidential Lysis fraction settlement for a completed WorldwideDay.
pub fn execute_lysis_fractions(
    request: LysisFractionsRequestV1,
) -> Result<LysisFractionsResultV1, EnclaveClientError> {
    let expected = tribute_day::lysis_fractions_inputs_hash(&request).map_err(codec)?;
    let expected_identity = (
        request.chain_id,
        request.worldwide_day,
        request.emission_day,
        request.completed_at,
        Some(1),
    );
    let (key, response) = request_local_enclave(EnclaveRequest::ExecuteLysisFractionsV1 {
        request: Box::new(request),
    })?;
    let EnclaveResponse::LysisFractionsExecutedV1 {
        result,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse.into());
    };
    let returned_identity = (
        result.day_fractions.chain_id,
        result.day_fractions.worldwide_day,
        result.day_fractions.emission_day,
        result.day_fractions.completed_at,
        result.day_fractions.version(),
    );
    if expected != inputs_canonical_hash || returned_identity != expected_identity {
        return Err(TransportError::EnclaveError(
            "Lysis fractions response does not match request".into(),
        )
        .into());
    }
    let preimage =
        tribute_day::lysis_fractions_attestation_preimage(expected, &result).map_err(codec)?;
    verify_day_attestation(&key, &preimage, &attestation_tag)?;
    Ok(*result)
}

/// Read decrypted per-league Lysis fractions from an encrypted day-fractions record.
pub fn read_day_fractions(
    record: &EncryptedTributeDayFractionsV1,
) -> Result<BTreeMap<u16, u64>, EnclaveClientError> {
    let expected = tribute_day::day_fractions_read_inputs_hash(record).map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::ReadTributeDayFractionsV1 {
        record: record.clone(),
    })?;
    let EnclaveResponse::TributeDayFractionsReadV1 {
        fractions,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse.into());
    };
    if expected != inputs_canonical_hash {
        return Err(TransportError::EnclaveError(
            "Tribute day fractions read response does not match request".into(),
        )
        .into());
    }
    let preimage = tribute_day::day_fractions_read_attestation_preimage(expected, &fractions)
        .map_err(codec)?;
    verify_day_attestation(&key, &preimage, &attestation_tag)?;
    Ok(fractions)
}

fn codec(error: serde_json::Error) -> EnclaveClientError {
    TransportError::codec(error).into()
}
