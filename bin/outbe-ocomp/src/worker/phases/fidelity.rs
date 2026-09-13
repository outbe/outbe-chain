use super::super::exact_unit_output_source;
use super::super::planner_from_authority;
use super::super::require_authenticated_input;
use super::super::require_lease_active;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::input_artifacts::decode_fidelity_subject_key;

use alloy_primitives::B256;
use alloy_primitives::U256;

use outbe_lysis::program_v1::artifacts::decode_enumerated_run;
use outbe_lysis::program_v1::artifacts::decode_fidelity_map_output;

use outbe_lysis::program_v1::artifacts::decode_fixed_reduce_output;

use outbe_lysis::program_v1::artifacts::encode_fidelity_map_output;
use outbe_lysis::program_v1::artifacts::encode_fixed_reduce_output;

use outbe_lysis::program_v1::artifacts::FixedReduceOutputV1;

use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;
use outbe_lysis::program_v1::artifacts::RawCoverageCarrierV1;

use outbe_lysis::program_v1::phases::fidelity_map;
use outbe_lysis::program_v1::phases::fidelity_reduce_pair;
use outbe_lysis::program_v1::phases::finalize_fi_fraction_table;

use outbe_lysis::program_v1::phases::FidelityReduceValueV1;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;

use outbe_lysis::program_v1::ObservationValueV1;
use outbe_lysis::program_v1::ObservedTributeV1;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::input::AuthenticatedOpeningV1;
use outbe_ocomp_protocol::input::InputChunkKind;

use outbe_ocomp_protocol::input::OpeningSourceKind;
use outbe_ocomp_protocol::league_snapshot::league_snapshot_slot;

use outbe_ocomp_protocol::unit::BinaryReducerNode;

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

use std::collections::BTreeMap;

