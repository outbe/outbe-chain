use super::super::super::planner_from_authority;
use super::super::super::require_lease_active;
use super::super::super::UnitExecutionAuthority;
use super::super::super::WorkerError;

use alloy_primitives::B256;

use outbe_lysis::program_v1::artifacts::decode_fidelity_map_output;
use outbe_lysis::program_v1::artifacts::decode_fixed_reduce_output;
use outbe_lysis::program_v1::artifacts::encode_fixed_reduce_output;
use outbe_lysis::program_v1::artifacts::FixedReduceOutputV1;
use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;
use outbe_lysis::program_v1::artifacts::RawCoverageCarrierV1;
use outbe_lysis::program_v1::phases::fidelity_reduce_pair;
use outbe_lysis::program_v1::phases::finalize_fi_fraction_table;
use outbe_lysis::program_v1::phases::FidelityReduceValueV1;
use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;
use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;
use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;

use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::unit::BinaryReducerNode;
use outbe_ocomp_protocol::unit::CanonicalInputRefV1;
use outbe_ocomp_protocol::unit::FidelityIndexHalfOpenRange;
use outbe_ocomp_protocol::unit::InputPurpose;
use outbe_ocomp_protocol::unit::InputSourceKind;
use outbe_ocomp_protocol::unit::PlanCommitmentV1;
use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitInterval;
use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;
use outbe_ocomp_protocol::SchemaLimits;

struct FixedReduceInputV1 {
    value: FidelityReduceValueV1,
    coverage: RawCoverageCarrierV1,
}

pub(in super::super::super) fn execute_fixed_reduce_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        manifest,
        input_chunks,
        bundle,
        limits,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let (phase_ordinal, position, level, index) =
        resolve_fixed_reduce_position(&authority, &topology)?;
    let reducer_inputs = validate_fixed_reduce_spec(spec, &planner, phase_ordinal, limits)?;
    let [left, right] = decode_fixed_reduce_inputs(
        spec,
        &authority,
        &topology,
        position,
        reducer_inputs,
    )?;
    let (value, coverage) =
        reduce_fixed_pair(plan.primary_work_unit_count, (level, index), left, right)?;
    let is_root = level == topology.tree().height() && index == 0;
    let aggregate = match value {
        FidelityReduceValueV1::Empty => None,
        FidelityReduceValueV1::Aggregate(aggregate) => Some(aggregate),
    };
    let ordered_fractions = if is_root {
        finalize_fi_fraction_table(
            aggregate.as_ref().ok_or(WorkerError::UnitBindingMismatch)?,
            plan.lysis_limit_minor,
        )
        .map_err(LysisArtifactErrorV1::from)?
    } else {
        Vec::new()
    };
    let output_count = aggregate
        .as_ref()
        .map_or(0, |aggregate| aggregate.tribute_count);
    let coverage_root = if is_root {
        coverage.final_root(plan.tribute_count)?
    } else {
        coverage.tree_root
    };
    let output = FixedReduceOutputV1 {
        aggregate,
        coverage,
        ordered_fractions,
    };
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: output_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_fixed_reduce_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn resolve_fixed_reduce_position(
    authority: &UnitExecutionAuthority<'_>,
    topology: &LysisPlanTopologyV1,
) -> Result<(u32, PlannedUnitPositionV1, u16, u32), WorkerError> {
    let fixed_reduce_offset = authority
        .plan
        .primary_work_unit_count
        .checked_mul(2)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let phase_ordinal = authority
        .unit_index
        .checked_sub(fixed_reduce_offset)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::FixedReduce, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::FixedReduce,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    Ok((phase_ordinal, position, level, index))
}

