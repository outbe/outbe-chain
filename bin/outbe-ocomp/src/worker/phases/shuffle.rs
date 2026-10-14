use super::super::planner_from_authority;
use super::super::require_lease_active;
use super::super::resolve_scan_artifacts;
use super::super::scan_producer_inputs;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::inbox::WorkerInbox;

use outbe_lysis::program_v1::artifacts::decode_finalized_output_run;

use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;

use outbe_lysis::program_v1::phases::shuffle_buckets;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::shuffle::build_bucket_shuffle_run;
use outbe_ocomp_protocol::shuffle::merge_verified_shuffle_runs;

use outbe_ocomp_protocol::shuffle::verified_shuffle_run_records;
use outbe_ocomp_protocol::shuffle::ShuffleBucketRecordV1;
use outbe_ocomp_protocol::shuffle::ShuffleRunArtifactV1;
use outbe_ocomp_protocol::shuffle::ShuffleRunBuildContextV1;
use outbe_ocomp_protocol::shuffle::ShuffleSourceCoverageV1;

use outbe_ocomp_protocol::unit::InputPurpose;
use outbe_ocomp_protocol::unit::InputSourceKind;

use outbe_ocomp_protocol::unit::UnitArtifactV1;

use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

use outbe_ocomp_protocol::SchemaLimits;

pub(in super::super) fn execute_shuffle_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        unit_index,
        input_chunks,
        producer_artifacts,
        limits,
        reader,
        inbox,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    if !input_chunks.is_empty() || spec.phase != UnitPhase::BucketShuffle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_ordinal = unit_index
        .checked_sub(topology.phase_offset(spec.phase)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let position = topology.phase_position_at(spec.phase, phase_ordinal)?;
    let PlannedUnitPositionV1::RunSpan {
        level,
        start_run,
        end_run,
        ..
    } = position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let run_span = outbe_ocomp_protocol::unit::CanonicalRunSpan { start_run, end_run };
    let (expected, resolved) = resolve_shuffle_producers(
        spec,
        &authority,
        &topology,
        (phase_ordinal, position, level),
        producer_artifacts,
    )?;

    if level == 0 {
        execute_leaf_shuffle_unit(
            spec,
            run_span,
            plan.tribute_count,
            &resolved,
            (limits, inbox),
        )
    } else {
        execute_branch_shuffle_unit(
            spec,
            run_span,
            &expected,
            &resolved,
            (limits, reader, inbox, cancelled),
        )
    }
}