pub(in super::super) fn execute_fidelity_map_unit(
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
    let shard_ordinal = unit_index
        .checked_sub(plan.primary_work_unit_count)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count || producer_artifacts.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    if planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }

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
    let producer = &producer_artifacts[0];
    producer.validate_against(&primary_spec, limits)?;
    if producer.unit_id != enumerate_unit_id || producer.phase != UnitPhase::Enumerate {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerated = decode_enumerated_run(producer.phase_payload(limits)?, limits)?;
    let producer_header = producer.output_header(limits)?;
    if enumerated.coverage_root()? != producer_header.output_coverage_root
        || enumerated.ordered_records.len()
            != usize::try_from(producer_header.output_coverage_count)
                .map_err(|_| WorkerError::UnitBindingMismatch)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let mut leagues = BTreeMap::new();
    let mut fidelity_opening_count = 0_u32;
    let mut fidelity_encoded_bytes = 0_u64;
    for (reference, chunk) in input_chunks {
        require_lease_active(cancelled)?;
        match chunk.kind {
            InputChunkKind::Tribute => {}
            InputChunkKind::Fidelity => {
                fidelity_encoded_bytes = fidelity_encoded_bytes
                    .checked_add(reference.encoded_bytes)
                    .ok_or(WorkerError::UnitBindingMismatch)?;
                for encoded in &chunk.canonical_records_or_openings {
                    require_lease_active(cancelled)?;
                    fidelity_opening_count = fidelity_opening_count
                        .checked_add(1)
                        .ok_or(WorkerError::UnitBindingMismatch)?;
                    let opening =
                        AuthenticatedOpeningV1::decode_canonical_record(&encoded.0, limits)?;
                    if opening.source_kind != OpeningSourceKind::Fidelity {
                        return Err(WorkerError::UnitBindingMismatch);
                    }
                    opening.validate_against_bundle(bundle, limits)?;
                    let raw = opening.decode_and_validate_raw_opening(
                        manifest.checkpoint.finalized_state_root,
                        limits,
                    )?;
                    let owners = decode_fidelity_subject_key(&opening.canonical_subject_key.0)?;
                    let slot_values = raw
                        .ordered_slots
                        .iter()
                        .map(|slot| (slot.slot, slot.value))
                        .collect::<BTreeMap<_, _>>();
                    for owner in &owners {
                        require_lease_active(cancelled)?;
                        // Independently re-derive each owner's snapshot slot rather
                        // than trusting slot order; the MPT-proven value is the
                        // on-chain league Metadosis committed for this day at
                        // prepare time. An absent (zero) or out-of-range word means
                        // the owner was not snapshotted and is rejected.
                        let slot = league_snapshot_slot(manifest.wwd, *owner);
                        let word = slot_values
                            .get(&slot)
                            .copied()
                            .ok_or(WorkerError::UnitBindingMismatch)?;
                        if word.is_zero() || word > U256::from(u16::MAX) {
                            return Err(WorkerError::UnitBindingMismatch);
                        }
                        if leagues.insert(*owner, word.to::<u16>()).is_some() {
                            return Err(WorkerError::UnitBindingMismatch);
                        }
                    }
                }
            }
            InputChunkKind::Oracle => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    require_authenticated_input(
        spec,
        InputPurpose::FidelityOpenings,
        manifest.fidelity_opening_root,
        fidelity_opening_count,
        fidelity_encoded_bytes,
    )?;

    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for record in &enumerated.ordered_records {
        require_lease_active(cancelled)?;
        let league = leagues
            .get(&record.tribute.owner)
            .copied()
            .ok_or(WorkerError::UnitBindingMismatch)?;
        observed.push(ObservedTributeV1 {
            tribute: record.tribute.clone(),
            first_league: ObservationValueV1::Value(league),
            second_league: ObservationValueV1::Value(league),
            conditional_entry_price_minor: ObservationValueV1::Unavailable,
            nod_target_available: true,
        });
    }
    let output =
        fidelity_map(enumerated.start_ordinal, &observed).map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = output.coverage_root()?;
    if output_coverage_root != producer_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: producer_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: producer_header.output_coverage_count,
            output_coverage_count: output.aggregate.tribute_count,
        },
        BoundedBytes(encode_fidelity_map_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

struct FixedReduceInputV1 {
    value: FidelityReduceValueV1,
    coverage: RawCoverageCarrierV1,
}

pub(in super::super) fn execute_fixed_reduce_unit(
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
    if !input_chunks.is_empty() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let fixed_reduce_offset = plan
        .primary_work_unit_count
        .checked_mul(2)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let phase_ordinal = unit_index
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
    let reducer_inputs = spec
        .canonical_ordered_inputs
        .iter()
        .filter(|input| input.purpose == InputPurpose::FidelityPartials)
        .collect::<Vec<_>>();
    if reducer_inputs.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let producer_ids = reducer_inputs
        .iter()
        .map(|input| match input.source_kind {
            InputSourceKind::UnitOutput if !input.source_id.is_zero() => Ok(Some(input.source_id)),
            InputSourceKind::CanonicalEmpty => Ok(None),
            _ => Err(WorkerError::UnitBindingMismatch),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let producer_ids: [Option<B256>; 2] = producer_ids
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    if planner.fixed_reduce_unit_at(phase_ordinal, producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let expected_producers = topology.required_producers(position)?;
    let mut artifacts = producer_artifacts.iter();
    let mut decoded_inputs = Vec::with_capacity(2);
    for (expected, input) in expected_producers.into_iter().zip(reducer_inputs) {
        require_lease_active(cancelled)?;
        match expected {
            PlannedProducerV1::CanonicalEmpty {
                purpose: InputPurpose::FidelityPartials,
                padded_ordinal,
            } => {
                if input.source_kind != InputSourceKind::CanonicalEmpty {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                decoded_inputs.push(FixedReduceInputV1 {
                    value: FidelityReduceValueV1::Empty,
                    coverage: RawCoverageCarrierV1::canonical_empty(
                        plan.tribute_count,
                        padded_ordinal,
                    )?,
                });
            }
            PlannedProducerV1::Unit(producer_position) => {
                if input.source_kind != InputSourceKind::UnitOutput {
                    return Err(WorkerError::UnitBindingMismatch);
                }
                let artifact = artifacts.next().ok_or(WorkerError::UnitBindingMismatch)?;
                decoded_inputs.push(decode_fixed_reduce_producer(
                    artifact,
                    input.source_id,
                    producer_position,
                    spec,
                    plan,
                    limits,
                )?);
            }
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    if artifacts.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let [left, right]: [FixedReduceInputV1; 2] = decoded_inputs
        .try_into()
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    let single_primary_root = plan.primary_work_unit_count == 1 && level == 1 && index == 0;
    let (value, coverage) = if single_primary_root {
        if !matches!(&right.value, FidelityReduceValueV1::Empty)
            || !matches!(&left.value, FidelityReduceValueV1::Aggregate(_))
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        (left.value, left.coverage)
    } else {
        (
            fidelity_reduce_pair(left.value, right.value).map_err(LysisArtifactErrorV1::from)?,
            RawCoverageCarrierV1::merge(&left.coverage, &right.coverage)?,
        )
    };
    let is_root = level == topology.tree().height() && index == 0;
    let aggregate = match value {
        FidelityReduceValueV1::Empty => None,
        FidelityReduceValueV1::Aggregate(aggregate) => Some(aggregate),
    };
    let ordered_fractions = if is_root {
        finalize_fi_fraction_table(
            aggregate.as_ref().ok_or(WorkerError::UnitBindingMismatch)?,
            plan.lysis_budget,
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

fn decode_fixed_reduce_producer(
    artifact: &UnitArtifactV1,
    expected_unit_id: B256,
    position: PlannedUnitPositionV1,
    consumer_spec: &UnitSpecV1,
    plan: &PlanCommitmentV1,
    limits: &SchemaLimits,
) -> Result<FixedReduceInputV1, WorkerError> {
    artifact.validate_semantics(limits)?;
    if artifact.unit_id != expected_unit_id
        || artifact.protocol_bundle_hash != consumer_spec.protocol_bundle_hash
        || artifact.job_id != consumer_spec.job_id
        || artifact.attempt != consumer_spec.attempt
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (phase, interval) = match position {
        PlannedUnitPositionV1::Primary {
            phase: UnitPhase::FidelityMap,
            ordinal,
        } => {
            let start = ordinal
                .checked_mul(PRIMARY_WORK_SHARD_SIZE)
                .ok_or(WorkerError::UnitBindingMismatch)?;
            let end = start
                .saturating_add(PRIMARY_WORK_SHARD_SIZE)
                .min(plan.tribute_count);
            (
                UnitPhase::FidelityMap,
                UnitInterval::FidelityIndexRange(FidelityIndexHalfOpenRange { start, end }),
            )
        }
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level,
            index,
        } => (
            UnitPhase::FixedReduce,
            UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
        ),
        _ => return Err(WorkerError::UnitBindingMismatch),
    };
    let mut interval_binding = consumer_spec.clone();
    interval_binding.phase = phase;
    interval_binding.interval = interval;
    if artifact.phase != phase
        || artifact.interval_commitment != interval_binding.interval_commitment(limits)?
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
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
            let coverage = RawCoverageCarrierV1::from_records(plan.tribute_count, &records)?;
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
