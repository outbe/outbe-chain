use super::super::planner_from_authority;
use super::super::require_lease_active;
use super::super::resolve_scan_artifacts;
use super::super::unit_or_empty_id;
use super::super::validate_scan_artifact;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;
use super::decode_shuffle_producer_root;
use super::tribute_nominal::shard_tribute_nominals;

use crate::inbox::WorkerInbox;

use alloy_primitives::B256;
use alloy_primitives::U256;

use outbe_lysis::program_v1::artifacts::decode_finalized_output_run;

use outbe_lysis::program_v1::phases::FinalizedOutputRunV1;
use outbe_lysis::program_v1::phases::NodBucketKeyV1;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;
use outbe_lysis::program_v1::result::decode_root_reduce_output;
use outbe_lysis::program_v1::result::encode_root_reduce_output;
use outbe_lysis::program_v1::result::LysisListSubtreeCarrierV1;
use outbe_lysis::program_v1::result::RootReduceOutputV1;
use outbe_lysis::program_v1::result::RootReduceSummaryV1;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::input::InputChunkKind;
use outbe_ocomp_protocol::input::InputManifestV1;

use outbe_ocomp_protocol::result::NodActionV1 as ProtocolNodActionV1;
use outbe_ocomp_protocol::result::OutputManifestEntryV1;
use outbe_ocomp_protocol::result::ResultChunkV1;

use outbe_ocomp_protocol::shuffle::ShuffleBucketRecordV1;

use outbe_ocomp_protocol::shuffle::ShuffleRunKindV1;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::PlanCommitmentV1;
use outbe_ocomp_protocol::unit::UnitArtifactV1;

use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

use outbe_ocomp_protocol::ListKind;

use outbe_ocomp_protocol::SchemaLimits;

use std::sync::atomic::AtomicBool;

pub(in super::super) fn execute_root_reduce_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        manifest,
        input_chunks,
        limits,
        inbox,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    let (phase_ordinal, level, expected, resolved) =
        resolve_root_reduce_producers(spec, &authority)?;
    let plan_hash = plan.plan_hash(limits)?;
    if level == 0 {
        return execute_root_reduce_leaf(
            spec,
            (plan, manifest, phase_ordinal, plan_hash),
            (&expected, &resolved),
            input_chunks,
            (inbox, limits, cancelled),
        );
    }
    execute_root_reduce_branch(
        spec,
        (plan, manifest, plan_hash),
        input_chunks,
        (expected, resolved),
        (limits, cancelled),
    )
}

type RootReduceProducers<'a> = (
    u32,
    u16,
    Vec<PlannedProducerV1>,
    Vec<Option<&'a UnitArtifactV1>>,
);

fn resolve_root_reduce_producers<'a>(
    spec: &UnitSpecV1,
    authority: &'a UnitExecutionAuthority<'a>,
) -> Result<RootReduceProducers<'a>, WorkerError> {
    let topology = LysisPlanTopologyV1::new(authority.plan.primary_work_unit_count)?;
    let phase_ordinal = authority
        .unit_index
        .checked_sub(topology.phase_offset(UnitPhase::RootReduce)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(UnitPhase::RootReduce, phase_ordinal)?;
    let PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::RootReduce,
        level,
        ..
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    if spec
        .canonical_ordered_inputs
        .first()
        .map(|input| input.purpose)
        != Some(InputPurpose::InputManifest)
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let producer_inputs = spec
        .canonical_ordered_inputs
        .iter()
        .skip(1)
        .collect::<Vec<_>>();
    let producer_ids = producer_inputs
        .iter()
        .map(|input| unit_or_empty_id(input))
        .collect::<Result<Vec<_>, _>>()?;
    let planner = planner_from_authority(
        authority.plan,
        authority.manifest,
        authority.bundle,
        authority.limits,
    )?;
    if planner.root_reduce_unit_at(phase_ordinal, &producer_ids, authority.limits)? != *spec {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        authority.producer_artifacts,
        authority.limits,
    )?;
    Ok((phase_ordinal, level, expected, resolved))
}

