//! Encrypted day-state transforms through the pinned local NodeHost session.

use alloy_primitives::U256;
use outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2;

use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    tribute_client::request_local_enclave,
    tribute_day::{self, TributeDayOpRequestV2},
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
        .ok_or_else(|| {
            TransportError::EnclaveError("invalid Tribute day predecessor version".into())
        })?;
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

pub fn read_day_amount(record: &EncryptedTributeDayAmountV2) -> Result<U256, TransportError> {
    let expected = tribute_day::day_read_inputs_hash(record).map_err(codec)?;
    let (key, response) = request_local_enclave(EnclaveRequest::ReadTributeDayAmountV2 {
        record: record.clone(),
    })?;
    let EnclaveResponse::TributeDayAmountReadV2 {
        amount,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(response_error(response));
    };
    if expected != inputs_canonical_hash {
        return Err(TransportError::EnclaveError(
            "Tribute day read response does not match request".into(),
        ));
    }
    let preimage = tribute_day::day_read_attestation_preimage(expected, amount);
    verify_attestation(&key, &preimage, &attestation_tag)?;
    Ok(amount)
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
