use super::{
    canonical_empty_summary, require_global_nominal_conservation, stream_result_chunks,
    FinalizationResultChunkV1, LysisFinalizationErrorV1, ResultBindingV1, SummaryBindingV1,
};
use crate::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_compressed_entities::derive_poseidon_entity_id;
use outbe_ocomp_protocol::{
    local_control::poc_schema_limits,
    result::{NodActionV1, OutputManifestEntryV1, ResultChunkV1},
    unit::PlanCommitmentV1,
    CasObjectRefV1, ObjectKind,
};
use outbe_primitives::time::WorldwideDay;

#[test]
fn a_result_chunk_with_an_entry_beyond_the_call_price_bound_is_not_finalized() {
    let limits = poc_schema_limits();
    let day = WorldwideDay::new(20_260_724);
    let owner = Address::repeat_byte(0x51);
    let id = *derive_poseidon_entity_id(owner, day).unwrap();
    let plan = PlanCommitmentV1 {
        protocol_bundle_hash: B256::repeat_byte(0x01),
        job_id: B256::repeat_byte(0x02),
        attempt: 1,
        input_manifest_hash: B256::repeat_byte(0x03),
        wwd: day.value(),
        lysis_limit_minor: U256::from(1),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: 1,
        max_tributes_per_work_shard: PRIMARY_WORK_SHARD_SIZE,
        primary_work_unit_count: 1,
        primary_work_unit_root: B256::repeat_byte(0x04),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    let chunk = ResultChunkV1 {
        protocol_bundle_hash: plan.protocol_bundle_hash,
        job_id: plan.job_id,
        attempt: plan.attempt,
        chunk_ordinal: 0,
        first_nod_ordinal: 0,
        ordered_nod_actions: vec![NodActionV1 {
            raw_ordinal: 0,
            tribute_id: id,
            nod_id: id,
            owner,
            wwd: day.value(),
            league_id: 1,
            gratis_load_minor: U256::from(1),
            entry_price_minor: U256::MAX / U256::from(100 + u32::from(u16::MAX)) + U256::from(1),
            settlement_cost_minor: U256::from(1),
            issuance_currency: 840,
            reference_currency: 840,
        }],
        ordered_eligible_contributors: Vec::new(),
    };
    let bytes = chunk.encode_canonical(&limits).unwrap();
    let mut summary = canonical_empty_summary(
        SummaryBindingV1 {
            protocol_bundle_hash: plan.protocol_bundle_hash,
            job_id: plan.job_id,
            attempt: plan.attempt,
            plan_hash: B256::ZERO,
        },
        0,
    )
    .unwrap();
    summary.nod_count = 1;
    let item = FinalizationResultChunkV1 {
        chunk_ordinal: 0,
        summary: summary.clone(),
        output_manifest_entry: OutputManifestEntryV1 {
            chunk_ordinal: 0,
            result_chunk_hash: chunk.result_chunk_hash(&limits).unwrap(),
            result_chunk_ref: CasObjectRefV1 {
                transport_digest: keccak256(&bytes),
                encoded_bytes: u64::try_from(bytes.len()).unwrap(),
                expected_ocb1_kind: Some(ObjectKind::ResultChunkV1.tag()),
            },
        },
        canonical_chunk_bytes: bytes,
    };

    assert!(matches!(
        stream_result_chunks(
            ResultBindingV1 {
                job_id: plan.job_id,
                plan: &plan,
                plan_hash: B256::ZERO,
            },
            &summary,
            [Ok(item)],
            &limits
        ),
        Err(LysisFinalizationErrorV1::Authority("Nod entry price bound"))
    ));
}

#[test]
fn finalizer_enforces_nominal_conservation_only_at_the_global_root() {
    require_global_nominal_conservation(U256::from(2_000), U256::from(2_570))
        .expect("globally conserved totals");
    require_global_nominal_conservation(U256::from(2_570), U256::from(2_570))
        .expect("all Tribute can be eligible");
    assert!(require_global_nominal_conservation(U256::from(2_571), U256::from(2_570)).is_err());
}
