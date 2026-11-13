//! Operation-specific preimage verification for the shared confidential read/transform client.

use alloy_primitives::{B256, U256};
use outbe_tee::{
    tee_call::{verify_signed_rejection, SignedRejectionRef},
    AttestationMetricClass, TeeAttestationError, TransportError,
};

use super::{
    nod_mine,
    protocol::{EnclaveRequest, EnclaveResponse},
    response_auth, tribute_day, tribute_v2,
};

fn hash_matches(expected: B256, actual: B256) -> Result<(), TransportError> {
    if expected == actual {
        Ok(())
    } else {
        Err(TransportError::Attestation(
            "witness request commitment mismatch".into(),
        ))
    }
}

struct AttestedClaim<'a> {
    expected: B256,
    actual: B256,
    preimage: Vec<u8>,
    attestation_tag: &'a [u8],
}

impl AttestedClaim<'_> {
    fn verify(self, key: &[u8; 32]) -> Result<(), TransportError> {
        hash_matches(self.expected, self.actual)?;
        tribute_v2::verify_attestation(key, &self.preimage, self.attestation_tag)
    }
}

fn verify_read_amount(
    key: &[u8; 32],
    expected: Result<B256, serde_json::Error>,
    (actual, amount, attestation_tag): (B256, U256, &[u8]),
    preimage_fn: fn(B256, U256) -> Vec<u8>,
) -> Result<(), TransportError> {
    let expected = expected.map_err(TransportError::codec)?;
    AttestedClaim {
        expected,
        actual,
        preimage: preimage_fn(expected, amount),
        attestation_tag,
    }
    .verify(key)
}

fn verify_serialized_pair<'a, R>(
    key: &[u8; 32],
    expected: Result<B256, serde_json::Error>,
    (actual, result, attestation_tag): (B256, &'a R, &'a [u8]),
    preimage_fn: fn(B256, &'a R) -> Result<Vec<u8>, serde_json::Error>,
) -> Result<(), TransportError> {
    let expected = expected.map_err(TransportError::codec)?;
    let preimage = preimage_fn(expected, result).map_err(TransportError::codec)?;
    AttestedClaim {
        expected,
        actual,
        preimage,
        attestation_tag,
    }
    .verify(key)
}

fn verify_domain_pair<I: serde::Serialize, R: serde::Serialize>(
    key: &[u8; 32],
    (input_domain, result_domain): (&[u8], &[u8]),
    input: &I,
    (actual, result, attestation_tag): (B256, &R, &[u8]),
) -> Result<(), TransportError> {
    let expected = response_auth::hash(input_domain, input).map_err(TransportError::codec)?;
    let preimage = response_auth::attestation(result_domain, expected, result)
        .map_err(TransportError::codec)?;
    AttestedClaim {
        expected,
        actual,
        preimage,
        attestation_tag,
    }
    .verify(key)
}

