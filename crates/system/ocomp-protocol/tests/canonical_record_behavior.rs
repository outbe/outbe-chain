use alloy_primitives::{Address, B256, U256};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    input::{AuthenticatedOpeningV1, OpeningSourceKind},
    opening::{RawContractOpeningProofV1, RawStorageSlotV1},
    profile::poc_schema_limits,
    unit::{
        EntityIdHalfOpenRange, PlanCommitmentV1, UnitArtifactV1, UnitInterval, UnitPhase,
        UnitSpecV1, WorkOutputHeaderV1,
    },
    ProtocolError, SchemaLimits,
};

fn cap_limits() -> SchemaLimits {
    SchemaLimits {
        max_bounded_bytes: 3,
        max_proof_bytes: 5,
        ..poc_schema_limits()
    }
}

fn unit_limits() -> SchemaLimits {
    SchemaLimits {
        max_bounded_bytes: 128,
        ..cap_limits()
    }
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn bounded_opening() -> AuthenticatedOpeningV1 {
    AuthenticatedOpeningV1 {
        source_kind: OpeningSourceKind::Oracle,
        canonical_subject_key: BoundedBytes(vec![0x10]),
        canonical_value: BoundedBytes(vec![0x20, 0x21, 0x22]),
        opening_codec_id: hash(0x33),
        canonical_opening: BoundedBytes(vec![0x40]),
    }
}

fn proof_opening() -> RawContractOpeningProofV1 {
    let slot = RawStorageSlotV1 {
        slot: hash(0x44),
        value: U256::from(7),
    };
    RawContractOpeningProofV1 {
        contract_address: Address::repeat_byte(0x11),
        state_root: hash(0x22),
        ordered_slots: vec![slot],
        account_proof: ProofBytes(vec![1, 2, 3, 4, 5]),
        storage_proof: ProofBytes(vec![6]),
    }
}

#[test]
fn bounded_record_accepts_its_cap_and_rejects_one_extra_byte() {
    let limits = cap_limits();
    let opening = bounded_opening();
    let encoded = opening.encode_canonical_record(&limits).unwrap();
    let mut expected = vec![2, 0, 0, 0, 1, 0x10, 0, 0, 0, 3, 0x20, 0x21, 0x22];
    expected.extend_from_slice(hash(0x33).as_slice());
    expected.extend_from_slice(&[0, 0, 0, 1, 0x40]);
    assert_eq!(encoded, expected);
    assert_eq!(
        AuthenticatedOpeningV1::decode_canonical_record(&encoded, &limits),
        Ok(opening.clone())
    );

    let mut over_cap = opening;
    over_cap.canonical_value.0.push(0x23);
    assert_eq!(
        over_cap.encode_canonical_record(&limits),
        Err(ProtocolError::InvalidInvariant("bounded byte field cap"))
    );
    let loose_limits = SchemaLimits {
        max_bounded_bytes: 4,
        ..limits
    };
    let over_cap_bytes = over_cap.encode_canonical_record(&loose_limits).unwrap();
    assert_eq!(
        AuthenticatedOpeningV1::decode_canonical_record(&over_cap_bytes, &limits),
        Err(ProtocolError::CapacityExceeded {
            what: "bounded byte field",
            limit: 3,
            actual: 4,
        })
    );
}

#[test]
fn proof_record_accepts_its_distinct_cap_and_rejects_one_extra_byte() {
    let limits = cap_limits();
    let proof = proof_opening();
    let encoded = proof.encode_canonical_record(&limits).unwrap();
    let mut expected = vec![0x11; 20];
    expected.extend_from_slice(hash(0x22).as_slice());
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.extend_from_slice(hash(0x44).as_slice());
    expected.extend_from_slice(&U256::from(7).to_be_bytes::<32>());
    expected.extend_from_slice(&[0, 0, 0, 5, 1, 2, 3, 4, 5]);
    expected.extend_from_slice(&[0, 0, 0, 1, 6]);
    assert_eq!(encoded, expected);
    assert_eq!(
        RawContractOpeningProofV1::decode_canonical_record(&encoded, &limits),
        Ok(proof.clone())
    );

    let mut over_cap = proof;
    over_cap.account_proof.0.push(6);
    assert_eq!(
        over_cap.encode_canonical_record(&limits),
        Err(ProtocolError::InvalidInvariant("proof byte field cap"))
    );
    let loose_limits = SchemaLimits {
        max_proof_bytes: 6,
        ..limits
    };
    let over_cap_bytes = over_cap.encode_canonical_record(&loose_limits).unwrap();
    assert_eq!(
        RawContractOpeningProofV1::decode_canonical_record(&over_cap_bytes, &limits),
        Err(ProtocolError::CapacityExceeded {
            what: "bounded byte field",
            limit: 5,
            actual: 6,
        })
    );
}

fn valid_plan() -> PlanCommitmentV1 {
    PlanCommitmentV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 0,
        input_manifest_hash: hash(3),
        wwd: 1,
        lysis_limit_minor: U256::from(7),
        logical_evaluation_time: 1,
        tribute_count: 1,
        max_tributes_per_work_shard: 1,
        primary_work_unit_count: 1,
        primary_work_unit_root: hash(4),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    }
}

