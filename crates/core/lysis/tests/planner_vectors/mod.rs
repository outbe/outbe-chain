// OCOMP-TEST-ID: OCM-SEM-002

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, WwdEntityId};
use outbe_lysis::program_v1::artifacts::{
    decode_amount_run, decode_enumerated_run, decode_fidelity_map_output,
    decode_finalized_output_run, decode_fixed_reduce_output, decode_gratis_prefix_down_output,
    decode_gratis_segment_summary, decode_raw_coverage_carrier, encode_amount_run,
    encode_enumerated_run, encode_fidelity_map_output, encode_finalized_output_run,
    encode_fixed_reduce_output, encode_gratis_prefix_down_output, encode_gratis_segment_summary,
    encode_raw_coverage_carrier, enumerate_tributes, gratis_summary_coverage, FixedReduceOutputV1,
    GratisPrefixDownOutputV1, LysisArtifactErrorV1, RawCoverageCarrierV1,
};
use outbe_lysis::program_v1::phases::{
    amount_map, fidelity_map, fidelity_reduce, fidelity_reduce_pair, finalize_fi_fraction_table,
    finalize_gratis_leaf, gratis_prefix_down, gratis_summary, gratis_summary_reduce_pair,
    output_finalize, shuffle_buckets, shuffle_owners, AmountRecordV1, AmountRunV1,
    FidelityAggregateV1, FidelityLeaguePartialV1, FidelityReduceValueV1, GratisIncomingV1,
    GratisLeafPrefixV1, GratisSegmentSummaryV1, GratisSummaryValueV1,
};
use outbe_lysis::program_v1::planner::{
    primary_work_unit_count, LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1,
    PaddedBinaryTreeV1, PlannedProducerV1, PlannedUnitPositionV1, PlannerErrorV1, ReducerInputV1,
    PRIMARY_WORK_SHARD_SIZE,
};
use outbe_lysis::program_v1::reducers::{
    merge_bucket_runs_streaming, merge_owner_runs_streaming, CanonicalRunSpanV1,
    StreamingMergeErrorV1,
};
use outbe_lysis::program_v1::result::{
    decode_root_reduce_output, decode_root_reduce_summary, encode_root_reduce_output,
    encode_root_reduce_summary, LysisListSubtreeCarrierV1, RootReduceOutputV1, RootReduceSummaryV1,
};
use outbe_lysis::program_v1::{
    execute, LeagueFractionV1, ObservationValueV1, ObservedTributeV1, ProgramErrorV1,
    ProgramInputV1, TributeInputV1,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::CasObjectRefV1,
    input::{InputChunkKind, InputChunkRefV1},
    local_control::poc_schema_limits,
    ordered_list_root,
    result::OutputManifestEntryV1,
    unit::{InputPurpose, InputSourceKind, UnitInterval, UnitPhase},
    ListKind, ObjectKind, OrderedListLimits,
};
use outbe_primitives::time::WorldwideDay;
use std::collections::BTreeMap;

const SIX_DECIMAL_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

fn tribute(seed: u32, day: WorldwideDay, nominal: u64, excluded: bool) -> TributeInputV1 {
    let mut owner_bytes = [0_u8; 20];
    owner_bytes[16..].copy_from_slice(&seed.to_be_bytes());
    let owner = Address::from(owner_bytes);
    TributeInputV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_currency: 840,
        nominal_amount_minor: U256::from(nominal),
        reference_currency: 978,
        tribute_price_minor: U256::from(2),
        exclude_from_intex_issuance: excluded,
    }
}

fn observed(
    seed: u32,
    day: WorldwideDay,
    nominal: u64,
    league: u16,
    excluded: bool,
) -> ObservedTributeV1 {
    ObservedTributeV1 {
        tribute: tribute(seed, day, nominal, excluded),
        first_league: ObservationValueV1::Value(league),
        second_league: ObservationValueV1::Value(league),
        entry_price_minor: ObservationValueV1::Value(U256::from(3)),
        nod_target_available: true,
    }
}

fn tribute_chunk_ref(ordinal: u32, ids: &[B256], encoded_bytes: u64) -> InputChunkRefV1 {
    InputChunkRefV1 {
        kind: InputChunkKind::Tribute,
        ordinal,
        record_count: u32::try_from(ids.len()).unwrap(),
        first_key: BoundedBytes(ids.first().unwrap().0.to_vec()),
        last_key_inclusive: BoundedBytes(ids.last().unwrap().0.to_vec()),
        encoded_bytes,
        semantic_digest: B256::repeat_byte(u8::try_from(ordinal + 10).unwrap()),
        transport_digest: B256::repeat_byte(u8::try_from(ordinal + 20).unwrap()),
    }
}

fn planner_bindings(tribute_count: u32) -> LysisPlannerBindingsV1 {
    LysisPlannerBindingsV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 3,
        input_manifest_hash: B256::repeat_byte(4),
        input_manifest_encoded_bytes: 512,
        fidelity_opening_root: B256::repeat_byte(6),
        oracle_opening_root: B256::repeat_byte(7),
        wwd: 20_260_724,
        lysis_limit_minor: U256::from(99_000_000_u64),
        logical_evaluation_time: 1_784_765_900,
        tribute_count,
        lysis_program_semantics_hash: B256::repeat_byte(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    }
}

fn entity_id(ordinal: u32) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[28..].copy_from_slice(&ordinal.to_be_bytes());
    B256::from(bytes)
}

mod carriers;

mod planner_units;

mod topology;

mod amounts;

mod prefixes;