pub(crate) fn authenticate_request(
    request: &EnclaveRequest,
    key: &[u8; 32],
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    use EnclaveRequest as Q;
    use EnclaveResponse as A;
    let verify =
        |preimage: Vec<u8>, tag: &[u8]| tribute_v2::verify_attestation(key, &preimage, tag);
    match (request, response) {
        (
            Q::ReadTributeAmountsV2 { tributes },
            A::TributeAmountsReadV2 {
                amounts,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected =
                tribute_v2::tribute_read_inputs_hash(tributes).map_err(TransportError::codec)?;
            hash_matches(expected, *inputs_canonical_hash)?;
            if amounts.len() != tributes.len() {
                return Err(TransportError::UnexpectedResponse);
            }
            verify(
                tribute_v2::tribute_read_attestation_preimage(expected, amounts)
                    .map_err(TransportError::codec)?,
                attestation_tag,
            )
        }
        (Q::MineEncryptedNodV1 { request }, A::EncryptedNodMinedV1 { result }) => {
            let expected = nod_mine::inputs_hash(request).map_err(TransportError::codec)?;
            let preimage = nod_mine::result_preimage(result).map_err(TransportError::codec)?;
            AttestedClaim {
                expected,
                actual: result.inputs_canonical_hash,
                preimage,
                attestation_tag: &result.attestation_tag,
            }
            .verify(key)
        }
        (
            Q::MineEncryptedNodV1 { request },
            A::EncryptedNodMintRejectedV1 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => {
            let expected = nod_mine::inputs_hash(request).map_err(TransportError::codec)?;
            verify_signed_rejection(
                key,
                SignedRejectionRef {
                    domain: b"outbe/nod/mint-rejected/v1",
                    expected_inputs_hash: expected,
                    actual_inputs_hash: *inputs_canonical_hash,
                    reason,
                    tag: attestation_tag,
                },
            )
            .map_err(|cause| match cause {
                TeeAttestationError::InputsHashMismatch { .. } => {
                    TransportError::Attestation("witness request commitment mismatch".into())
                }
                other => {
                    TransportError::app_attestation(AttestationMetricClass::TributeOffer, other)
                }
            })
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
            nod_mine::nod_read_hash(nod),
            (*inputs_canonical_hash, *amount, attestation_tag),
            nod_mine::nod_read_preimage,
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
        ) => verify_domain_pair(
            key,
            (
                b"outbe/nod/tribute-create-inputs/v1",
                b"outbe/nod/tribute-create-result/v1",
            ),
            &(tribute, terms, amount),
            (*inputs_canonical_hash, nod, attestation_tag),
        ),
        (
            Q::ApplyTributeDayOpV2 { request },
            A::TributeDayOpAppliedV2 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_serialized_pair(
            key,
            tribute_day::day_operation_inputs_hash(request),
            (*inputs_canonical_hash, record, attestation_tag),
            tribute_day::day_operation_attestation_preimage,
        ),
        (
            Q::ReadTributeDayAmountV2 { record },
            A::TributeDayAmountReadV2 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_read_amount(
            key,
            tribute_day::day_read_inputs_hash(record),
            (*inputs_canonical_hash, *amount, attestation_tag),
            tribute_day::day_read_attestation_preimage,
        ),
        (
            Q::ExecuteLysisFractionsV1 { request },
            A::LysisFractionsExecutedV1 {
                result,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_serialized_pair(
            key,
            tribute_day::lysis_fractions_inputs_hash(request),
            (*inputs_canonical_hash, result.as_ref(), attestation_tag),
            tribute_day::lysis_fractions_attestation_preimage,
        ),
        (
            Q::ReadTributeDayFractionsV1 { record },
            A::TributeDayFractionsReadV1 {
                fractions,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_serialized_pair(
            key,
            tribute_day::day_fractions_read_inputs_hash(record),
            (*inputs_canonical_hash, fractions, attestation_tag),
            tribute_day::day_fractions_read_attestation_preimage,
        ),
        (
            Q::IssueNodFromTributeV1 { request },
            A::NodIssuedFromTributeV1 {
                outcome,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_domain_pair(
            key,
            (
                b"outbe/nod/issue-from-tribute-inputs/v1",
                b"outbe/nod/issue-from-tribute-result/v1",
            ),
            request,
            (*inputs_canonical_hash, outcome, attestation_tag),
        ),
        (
            Q::ApplyNodGroupLoadOpV1 { request },
            A::NodGroupLoadOpAppliedV1 {
                record,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_serialized_pair(
            key,
            nod_mine::nod_group_load_inputs_hash(request),
            (*inputs_canonical_hash, record, attestation_tag),
            nod_mine::nod_group_load_attestation_preimage,
        ),
        (
            Q::ReadNodGroupLoadV1 { record },
            A::NodGroupLoadReadV1 {
                amount,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_read_amount(
            key,
            nod_mine::nod_group_load_read_hash(record),
            (*inputs_canonical_hash, *amount, attestation_tag),
            nod_mine::nod_group_load_read_preimage,
        ),
        (
            Q::EncryptNodQuoteV1 {
                nod,
                context,
                payment_minor,
            },
            A::NodQuoteEncryptedV1 {
                encrypted_payment,
                inputs_canonical_hash,
                attestation_tag,
            },
        ) => verify_domain_pair(
            key,
            (
                b"outbe/nod/quote-encrypt-inputs/v1",
                b"outbe/nod/quote-encrypt-result/v1",
            ),
            &(nod, context, payment_minor),
            (*inputs_canonical_hash, encrypted_payment, attestation_tag),
        ),
        _ => Err(TransportError::UnexpectedResponse),
    }
}