fn resolve_shuffle_producers<'a>(
    spec: &UnitSpecV1,
    authority: &UnitExecutionAuthority<'_>,
    topology: &LysisPlanTopologyV1,
    (phase_ordinal, position, level): (u32, PlannedUnitPositionV1, u16),
    producer_artifacts: &'a [UnitArtifactV1],
) -> Result<(Vec<PlannedProducerV1>, Vec<Option<&'a UnitArtifactV1>>), WorkerError> {
    let purpose = if level == 0 {
        InputPurpose::FinalizedOutputRecords
    } else {
        InputPurpose::BucketOrderedRecords
    };
    let producer_inputs = scan_producer_inputs(spec, purpose)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| {
            (input.source_kind == InputSourceKind::UnitOutput && !input.source_id.is_zero())
                .then_some(input.source_id)
                .ok_or(WorkerError::UnitBindingMismatch)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let planner = planner_from_authority(
        authority.plan,
        authority.manifest,
        authority.bundle,
        authority.limits,
    )?;
    if planner.shuffle_unit_at(phase_ordinal, &producer_ids, authority.limits)? != *spec {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let expected = topology.required_producers(position)?;
    let resolved = resolve_scan_artifacts(
        spec,
        &expected,
        &producer_inputs,
        producer_artifacts,
        authority.limits,
    )?;
    if resolved.iter().any(Option::is_none) {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok((expected, resolved))
}

fn execute_leaf_shuffle_unit(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    tribute_count: u32,
    resolved: &[Option<&UnitArtifactV1>],
    (limits, inbox): (&SchemaLimits, &WorkerInbox),
) -> Result<UnitArtifactV1, WorkerError> {
    if resolved.len() != 1 || run_span.end_run != run_span.start_run + 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let producer = resolved[0].ok_or(WorkerError::UnitBindingMismatch)?;
    let finalized = decode_finalized_output_run(producer.phase_payload(limits)?, limits)?;
    let header = producer.output_header(limits)?;
    let expected_start = run_span
        .start_run
        .checked_mul(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let expected_end = run_span
        .end_run
        .checked_mul(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(WorkerError::UnitBindingMismatch)?
        .min(tribute_count);
    if finalized.start_ordinal != expected_start
        || finalized.end_ordinal != expected_end
        || finalized.coverage_root()? != header.output_coverage_root
        || finalized.end_ordinal - finalized.start_ordinal != header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let coverage = ShuffleSourceCoverageV1::leaf(
        run_span.clone(),
        header.output_coverage_root,
        header.output_coverage_count,
        limits,
    )?;
    let bucket_run = shuffle_buckets(&finalized).map_err(LysisArtifactErrorV1::from)?;
    let records = bucket_run.ordered_records.into_iter().map(|record| {
        Ok(ShuffleBucketRecordV1 {
            bucket_key: record.bucket_key,
            raw_ordinal: record.raw_ordinal,
            tribute_id: *record.tribute_id,
            nod_id: *record.nod_id,
        })
    });
    build_bucket_shuffle_unit_artifact(spec, (run_span, coverage), records, (limits, inbox))
}

fn execute_branch_shuffle_unit(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    expected: &[PlannedProducerV1],
    resolved: &[Option<&UnitArtifactV1>],
    (limits, reader, inbox, cancelled): (
        &SchemaLimits,
        &crate::cas::FilesystemCasReader,
        &WorkerInbox,
        Option<&std::sync::atomic::AtomicBool>,
    ),
) -> Result<UnitArtifactV1, WorkerError> {
    if resolved.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let left = decode_shuffle_producer_root(
        spec,
        resolved[0].ok_or(WorkerError::UnitBindingMismatch)?,
        expected[0],
        limits,
    )?;
    let right = decode_shuffle_producer_root(
        spec,
        resolved[1].ok_or(WorkerError::UnitBindingMismatch)?,
        expected[1],
        limits,
    )?;
    let coverage = ShuffleSourceCoverageV1::merge(
        &ShuffleSourceCoverageV1 {
            run_span: left.run_span.clone(),
            root: left.source_coverage_root,
            count: left.source_coverage_count,
        },
        &ShuffleSourceCoverageV1 {
            run_span: right.run_span.clone(),
            root: right.source_coverage_root,
            count: right.source_coverage_count,
        },
        limits,
    )?;
    if coverage.run_span != run_span {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let left_records = read_verified_shuffle_records(left, limits, reader, cancelled)?;
    let right_records = read_verified_shuffle_records(right, limits, reader, cancelled)?;
    let records = merge_verified_shuffle_runs(left_records, right_records);
    build_bucket_shuffle_unit_artifact(spec, (run_span, coverage), records, (limits, inbox))
}

fn read_verified_shuffle_records<'a>(
    root: ShuffleRunArtifactV1,
    limits: &'a SchemaLimits,
    reader: &'a crate::cas::FilesystemCasReader,
    cancelled: Option<&'a std::sync::atomic::AtomicBool>,
) -> Result<
    impl Iterator<Item = Result<ShuffleBucketRecordV1, outbe_ocomp_protocol::ProtocolError>> + 'a,
    WorkerError,
> {
    verified_shuffle_run_records(root, limits, move |reference| {
        require_lease_active(cancelled).map_err(|_| {
            outbe_ocomp_protocol::ProtocolError::InvalidInvariant("cancelled shuffle lease")
        })?;
        reader
            .read_verified(reference)
            .map(|object| object.bytes().to_vec())
            .map_err(|_| {
                outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                    "shuffle producer descendant read",
                )
            })
    })
    .map_err(WorkerError::from)
}

fn build_bucket_shuffle_unit_artifact<I>(
    spec: &UnitSpecV1,
    (run_span, coverage): (
        outbe_ocomp_protocol::unit::CanonicalRunSpan,
        ShuffleSourceCoverageV1,
    ),
    records: I,
    (limits, inbox): (&SchemaLimits, &WorkerInbox),
) -> Result<UnitArtifactV1, WorkerError>
where
    I: IntoIterator<Item = Result<ShuffleBucketRecordV1, outbe_ocomp_protocol::ProtocolError>>,
{
    if spec.phase != UnitPhase::BucketShuffle {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let context = ShuffleRunBuildContextV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        unit_id: spec.unit_id(limits)?,
        run_span,
        source_coverage_root: coverage.root,
        source_coverage_count: coverage.count,
    };
    let mut stage = |bytes: &[u8]| {
        inbox.stage_shuffle_object(bytes, limits).map_err(|_| {
            outbe_ocomp_protocol::ProtocolError::InvalidInvariant("shuffle object staging")
        })
    };
    let root = build_bucket_shuffle_run(context, records, limits, &mut stage)?;
    finish_shuffle_unit_artifact(spec, root, limits)
}

fn finish_shuffle_unit_artifact(
    spec: &UnitSpecV1,
    root: ShuffleRunArtifactV1,
    limits: &SchemaLimits,
) -> Result<UnitArtifactV1, WorkerError> {
    let payload = root.encode_canonical(limits)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: root.source_coverage_root,
            output_coverage_root: root.ordered_record_root,
            source_coverage_count: root.source_coverage_count,
            output_coverage_count: root.record_count,
        },
        BoundedBytes(payload),
        limits,
    )
    .map_err(WorkerError::from)
}

pub(in super::super) fn decode_shuffle_producer_root(
    consumer: &UnitSpecV1,
    artifact: &UnitArtifactV1,
    expected_position: PlannedProducerV1,
    limits: &SchemaLimits,
) -> Result<ShuffleRunArtifactV1, WorkerError> {
    let PlannedProducerV1::Unit(PlannedUnitPositionV1::RunSpan {
        phase,
        start_run,
        end_run,
        ..
    }) = expected_position
    else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let expected_phase = UnitPhase::BucketShuffle;
    if phase != expected_phase || artifact.phase != expected_phase {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root = ShuffleRunArtifactV1::decode_canonical(artifact.phase_payload(limits)?, limits)?;
    root.validate_root_semantics(limits)?;
    let header = artifact.output_header(limits)?;
    let expected_binding = (
        consumer.protocol_bundle_hash,
        consumer.job_id,
        consumer.attempt,
        artifact.unit_id,
        start_run,
        end_run,
        header.source_coverage_root,
        header.source_coverage_count,
        header.output_coverage_root,
        header.output_coverage_count,
    );
    let actual_binding = (
        root.protocol_bundle_hash,
        root.job_id,
        root.attempt,
        root.unit_id,
        root.run_span.start_run,
        root.run_span.end_run,
        root.source_coverage_root,
        root.source_coverage_count,
        root.ordered_record_root,
        root.record_count,
    );
    if actual_binding != expected_binding {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(root)
}
