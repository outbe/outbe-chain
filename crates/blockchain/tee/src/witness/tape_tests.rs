use super::tape::*;
use crate::protocol::EnclaveResponse;
use alloy_primitives::B256;

fn entries() -> Vec<EnclaveWitnessEntryV1> {
    let tag = vec![0xAA; 64];
    vec![
        EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: vec![1],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
            request_hash: B256::ZERO,
            outcome: vec![1],
            onboarding_artifact: vec![2],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
            request_hash: B256::ZERO,
            onboarding_artifact: vec![2],
            attestation_tag: tag.clone(),
        },
        EnclaveWitnessEntryV1::Payload {
            payload: vec![3, 4, 5],
            inputs_canonical_hash: B256::ZERO,
            attestation_tag: tag,
        },
    ]
}

#[test]
fn all_frozen_variants_round_trip_conversion_and_serialize_with_indices_zero_through_three() {
    for (index, entry) in entries().into_iter().enumerate() {
        let response = EnclaveResponse::from(entry.clone());
        let converted = EnclaveWitnessEntryV1::try_from(response.clone()).unwrap();
        assert_eq!(converted, entry);
        assert_eq!(EnclaveResponse::from(converted), response);
        let encoded = postcard::to_allocvec(&entry).unwrap();
        assert_eq!(encoded[0], index as u8, "variant index {index}");
    }
}

#[test]
fn semantic_equality_ignores_only_attestation_tag() {
    for entry in entries() {
        let mut other = entry.clone();
        match &mut other {
            EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
                attestation_tag, ..
            }
            | EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
                attestation_tag,
                ..
            }
            | EnclaveWitnessEntryV1::Payload {
                attestation_tag, ..
            } => attestation_tag.push(1),
        }
        assert!(entry.semantic_eq_ignoring_attestation_tag(&other));
    }
    let variants = entries();
    for (i, entry) in variants.iter().enumerate() {
        assert!(!entry.semantic_eq_ignoring_attestation_tag(&variants[(i + 1) % variants.len()]));
    }
}

fn mutate_commitment(entry: &mut EnclaveWitnessEntryV1) {
    let changed = B256::repeat_byte(1);
    match entry {
        EnclaveWitnessEntryV1::DcapVerificationFinishedV1 { request_hash, .. }
        | EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 { request_hash, .. }
        | EnclaveWitnessEntryV1::GramineDirectDevOnboardingArtifactPreparedV2 {
            request_hash,
            ..
        } => {
            *request_hash = changed;
        }
        EnclaveWitnessEntryV1::Payload {
            inputs_canonical_hash,
            ..
        } => {
            *inputs_canonical_hash = changed;
        }
    }
}

#[test]
fn semantic_equality_compares_commitments_and_payload_bytes() {
    for entry in entries() {
        let mut changed = entry.clone();
        mutate_commitment(&mut changed);
        assert!(
            !entry.semantic_eq_ignoring_attestation_tag(&changed),
            "{entry:?}"
        );
    }

    let payload = EnclaveWitnessEntryV1::Payload {
        payload: vec![1, 2, 3],
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: vec![0xAA; 64],
    };
    let mut mutated_payload = payload.clone();
    if let EnclaveWitnessEntryV1::Payload { payload, .. } = &mut mutated_payload {
        payload.push(0xFF);
    }
    assert!(!payload.semantic_eq_ignoring_attestation_tag(&mutated_payload));
}

#[test]
fn unsupported_and_non_recordable_responses_are_not_witness_entries() {
    assert!(EnclaveWitnessEntryV1::try_from(EnclaveResponse::Ack).is_err());
    assert!(EnclaveWitnessEntryV1::try_from(EnclaveResponse::Error {
        message: "err".into()
    })
    .is_err());
    assert!(EnclaveWitnessEntryV1::try_from(EnclaveResponse::Payload {
        recordable: false,
        payload: vec![1],
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: vec![0; 64],
    })
    .is_err());
}