fn execute_root_reduce_branch(
    spec: &UnitSpecV1,
    (plan, manifest, plan_hash): (&PlanCommitmentV1, &InputManifestV1, B256),
    input_chunks: &[(
        outbe_ocomp_protocol::input::InputChunkRefV1,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    )],
    (expected, resolved): (Vec<PlannedProducerV1>, Vec<Option<&UnitArtifactV1>>),
    (limits, cancelled): (&SchemaLimits, Option<&AtomicBool>),
) -> Result<UnitArtifactV1, WorkerError> {
    if !input_chunks.is_empty() || resolved.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let mut summaries = Vec::with_capacity(2);
    for (producer, artifact) in expected.into_iter().zip(resolved) {
        require_lease_active(cancelled)?;
        match (producer, artifact) {
            (
                position @ PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    ..
                }),
                Some(artifact),
            ) => summaries.push(decode_root_reduce_producer(
                spec, position, artifact, plan_hash, limits,
            )?),
            (
                PlannedProducerV1::CanonicalEmpty {
                    purpose: InputPurpose::RootSummary,
                    padded_ordinal,
                },
                None,
            ) => summaries.push(empty_root_reduce_leaf(spec, plan_hash, padded_ordinal)?),
            _ => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    let right = summaries.pop().ok_or(WorkerError::UnitBindingMismatch)?;
    let left = summaries.pop().ok_or(WorkerError::UnitBindingMismatch)?;
    let summary = left.merge_adjacent(right)?;
    require_complete_root_summary(&summary, plan, manifest)?;
    let coverage_root = summary.result_chunk_hashes.tree_root;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: summary.covered_primary_count,
            output_coverage_count: summary.covered_primary_count,
        },
        BoundedBytes(encode_root_reduce_output(
            &RootReduceOutputV1::Node { summary },
            limits,
        )?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn execute_root_reduce_leaf(
    spec: &UnitSpecV1,
    (plan, manifest, shard_ordinal, plan_hash): (&PlanCommitmentV1, &InputManifestV1, u32, B256),
    (expected, resolved): (&[PlannedProducerV1], &[Option<&UnitArtifactV1>]),
    input_chunks: &[(
        outbe_ocomp_protocol::input::InputChunkRefV1,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    )],
    (inbox, limits, cancelled): (&WorkerInbox, &SchemaLimits, Option<&AtomicBool>),
) -> Result<UnitArtifactV1, WorkerError> {
    require_lease_active(cancelled)?;
    let (finalized, expected_start, tribute_count) = validate_root_reduce_leaf_producers(
        spec,
        (shard_ordinal, plan.tribute_count),
        (expected, resolved),
        limits,
    )?;
    validate_leaf_tribute_nominals(input_chunks, shard_ordinal, tribute_count, &finalized)?;
    let (summary, output_manifest_entry) = build_root_reduce_leaf_summary(
        spec,
        (shard_ordinal, expected_start, tribute_count, plan_hash),
        &finalized,
        (inbox, limits),
    )?;
    require_complete_root_summary(&summary, plan, manifest)?;
    let coverage_root = summary.result_chunk_hashes.tree_root;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: 1,
            output_coverage_count: 1,
        },
        BoundedBytes(encode_root_reduce_output(
            &RootReduceOutputV1::Leaf {
                summary,
                output_manifest_entry,
            },
            limits,
        )?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn validate_root_reduce_leaf_producers(
    spec: &UnitSpecV1,
    (shard_ordinal, plan_tribute_count): (u32, u32),
    (expected, resolved): (&[PlannedProducerV1], &[Option<&UnitArtifactV1>]),
    limits: &SchemaLimits,
) -> Result<(FinalizedOutputRunV1, u32, u32), WorkerError> {
    let [finalized_position @ PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
        phase: UnitPhase::OutputFinalize,
        ordinal,
    }), bucket_position @ PlannedProducerV1::Unit(PlannedUnitPositionV1::RunSpan {
        phase: UnitPhase::BucketShuffle,
        ..
    })] = expected
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let [Some(finalized_artifact), Some(bucket_artifact)] = resolved else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    if *ordinal != shard_ordinal {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let finalized = decode_finalized_output_run(finalized_artifact.phase_payload(limits)?, limits)?;
    let finalized_header = finalized_artifact.output_header(limits)?;
    let expected_start = shard_ordinal
        .checked_mul(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let expected_end = expected_start
        .checked_add(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(WorkerError::UnitBindingMismatch)?
        .min(plan_tribute_count);
    let tribute_count = expected_end
        .checked_sub(expected_start)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    require_root_reduce_finalized_binding(
        &finalized,
        finalized_header,
        expected_start,
        expected_end,
        tribute_count,
    )?;
    let PlannedProducerV1::Unit(position) = finalized_position else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    validate_scan_artifact(
        spec,
        *position,
        finalized_artifact.unit_id,
        finalized_artifact,
        limits,
    )?;
    let bucket_root = decode_shuffle_producer_root(
        spec,
        bucket_artifact,
        ShuffleRunKindV1::Bucket,
        *bucket_position,
        limits,
    )?;
    require_root_reduce_shuffle_population(
        bucket_root.source_coverage_count,
        bucket_root.record_count,
        plan_tribute_count,
    )?;
    Ok((finalized, expected_start, tribute_count))
}

fn validate_leaf_tribute_nominals(
    input_chunks: &[(
        outbe_ocomp_protocol::input::InputChunkRefV1,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    )],
    shard_ordinal: u32,
    tribute_count: u32,
    finalized: &FinalizedOutputRunV1,
) -> Result<(), WorkerError> {
    if input_chunks.iter().any(|(reference, chunk)| {
        reference.kind != InputChunkKind::Tribute || chunk.kind != InputChunkKind::Tribute
    }) {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let nominals = shard_tribute_nominals(input_chunks, shard_ordinal)?;
    let nominal_total = checked_sum(
        nominals.values().copied(),
        "root reducer tribute nominal total",
    )?;
    let expected_len =
        usize::try_from(tribute_count).map_err(|_| WorkerError::UnitBindingMismatch)?;
    if nominals.len() != expected_len || nominal_total != finalized.checked_tribute_nominal_total {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(())
}

fn build_root_reduce_leaf_summary(
    spec: &UnitSpecV1,
    (shard_ordinal, expected_start, tribute_count, plan_hash): (u32, u32, u32, B256),
    finalized: &FinalizedOutputRunV1,
    (inbox, limits): (&WorkerInbox, &SchemaLimits),
) -> Result<(RootReduceSummaryV1, OutputManifestEntryV1), WorkerError> {
    let nod_actions = finalized
        .ordered_records
        .iter()
        .map(|record| ProtocolNodActionV1 {
            raw_ordinal: record.raw_ordinal,
            tribute_id: *record.nod_action.source_tribute_id,
            nod_id: *record.nod_action.nod_id,
            owner: record.nod_action.owner,
            wwd: record.nod_action.worldwide_day.value(),
            league_id: record.nod_action.league_id,
            entry_price_minor: record.nod_action.entry_price_minor,
            issuance_currency: record.nod_action.issuance_currency,
            reference_currency: record.nod_action.reference_currency,
        })
        .collect::<Vec<_>>();
    let mut buckets = nod_actions
        .iter()
        .map(|action| ShuffleBucketRecordV1 {
            bucket_key: action.bucket_key(),
            raw_ordinal: action.raw_ordinal,
            tribute_id: action.tribute_id,
            nod_id: action.nod_id,
        })
        .collect::<Vec<_>>();
    buckets.sort_by_key(|record| (record.bucket_key, record.raw_ordinal));
    let chunk = ResultChunkV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        chunk_ordinal: shard_ordinal,
        first_nod_ordinal: expected_start,
        ordered_nod_actions: nod_actions.clone(),
    };
    let chunk_hash = chunk.result_chunk_hash(limits)?;
    let chunk_bytes = chunk.encode_canonical(limits)?;
    let chunk_ref = inbox.stage_result_chunk(&chunk_bytes, limits)?;
    let output_manifest_entry = OutputManifestEntryV1 {
        chunk_ordinal: shard_ordinal,
        result_chunk_hash: chunk_hash,
        result_chunk_ref: chunk_ref,
    };
    let nod_records = nod_actions
        .iter()
        .map(|action| action.encode_canonical_record(limits))
        .collect::<Result<Vec<_>, _>>()?;
    let bucket_records = buckets
        .iter()
        .map(|record| record.encode_canonical_record(limits))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest_records = vec![output_manifest_entry.encode_canonical_record(limits)?];
    let chunk_hash_records = vec![chunk_hash.as_slice().to_vec()];
    let summary = RootReduceSummaryV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        plan_hash,
        covered_primary_start: shard_ordinal,
        covered_primary_count: 1,
        nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::NodActions,
            shard_ordinal,
            &nod_records,
            limits.max_bounded_bytes,
        )?,
        bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::BucketRecords,
            shard_ordinal,
            &bucket_records,
            limits.max_bounded_bytes,
        )?,
        output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::CompleteOutputManifest,
            shard_ordinal,
            &manifest_records,
            limits.max_bounded_bytes,
        )?,
        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::ResultChunkHashes,
            shard_ordinal,
            &chunk_hash_records,
            B256::len_bytes(),
        )?,
        tribute_count,
        nod_count: tribute_count,
        bucket_count: tribute_count,
        tribute_nominal_total: finalized.checked_tribute_nominal_total,
        lysis_allocation_minor: finalized.checked_segment_gratis_total,
        nod_cost_total: finalized.checked_settlement_cost_total,
        first_error_ordinal: None,
    };
    Ok((summary, output_manifest_entry))
}