fn validate_fixed_reduce_spec<'a>(
    spec: &'a UnitSpecV1,
    planner: &outbe_lysis::program_v1::planner::LysisPlannerV1,
    phase_ordinal: u32,
    limits: &SchemaLimits,
) -> Result<[&'a CanonicalInputRefV1; 2], WorkerError> {
    let reducer_inputs = spec
        .canonical_ordered_inputs
        .iter()
        .filter(|input| input.purpose == InputPurpose::FidelityPartials)
        .collect::<Vec<_>>();
    let [left_input, right_input]: [&CanonicalInputRefV1; 2] = reducer_inputs
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    let producer_ids = [
        fixed_reduce_source_id(left_input)?,
        fixed_reduce_source_id(right_input)?,
    ];
    if planner.fixed_reduce_unit_at(phase_ordinal, producer_ids, limits)? != *spec {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok([left_input, right_input])
}

fn fixed_reduce_source_id(input: &CanonicalInputRefV1) -> Result<Option<B256>, WorkerError> {
    match input.source_kind {
        InputSourceKind::UnitOutput if !input.source_id.is_zero() => Ok(Some(input.source_id)),
        InputSourceKind::CanonicalEmpty => Ok(None),
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}

fn decode_fixed_reduce_inputs(
    spec: &UnitSpecV1,
    authority: &UnitExecutionAuthority<'_>,
    topology: &LysisPlanTopologyV1,
    position: PlannedUnitPositionV1,
    reducer_inputs: [&CanonicalInputRefV1; 2],
) -> Result<[FixedReduceInputV1; 2], WorkerError> {
    let expected_producers = topology.required_producers(position)?;
    let mut artifacts = authority.producer_artifacts.iter();
    let mut decoded_inputs = Vec::with_capacity(2);
    for (expected, input) in expected_producers.into_iter().zip(reducer_inputs) {
        require_lease_active(authority.cancelled)?;
        match expected {
            PlannedProducerV1::CanonicalEmpty {
                purpose: InputPurpose::FidelityPartials,
                padded_ordinal,
            } if input.source_kind == InputSourceKind::CanonicalEmpty => {
                decoded_inputs.push(FixedReduceInputV1 {
                    value: FidelityReduceValueV1::Empty,
                    coverage: RawCoverageCarrierV1::canonical_empty(
                        authority.plan.tribute_count,
                        padded_ordinal,
                    )?,
                });
            }
            PlannedProducerV1::Unit(producer_position)
                if input.source_kind == InputSourceKind::UnitOutput =>
            {
                let artifact = artifacts.next().ok_or(WorkerError::UnitBindingMismatch)?;
                decoded_inputs.push(decode_fixed_reduce_producer(
                    artifact,
                    input.source_id,
                    producer_position,
                    (spec, authority.plan, authority.limits),
                )?);
            }
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    if artifacts.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    decoded_inputs
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)
}

fn reduce_fixed_pair(
    primary_work_unit_count: u32,
    (level, index): (u16, u32),
    left: FixedReduceInputV1,
    right: FixedReduceInputV1,
) -> Result<(FidelityReduceValueV1, RawCoverageCarrierV1), WorkerError> {
    let single_primary_root = primary_work_unit_count == 1 && level == 1 && index == 0;
    if single_primary_root {
        if !matches!(&right.value, FidelityReduceValueV1::Empty)
            || !matches!(&left.value, FidelityReduceValueV1::Aggregate(_))
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        Ok((left.value, left.coverage))
    } else {
        Ok((
            fidelity_reduce_pair(left.value, right.value).map_err(LysisArtifactErrorV1::from)?,
            RawCoverageCarrierV1::merge(&left.coverage, &right.coverage)?,
        ))
    }
}

fn decode_fixed_reduce_producer(
    artifact: &UnitArtifactV1,
    expected_unit_id: B256,
    position: PlannedUnitPositionV1,
    (consumer_spec, plan, limits): (&UnitSpecV1, &PlanCommitmentV1, &SchemaLimits),
) -> Result<FixedReduceInputV1, WorkerError> {
    artifact.validate_semantics(limits)?;
    let (phase, interval) = expected_producer_interval(position, plan.tribute_count)?;
    let mut interval_binding = consumer_spec.clone();
    interval_binding.phase = phase;
    interval_binding.interval = interval;
    let expected_binding = (
        expected_unit_id,
        consumer_spec.protocol_bundle_hash,
        consumer_spec.job_id,
        consumer_spec.attempt,
        phase,
        interval_binding.interval_commitment(limits)?,
    );
    let actual_binding = (
        artifact.unit_id,
        artifact.protocol_bundle_hash,
        artifact.job_id,
        artifact.attempt,
        artifact.phase,
        artifact.interval_commitment,
    );
    if actual_binding != expected_binding {
        return Err(WorkerError::UnitBindingMismatch);
    }
    decode_fixed_reduce_payload(artifact, phase, plan.tribute_count, limits)
}

fn expected_producer_interval(
    position: PlannedUnitPositionV1,
    tribute_count: u32,
) -> Result<(UnitPhase, UnitInterval), WorkerError> {
    match position {
        PlannedUnitPositionV1::Primary {
            phase: UnitPhase::FidelityMap,
            ordinal,
        } => {
            let start = ordinal
                .checked_mul(PRIMARY_WORK_SHARD_SIZE)
                .ok_or(WorkerError::UnitBindingMismatch)?;
            let end = start
                .saturating_add(PRIMARY_WORK_SHARD_SIZE)
                .min(tribute_count);
            Ok((
                UnitPhase::FidelityMap,
                UnitInterval::FidelityIndexRange(FidelityIndexHalfOpenRange { start, end }),
            ))
        }
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level,
            index,
        } => Ok((
            UnitPhase::FixedReduce,
            UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
        )),
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}

fn decode_fixed_reduce_payload(
    artifact: &UnitArtifactV1,
    phase: UnitPhase,
    tribute_count: u32,
    limits: &SchemaLimits,
) -> Result<FixedReduceInputV1, WorkerError> {
    let header = artifact.output_header(limits)?;
    match phase {
        UnitPhase::FidelityMap => {
            let output = decode_fidelity_map_output(artifact.phase_payload(limits)?, limits)?;
            if output.coverage_root()? != header.output_coverage_root
                || output.aggregate.tribute_count != header.output_coverage_count
            {
                return Err(WorkerError::UnitBindingMismatch);
            }
            let records = output
                .observations
                .iter()
                .map(|observation| (observation.raw_ordinal, observation.tribute_id))
                .collect::<Vec<_>>();
            let coverage = RawCoverageCarrierV1::from_records(tribute_count, &records)?;
            Ok(FixedReduceInputV1 {
                value: FidelityReduceValueV1::Aggregate(output.aggregate),
                coverage,
            })
        }
        UnitPhase::FixedReduce => {
            let output = decode_fixed_reduce_output(artifact.phase_payload(limits)?, limits)?;
            let output_count = output
                .aggregate
                .as_ref()
                .map_or(0, |aggregate| aggregate.tribute_count);
            if !output.ordered_fractions.is_empty()
                || output.coverage.tree_root != header.output_coverage_root
                || output_count != header.output_coverage_count
            {
                return Err(WorkerError::UnitBindingMismatch);
            }
            Ok(FixedReduceInputV1 {
                value: output.aggregate.map_or(
                    FidelityReduceValueV1::Empty,
                    FidelityReduceValueV1::Aggregate,
                ),
                coverage: output.coverage,
            })
        }
        _ => Err(WorkerError::UnitBindingMismatch),
    }
}
