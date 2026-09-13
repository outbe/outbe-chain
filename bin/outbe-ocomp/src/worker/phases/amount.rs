use super::super::exact_unit_output_source;
use super::super::planner_from_authority;
use super::super::require_authenticated_input;
use super::super::require_lease_active;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::input_artifacts::decode_oracle_subject_key;

use outbe_lysis::program_v1::artifacts::decode_enumerated_run;
use outbe_lysis::program_v1::artifacts::decode_fidelity_map_output;

use outbe_lysis::program_v1::artifacts::decode_fixed_reduce_output;

use outbe_lysis::program_v1::artifacts::encode_amount_run;

use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;

use outbe_lysis::program_v1::phases::amount_map;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::ObservationValueV1;
use outbe_lysis::program_v1::ObservedTributeV1;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::input::AuthenticatedOpeningV1;
use outbe_ocomp_protocol::input::InputChunkKind;

use outbe_ocomp_protocol::input::OpeningSourceKind;

use outbe_ocomp_protocol::unit::BinaryReducerNode;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitInterval;
use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

use outbe_oracle::evaluate_oracle_opening_v1;

use outbe_primitives::time::WorldwideDay;

pub(in super::super) fn execute_amount_map_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        unit_index,
        manifest,
        input_chunks,
        producer_artifacts,
        bundle,
        limits,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    if producer_artifacts.len() != 3 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let amount_offset = plan
        .primary_work_unit_count
        .checked_mul(2)
        .and_then(|offset| offset.checked_add(topology.phase_unit_count(UnitPhase::FixedReduce)))
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let shard_ordinal = unit_index
        .checked_sub(amount_offset)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let fidelity_unit_id = exact_unit_output_source(spec, InputPurpose::FidelityPartials)?;
    let fraction_root_unit_id = exact_unit_output_source(spec, InputPurpose::FiFractionTable)?;

    let primary_spec = planner.primary_unit_at(
        shard_ordinal,
        |ordinal| {
            input_chunks
                .iter()
                .find(|(reference, chunk)| {
                    reference.kind == InputChunkKind::Tribute
                        && chunk.kind == InputChunkKind::Tribute
                        && reference.ordinal == ordinal
                })
                .map(|(reference, _)| reference.clone())
        },
        limits,
    )?;
    if primary_spec.unit_id(limits)? != enumerate_unit_id
        || planner.amount_map_unit_at(
            shard_ordinal,
            &primary_spec,
            fidelity_unit_id,
            fraction_root_unit_id,
            limits,
        )? != spec.clone()
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let enumerate_artifact = &producer_artifacts[0];
    enumerate_artifact.validate_against(&primary_spec, limits)?;
    let enumerated = decode_enumerated_run(enumerate_artifact.phase_payload(limits)?, limits)?;
    let enumerate_header = enumerate_artifact.output_header(limits)?;
    if enumerated.coverage_root()? != enumerate_header.output_coverage_root
        || enumerate_header.output_coverage_count
            != u32::try_from(enumerated.ordered_records.len())
                .map_err(|_| WorkerError::UnitBindingMismatch)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let fidelity_spec = planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)?;
    let fidelity_artifact = &producer_artifacts[1];
    fidelity_artifact.validate_against(&fidelity_spec, limits)?;
    if fidelity_artifact.unit_id != fidelity_unit_id {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let fidelity = decode_fidelity_map_output(fidelity_artifact.phase_payload(limits)?, limits)?;
    let fidelity_header = fidelity_artifact.output_header(limits)?;
    if fidelity.coverage_root()? != fidelity_header.output_coverage_root
        || fidelity_header.output_coverage_root != enumerate_header.output_coverage_root
        || fidelity.aggregate.tribute_count != fidelity_header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let root_artifact = &producer_artifacts[2];
    root_artifact.validate_semantics(limits)?;
    let root_interval = UnitInterval::BinaryReducerNode(BinaryReducerNode {
        level: topology.tree().height(),
        index: 0,
    });
    let mut root_interval_binding = spec.clone();
    root_interval_binding.phase = UnitPhase::FixedReduce;
    root_interval_binding.interval = root_interval;
    if root_artifact.unit_id != fraction_root_unit_id
        || root_artifact.protocol_bundle_hash != spec.protocol_bundle_hash
        || root_artifact.job_id != spec.job_id
        || root_artifact.attempt != spec.attempt
        || root_artifact.phase != UnitPhase::FixedReduce
        || root_artifact.interval_commitment != root_interval_binding.interval_commitment(limits)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root_output = decode_fixed_reduce_output(root_artifact.phase_payload(limits)?, limits)?;
    let root_header = root_artifact.output_header(limits)?;
    let root_aggregate = root_output
        .aggregate
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if root_output.ordered_fractions.is_empty()
        || root_aggregate.tribute_count != plan.tribute_count
        || root_output.coverage.final_root(plan.tribute_count)? != root_header.output_coverage_root
        || root_header.output_coverage_count != plan.tribute_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let oracle_chunks = input_chunks
        .iter()
        .filter(|(_, chunk)| chunk.kind == InputChunkKind::Oracle)
        .collect::<Vec<_>>();
    if oracle_chunks.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (oracle_reference, oracle_chunk) = oracle_chunks[0];
    if oracle_chunk.canonical_records_or_openings.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        spec,
        InputPurpose::OracleOpenings,
        manifest.oracle_opening_root,
        1,
        oracle_reference.encoded_bytes,
    )?;
    let opening = AuthenticatedOpeningV1::decode_canonical_record(
        &oracle_chunk.canonical_records_or_openings[0].0,
        limits,
    )?;
    if opening.source_kind != OpeningSourceKind::Oracle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    opening.validate_against_bundle(bundle, limits)?;
    let (oracle_wwd, reference_isos) = decode_oracle_subject_key(&opening.canonical_subject_key.0)?;
    if oracle_wwd != manifest.wwd {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let raw = opening
        .decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
    let raw_slots = raw
        .ordered_slots
        .iter()
        .map(|slot| (slot.slot, slot.value))
        .collect::<Vec<_>>();
    let oracle =
        evaluate_oracle_opening_v1(WorldwideDay::new(manifest.wwd), &reference_isos, &raw_slots)?;
    let mandatory_entry_price = oracle
        .entry_price(840)
        .ok_or(WorkerError::UnitBindingMismatch)?;

    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for (record, fidelity) in enumerated
        .ordered_records
        .iter()
        .zip(&fidelity.observations)
    {
        require_lease_active(cancelled)?;
        if record.raw_ordinal != fidelity.raw_ordinal
            || record.tribute.tribute_id != fidelity.tribute_id
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        observed.push(ObservedTributeV1 {
            tribute: record.tribute.clone(),
            first_league: ObservationValueV1::Value(fidelity.pre_distribution_league),
            second_league: ObservationValueV1::Value(fidelity.issuance_league),
            conditional_entry_price_minor: oracle
                .entry_price(record.tribute.reference_currency)
                .map_or(ObservationValueV1::Unavailable, ObservationValueV1::Value),
            nod_target_available: true,
        });
    }
    let amount = amount_map(
        enumerated.start_ordinal,
        &observed,
        &fidelity.observations,
        &root_output.ordered_fractions,
        mandatory_entry_price,
    )
    .map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = amount.coverage_root()?;
    if output_coverage_root != enumerate_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output_count = u32::try_from(amount.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: enumerate_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: enumerate_header.output_coverage_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_amount_run(&amount, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}