fn checked_sum(
    values: impl IntoIterator<Item = U256>,
    what: &'static str,
) -> Result<U256, WorkerError> {
    values.into_iter().try_fold(U256::ZERO, |total, value| {
        total
            .checked_add(value)
            .ok_or(outbe_ocomp_protocol::ProtocolError::IntegerOverflow { what }.into())
    })
}

pub(in super::super) fn require_root_reduce_finalized_binding(
    finalized: &FinalizedOutputRunV1,
    header: WorkOutputHeaderV1,
    expected_start: u32,
    expected_end: u32,
    expected_count: u32,
) -> Result<(), WorkerError> {
    let coverage_root = finalized.coverage_root()?;
    let actual = (
        finalized.start_ordinal,
        finalized.end_ordinal,
        u32::try_from(finalized.ordered_records.len()).ok(),
        header.source_coverage_root,
        header.output_coverage_root,
        header.source_coverage_count,
        header.output_coverage_count,
    );
    let expected = (
        expected_start,
        expected_end,
        Some(expected_count),
        coverage_root,
        coverage_root,
        expected_count,
        expected_count,
    );
    if actual != expected {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(())
}

pub(in super::super) fn require_root_reduce_shuffle_population(
    bucket_source_count: u32,
    bucket_record_count: u32,
    tribute_count: u32,
) -> Result<(), WorkerError> {
    if bucket_source_count != tribute_count || bucket_record_count != tribute_count {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(())
}

fn require_complete_root_summary(
    summary: &RootReduceSummaryV1,
    plan: &PlanCommitmentV1,
    manifest: &InputManifestV1,
) -> Result<(), WorkerError> {
    require_complete_root_values(
        (
            summary.covered_primary_start,
            summary.covered_primary_count,
            plan.primary_work_unit_count,
        ),
        (
            summary.tribute_count,
            plan.tribute_count,
            manifest.tribute_count,
        ),
        (
            summary.tribute_nominal_total,
            manifest.tribute_nominal_total,
        ),
    )
}

pub(in super::super) fn require_complete_root_values(
    (covered_primary_start, covered_primary_count, primary_work_unit_count): (u32, u32, u32),
    (summary_tribute_count, plan_tribute_count, manifest_tribute_count): (u32, u32, u32),
    (summary_tribute_nominal_total, manifest_tribute_nominal_total): (U256, U256),
) -> Result<(), WorkerError> {
    let is_complete_root =
        covered_primary_start == 0 && covered_primary_count == primary_work_unit_count;
    let totals_match = (
        summary_tribute_count,
        summary_tribute_count,
        summary_tribute_nominal_total,
    ) == (
        plan_tribute_count,
        manifest_tribute_count,
        manifest_tribute_nominal_total,
    );
    if is_complete_root && !totals_match {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(())
}

fn decode_root_reduce_producer(
    consumer: &UnitSpecV1,
    expected: PlannedProducerV1,
    artifact: &UnitArtifactV1,
    plan_hash: B256,
    limits: &SchemaLimits,
) -> Result<RootReduceSummaryV1, WorkerError> {
    let PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::RootReduce,
        level,
        index,
    }) = expected
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let output = decode_root_reduce_output(artifact.phase_payload(limits)?, limits)?;
    let summary = match (level, output) {
        (0, RootReduceOutputV1::Leaf { summary, .. }) => summary,
        (_, RootReduceOutputV1::Node { summary }) if level > 0 => summary,
        _ => return Err(WorkerError::UnitBindingMismatch),
    };
    let expected_start = index
        .checked_shl(u32::from(level))
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let header = artifact.output_header(limits)?;
    let expected_binding = (
        consumer.protocol_bundle_hash,
        consumer.job_id,
        consumer.attempt,
        plan_hash,
        expected_start,
        level,
        index,
        summary.result_chunk_hashes.tree_root,
        summary.result_chunk_hashes.tree_root,
        summary.covered_primary_count,
        summary.covered_primary_count,
    );
    let actual_binding = (
        summary.protocol_bundle_hash,
        summary.job_id,
        summary.attempt,
        summary.plan_hash,
        summary.covered_primary_start,
        summary.result_chunk_hashes.subtree_height,
        summary.result_chunk_hashes.subtree_index,
        header.source_coverage_root,
        header.output_coverage_root,
        header.source_coverage_count,
        header.output_coverage_count,
    );
    if actual_binding != expected_binding {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(summary)
}

fn empty_root_reduce_leaf(
    spec: &UnitSpecV1,
    plan_hash: B256,
    padded_ordinal: u32,
) -> Result<RootReduceSummaryV1, WorkerError> {
    Ok(RootReduceSummaryV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        plan_hash,
        covered_primary_start: padded_ordinal,
        covered_primary_count: 0,
        nod_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::NodActions,
            padded_ordinal,
        )?,
        bucket_records: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::BucketRecords,
            padded_ordinal,
        )?,
        output_manifest_entries: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::CompleteOutputManifest,
            padded_ordinal,
        )?,
        result_chunk_hashes: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ResultChunkHashes,
            padded_ordinal,
        )?,
        tribute_count: 0,
        nod_count: 0,
        bucket_count: 0,
        tribute_nominal_total: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        nod_cost_total: U256::ZERO,
        first_error_ordinal: None,
    })
}
