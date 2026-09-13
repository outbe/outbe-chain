use super::super::planner_from_authority;
use super::super::require_lease_active;
use super::super::resolve_scan_artifacts;
use super::super::scan_producer_inputs;
use super::super::unit_or_empty_id;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use outbe_lysis::program_v1::artifacts::decode_amount_run;

use outbe_lysis::program_v1::artifacts::decode_gratis_prefix_down_output;
use outbe_lysis::program_v1::artifacts::decode_gratis_segment_summary;

use outbe_lysis::program_v1::artifacts::encode_gratis_prefix_down_output;
use outbe_lysis::program_v1::artifacts::encode_gratis_segment_summary;

use outbe_lysis::program_v1::artifacts::gratis_summary_coverage;

use outbe_lysis::program_v1::artifacts::GratisPrefixDownOutputV1;
use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;

use outbe_lysis::program_v1::phases::gratis_prefix_down;
use outbe_lysis::program_v1::phases::gratis_summary;
use outbe_lysis::program_v1::phases::gratis_summary_reduce_pair;

use outbe_lysis::program_v1::phases::GratisLeafPrefixV1;
use outbe_lysis::program_v1::phases::GratisSummaryValueV1;
use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::UnitArtifactV1;

use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

pub(in super::super) fn execute_gratis_prefix_unit(
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
    let phase_ordinal = unit_index
        .checked_sub(topology.phase_offset(UnitPhase::GratisPrefix)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::GratisPrefix, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefix,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let purpose = if level == 0 {
        InputPurpose::AmountRecords
    } else {
        InputPurpose::GratisPrefixTable
    };
    let producer_inputs = scan_producer_inputs(spec, purpose)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    if planner.gratis_prefix_unit_at(phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;

    let (summary, coverage_root, coverage_count) = if level == 0 {
        let amount_artifact = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        if amount_artifact.phase != UnitPhase::AmountMap {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let amount = decode_amount_run(amount_artifact.phase_payload(limits)?, limits)?;
        let amount_header = amount_artifact.output_header(limits)?;
        let expected_start = index
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        if amount.start_ordinal != expected_start
            || amount.end_ordinal > plan.tribute_count
            || amount.coverage_root()? != amount_header.output_coverage_root
            || amount_header.output_coverage_count
                != u32::try_from(amount.ordered_records.len())
                    .map_err(|_| WorkerError::UnitBindingMismatch)?
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let summary = gratis_summary(
            amount.start_ordinal,
            &amount
                .ordered_records
                .iter()
                .map(|record| record.gratis_load_minor)
                .collect::<Vec<_>>(),
        )
        .map_err(LysisArtifactErrorV1::from)?;
        (
            summary,
            amount_header.output_coverage_root,
            amount_header.output_coverage_count,
        )
    } else {
        let mut values = [GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty];
        let mut child_coverage = [None, None];
        for (child_index, artifact) in resolved.into_iter().enumerate() {
            require_lease_active(cancelled)?;
            let Some(artifact) = artifact else {
                continue;
            };
            let summary = decode_gratis_segment_summary(artifact.phase_payload(limits)?, limits)?;
            let header = artifact.output_header(limits)?;
            if summary.end_ordinal - summary.start_ordinal != header.output_coverage_count {
                return Err(WorkerError::UnitBindingMismatch);
            }
            values[child_index] = GratisSummaryValueV1::Summary(summary);
            child_coverage[child_index] =
                Some((header.output_coverage_root, header.output_coverage_count));
        }
        let summary = match gratis_summary_reduce_pair(values[0].clone(), values[1].clone())
            .map_err(LysisArtifactErrorV1::from)?
        {
            GratisSummaryValueV1::Summary(summary) => summary,
            GratisSummaryValueV1::Empty => return Err(WorkerError::UnitBindingMismatch),
        };
        let coverage = gratis_summary_coverage(spec.interval_commitment(limits)?, child_coverage)?;
        if coverage.count != summary.end_ordinal - summary.start_ordinal {
            return Err(WorkerError::UnitBindingMismatch);
        }
        (summary, coverage.root, coverage.count)
    };

    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: coverage_count,
            output_coverage_count: coverage_count,
        },
        BoundedBytes(encode_gratis_segment_summary(&summary, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

pub(in super::super) fn execute_gratis_prefix_down_unit(
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
    let phase_ordinal = unit_index
        .checked_sub(topology.phase_offset(UnitPhase::GratisPrefixDown)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::GratisPrefixDown, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefixDown,
        level,
        index,
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let producer_inputs = scan_producer_inputs(spec, InputPurpose::GratisPrefixTable)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    if planner.gratis_prefix_down_unit_at(phase_ordinal, &producer_ids, limits)? != spec.clone() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        limits,
    )?;

    if level == 0 {
        let parent = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let summary_artifact = resolved
            .get(1)
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let incoming = children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        let summary =
            decode_gratis_segment_summary(summary_artifact.phase_payload(limits)?, limits)?;
        let summary_header = summary_artifact.output_header(limits)?;
        if incoming.start_ordinal != summary.start_ordinal
            || incoming.end_ordinal != summary.end_ordinal
            || summary.end_ordinal - summary.start_ordinal != summary_header.output_coverage_count
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let incoming_remaining = incoming
            .incoming_remaining
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let outgoing_remaining = incoming_remaining
            .checked_sub(summary.checked_segment_gratis_total)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let output = GratisPrefixDownOutputV1::Leaf(GratisLeafPrefixV1 {
            segment_ordinal: index,
            incoming_remaining,
            outgoing_remaining,
            first_error_ordinal: None,
        });
        return UnitArtifactV1::from_canonical_output(
            spec,
            WorkOutputHeaderV1 {
                source_coverage_root: summary_header.output_coverage_root,
                output_coverage_root: summary_header.output_coverage_root,
                source_coverage_count: summary_header.output_coverage_count,
                output_coverage_count: summary_header.output_coverage_count,
            },
            BoundedBytes(encode_gratis_prefix_down_output(&output, limits)?),
            limits,
        )
        .map_err(WorkerError::from);
    }

    let is_root = level == topology.tree().height() && index == 0;
    let (incoming_remaining, child_start) = if is_root {
        (Some(plan.lysis_budget), 0)
    } else {
        let parent = resolved
            .first()
            .and_then(|artifact| *artifact)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let incoming = children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        (incoming.incoming_remaining, 1)
    };
    let mut values = [GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty];
    let mut child_coverage = [None, None];
    for child_index in 0..2 {
        require_lease_active(cancelled)?;
        let Some(artifact) = resolved
            .get(child_start + child_index)
            .and_then(|artifact| *artifact)
        else {
            continue;
        };
        let summary = decode_gratis_segment_summary(artifact.phase_payload(limits)?, limits)?;
        let header = artifact.output_header(limits)?;
        if summary.end_ordinal - summary.start_ordinal != header.output_coverage_count {
            return Err(WorkerError::UnitBindingMismatch);
        }
        values[child_index] = GratisSummaryValueV1::Summary(summary);
        child_coverage[child_index] =
            Some((header.output_coverage_root, header.output_coverage_count));
    }
    let combined = match gratis_summary_reduce_pair(values[0].clone(), values[1].clone())
        .map_err(LysisArtifactErrorV1::from)?
    {
        GratisSummaryValueV1::Summary(summary) => summary,
        GratisSummaryValueV1::Empty => return Err(WorkerError::UnitBindingMismatch),
    };
    let children = gratis_prefix_down(incoming_remaining, values[0].clone(), values[1].clone())
        .map_err(LysisArtifactErrorV1::from)?;
    if !is_root {
        let parent = resolved[0].ok_or(WorkerError::UnitBindingMismatch)?;
        let GratisPrefixDownOutputV1::Branch(parent_children) =
            decode_gratis_prefix_down_output(parent.phase_payload(limits)?, limits)?
        else {
            return Err(WorkerError::UnitBindingMismatch);
        };
        let assigned = parent_children
            [usize::try_from(index & 1).map_err(|_| WorkerError::UnitBindingMismatch)?]
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
        if assigned.start_ordinal != combined.start_ordinal
            || assigned.end_ordinal != combined.end_ordinal
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
    }
    let mut prefix_spec = spec.clone();
    prefix_spec.phase = UnitPhase::GratisPrefix;
    let coverage =
        gratis_summary_coverage(prefix_spec.interval_commitment(limits)?, child_coverage)?;
    if coverage.count != combined.end_ordinal - combined.start_ordinal {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output = GratisPrefixDownOutputV1::Branch(children);
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage.root,
            output_coverage_root: coverage.root,
            source_coverage_count: coverage.count,
            output_coverage_count: coverage.count,
        },
        BoundedBytes(encode_gratis_prefix_down_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}
