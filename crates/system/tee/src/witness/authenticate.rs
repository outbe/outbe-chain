//! Existing operation-specific preimages for the shared confidential read/transform client.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    TransportError,
};
use alloy_primitives::B256;

fn hash_matches(expected: B256, actual: B256) -> Result<(), TransportError> {
    if expected == actual {
        Ok(())
    } else {
        Err(TransportError::Attestation(
            "witness request commitment mismatch".into(),
        ))
    }
}
fn verify_hash_and_preimage(
    key: &[u8; 32],
    expected: B256,
    actual: B256,
    preimage: Vec<u8>,
    attestation_tag: &[u8],
) -> Result<(), TransportError> {
    hash_matches(expected, actual)?;
    crate::tribute_v2::verify_attestation(key, &preimage, attestation_tag)
}

fn verify_read_amount(
    key: &[u8; 32],
    expected: Result<B256, serde_json::Error>,
    (actual, amount, attestation_tag): (B256, alloy_primitives::U256, &[u8]),
    preimage_fn: fn(B256, alloy_primitives::U256) -> Vec<u8>,
) -> Result<(), TransportError> {
    let expected = expected.map_err(TransportError::codec)?;
    verify_hash_and_preimage(
        key,
        expected,
        actual,
        preimage_fn(expected, amount),
        attestation_tag,
    )
}

pub(crate) fn authenticate_request(
    request: &EnclaveRequest,
    key: &[u8; 32],
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    use EnclaveRequest as Q;
    use EnclaveResponse as A;
    let verify =
        |preimage: Vec<u8>, tag: &[u8]| crate::tribute_v2::verify_attestation(key, &preimage, tag);
    match (request, response) {
        (
            Q::ReadTributeAmountsV2 { tributes },
            A::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = crate::tribute_v2::tribute_read_inputs_hash(tributes)
                .map_err(TransportError::codec)?;
            hash_matches(expected, *inputs_canonical_hash)?;
            if amounts.len() != tributes.len() {
                return Err(TransportError::UnexpectedResponse);
            }
            verify(
                crate::tribute_v2::tribute_read_attestation_preimage(expected, amounts)
                    .map_err(TransportError::codec)?,
                attestation_tag,
            )
        }
        (Q::MineEncryptedNodV1 { request }, A::EncryptedNodMinedV1 { result }) => {
            let expected = crate::nod_mine::inputs_hash(request).map_err(TransportError::codec)?;
            let preimage =
                crate::nod_mine::result_preimage(result).map_err(TransportError::codec)?;
            verify_hash_and_preimage(
                key,
                expected,
                result.inputs_canonical_hash,
                preimage,
                &result.attestation_tag,
            )
        }
        (
            Q::MineEncryptedNodV1 { request },
            A::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = crate::nod_mine::inputs_hash(request).map_err(TransportError::codec)?;
            let preimage =
                crate::response_auth::attestation(b"outbe/nod/mint-rejected/v1", expected, reason)
                    .map_err(TransportError::codec)?;
            verify_hash_and_preimage(
                key,
                expected,
                *inputs_canonical_hash,
                preimage,
                attestation_tag,
            )
        }
        (
            Q::ReadNodAmountV1 { nod },
            A::NodAmountReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_read_amount(
            key,
            crate::nod_mine::nod_read_hash(nod),
            (*inputs_canonical_hash, *amount, attestation_tag),
            crate::nod_mine::nod_read_preimage,
        ),
        (
            Q::EncryptNodForTributeV1 {
                tribute,
                terms,
                amount,
            },
            A::NodEncryptedForTributeV1 {
                nod,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = crate::response_auth::hash(
                b"outbe/nod/tribute-create-inputs/v1",
                &(tribute, terms, amount),
            )
            .map_err(TransportError::codec)?;
            let preimage = crate::response_auth::attestation(
                b"outbe/nod/tribute-create-result/v1",
                expected,
                nod,
            )
            .map_err(TransportError::codec)?;
            verify_hash_and_preimage(
                key,
                expected,
                *inputs_canonical_hash,
                preimage,
                attestation_tag,
            )
        }
        (
            Q::ApplyTributeDayOpV2 { request },
            A::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = crate::tribute_day::day_operation_inputs_hash(request)
                .map_err(TransportError::codec)?;
            let preimage = crate::tribute_day::day_operation_attestation_preimage(expected, record)
                .map_err(TransportError::codec)?;
            verify_hash_and_preimage(
                key,
                expected,
                *inputs_canonical_hash,
                preimage,
                attestation_tag,
            )
        }
        (
            Q::ReadTributeDayAmountV2 { record },
            A::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_read_amount(
            key,
            crate::tribute_day::day_read_inputs_hash(record),
            (*inputs_canonical_hash, *amount, attestation_tag),
            crate::tribute_day::day_read_attestation_preimage,
        ),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