#[test]
fn plan_record_finishes_input_before_exposing_semantic_invalidity() {
    let limits = cap_limits();
    let plan = valid_plan();
    let encoded = plan.encode_canonical_record(&limits).unwrap();
    assert_eq!(encoded.len(), 192);
    assert_eq!(&encoded[144..148], &1_u32.to_be_bytes());
    assert_eq!(
        PlanCommitmentV1::decode_canonical_record(&encoded, &limits),
        Ok(plan)
    );

    let mut zero_population = encoded.clone();
    zero_population[144..148].copy_from_slice(&0_u32.to_be_bytes());
    let decoded = PlanCommitmentV1::decode_canonical_record(&zero_population, &limits).unwrap();
    assert_eq!(decoded.tribute_count, 0);
    assert_eq!(
        decoded.validate_semantics(),
        Err(ProtocolError::InvalidInvariant("plan committed population"))
    );
    assert_eq!(
        decoded.encode_canonical_record(&limits),
        Ok(zero_population.clone())
    );

    zero_population.push(0);
    assert_eq!(
        PlanCommitmentV1::decode_canonical_record(&zero_population, &limits),
        Err(ProtocolError::TrailingBytes {
            offset: encoded.len(),
            remaining: 1,
        })
    );
}

fn valid_artifact() -> Result<(UnitArtifactV1, WorkOutputHeaderV1), ProtocolError> {
    let limits = unit_limits();
    let spec = UnitSpecV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 0,
        phase: UnitPhase::Enumerate,
        interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange {
            start: B256::ZERO,
            end: Some(hash(3)),
        }),
        canonical_ordered_inputs: Vec::new(),
        lysis_program_semantics_hash: hash(4),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    let header = WorkOutputHeaderV1 {
        source_coverage_root: hash(5),
        output_coverage_root: hash(6),
        source_coverage_count: 1,
        output_coverage_count: 1,
    };
    let artifact = UnitArtifactV1::from_canonical_output(
        &spec,
        header.clone(),
        BoundedBytes(vec![0x11, 0x22]),
        &limits,
    )?;
    Ok((artifact, header))
}

#[test]
fn invalid_artifact_header_precedes_missing_payload_and_stale_digest() {
    let limits = unit_limits();
    let (mut artifact, header) = valid_artifact().unwrap();
    assert_eq!(artifact.output_header(&limits), Ok(header));
    let header_len =
        artifact.canonical_output_bytes.0.len() - artifact.phase_payload(&limits).unwrap().len();
    artifact.canonical_output_bytes.0.truncate(header_len);
    artifact.canonical_output_bytes.0[..32].fill(0);
    let expected = ProtocolError::InvalidInvariant("work output coverage header");
    assert_eq!(artifact.output_header(&limits), Err(expected.clone()));
    assert_eq!(artifact.phase_payload(&limits), Err(expected.clone()));
    assert_eq!(artifact.validate_semantics(&limits), Err(expected));
}

#[test]
fn missing_artifact_payload_precedes_stale_digest() {
    let limits = unit_limits();
    let (mut artifact, header) = valid_artifact().unwrap();
    assert_eq!(artifact.phase_payload(&limits), Ok(&[0x11, 0x22][..]));
    let header_len =
        artifact.canonical_output_bytes.0.len() - artifact.phase_payload(&limits).unwrap().len();
    artifact.canonical_output_bytes.0.truncate(header_len);
    assert_eq!(artifact.output_header(&limits), Ok(header));
    let expected = ProtocolError::InvalidInvariant("unit phase payload is non-empty");
    assert_eq!(artifact.phase_payload(&limits), Err(expected.clone()));
    assert_eq!(artifact.validate_semantics(&limits), Err(expected));
}

#[test]
fn changed_artifact_digest_reports_digest_binding_after_valid_output() {
    let limits = unit_limits();
    let (mut artifact, header) = valid_artifact().unwrap();
    assert_eq!(artifact.validate_semantics(&limits), Ok(()));
    artifact.output_semantic_digest = hash(0xff);
    assert_eq!(artifact.output_header(&limits), Ok(header));
    assert_eq!(artifact.phase_payload(&limits), Ok(&[0x11, 0x22][..]));
    assert_eq!(
        artifact.validate_semantics(&limits),
        Err(ProtocolError::InvalidInvariant(
            "unit output semantic digest"
        ))
    );
}
