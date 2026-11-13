//! Encrypted day-state transforms through the pinned local NodeHost session.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use outbe_primitives::tribute_day_encryption::{
    EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1,
};

use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_client::request_local_enclave,
    tribute_day::{self, LysisFractionsRequestV1, LysisFractionsResultV1, TributeDayOpRequestV2},
    tribute_v2::verify_attestation,
    TransportError,
};

pub fn apply_day_operation(
    request: TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2, TransportError> {
    let expected = tribute_day::day_operation_inputs_hash(&request).map_err(codec)?;
    let chain_id = request.chain_id;
    let day = request.worldwide_day;
    let frozen = matches!(
        request.operation,
        tribute_day::TributeDayOperationV2::Freeze
    );
    let version = request
        .previous
        .as_ref()
        .map_or(Some(1), |previous| previous.version()?.checked_add(1))
        .ok_or(crate::errors::TeeClientVerificationError::InvalidTributeDayPredecessorVersion)?;
    let (key, response) = request_local_enclave(EnclaveRequest::ApplyTributeDayOpV2 {
        request: Box::new(request),
    })?;
    let EnclaveResponse::TributeDayOpAppliedV2 {
        record,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(response_error(response));
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
        ));
    }
    let preimage =
        tribute_day::day_operation_attestation_preimage(expected, &record).map_err(codec)?;
    verify_attestation(&key, &preimage, &attestation_tag)?;
    Ok(record)
}

fn request_amount_read(
    expected: Result<alloy_primitives::B256, serde_json::Error>,
    request: EnclaveRequest,
    preimage_fn: fn(alloy_primitives::B256, U256) -> Vec<u8>,
    mismatch_msg: &'static str,
) -> Result<U256, TransportError> {
    let expected = expected.map_err(codec)?;
    let (key, response) = request_local_enclave(request)?;
    let (amount, inputs_canonical_hash, attestation_tag) = match response {
        EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        } => (amount, inputs_canonical_hash, attestation_tag),
        other => return Err(response_error(other)),
    };
    if expected != inputs_canonical_hash {
        return Err(TransportError::EnclaveError(mismatch_msg.into()));
    }
    let preimage = preimage_fn(expected, amount);
    verify_attestation(&key, &preimage, &attestation_tag)?;
    Ok(amount)
}

pub fn read_day_amount(record: &EncryptedTributeDayAmountV2) -> Result<U256, TransportError> {
    request_amount_read(
        tribute_day::day_read_inputs_hash(record),
        EnclaveRequest::ReadTributeDayAmountV2 {
            record: record.clone(),
        },
        tribute_day::day_read_attestation_preimage,
        "Tribute day read response does not match request",
    )
}

pub fn execute_lysis_fractions(
    request: LysisFractionsRequestV1,
) -> Result<LysisFractionsResultV1, TransportError> {
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
        return Err(response_error(response));
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
        ));
    }
    let preimage =
        tribute_day::lysis_fractions_attestation_preimage(expected, &result).map_err(codec)?;
    verify_attestation(&key, &preimage, &attestation_tag)?;
    Ok(*result)
}

pub fn read_day_fractions(
    record: &EncryptedTributeDayFractionsV1,
) -> Result<BTreeMap<u16, u64>, TransportError> {
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
        return Err(response_error(response));
    };
    if expected != inputs_canonical_hash {
        return Err(TransportError::EnclaveError(
            "Tribute day fractions read response does not match request".into(),
        ));
    }
    let preimage = tribute_day::day_fractions_read_attestation_preimage(expected, &fractions)
        .map_err(codec)?;
    verify_attestation(&key, &preimage, &attestation_tag)?;
    Ok(fractions)
}

fn codec(error: serde_json::Error) -> TransportError {
    TransportError::Codec(error.to_string())
}
fn response_error(response: EnclaveResponse) -> TransportError {
    match response {
        EnclaveResponse::Error { message } => TransportError::EnclaveError(message),
        _ => TransportError::UnexpectedResponse,
    }
}
