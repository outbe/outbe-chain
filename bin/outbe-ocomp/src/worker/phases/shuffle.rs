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
use outbe_lysis::program_v1::phases::shuffle_owners;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::planner::PlannedProducerV1;
use outbe_lysis::program_v1::planner::PlannedUnitPositionV1;

use outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE;

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::result::ContributorActionV1 as ProtocolContributorActionV1;

use outbe_ocomp_protocol::shuffle::build_bucket_shuffle_run;
use outbe_ocomp_protocol::shuffle::build_owner_shuffle_run;
use outbe_ocomp_protocol::shuffle::merge_verified_shuffle_runs;

use outbe_ocomp_protocol::shuffle::verified_shuffle_run_records;
use outbe_ocomp_protocol::shuffle::ShuffleBucketRecordV1;
use outbe_ocomp_protocol::shuffle::ShuffleRunArtifactV1;
use outbe_ocomp_protocol::shuffle::ShuffleRunBuildContextV1;
use outbe_ocomp_protocol::shuffle::ShuffleRunKindV1;
use outbe_ocomp_protocol::shuffle::ShuffleSourceCoverageV1;
use outbe_ocomp_protocol::shuffle::VerifiedShuffleRecordV1;

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
        manifest,
        input_chunks,
        producer_artifacts,
        bundle,
        limits,
        reader,
        inbox,
        cancelled,
    } = authority;
    require_lease_active(cancelled)?;
    if !input_chunks.is_empty()
        || !matches!(
            spec.phase,
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle
        )
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let phase_offset = topology.phase_offset(spec.phase)?;
    let phase_ordinal = unit_index
        .checked_sub(phase_offset)
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
    let purpose = if level == 0 {
        InputPurpose::FinalizedOutputRecords
    } else if spec.phase == UnitPhase::OwnerShuffle {
        InputPurpose::OwnerOrderedRecords
    } else {
        InputPurpose::BucketOrderedRecords
    };
    let producer_inputs = scan_producer_inputs(spec, purpose)?;
    let producer_ids = producer_inputs
        .iter()
        .map(|input| {
            if input.source_kind != InputSourceKind::UnitOutput || input.source_id.is_zero() {
                Err(WorkerError::UnitBindingMismatch)
            } else {
                Ok(input.source_id)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let planner = planner_from_authority(plan, manifest, bundle, limits)?;
    if planner.shuffle_unit_at(spec.phase, phase_ordinal, &producer_ids, limits)? != spec.clone() {
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
    if resolved.iter().any(Option::is_none) {
        return Err(WorkerError::UnitBindingMismatch);
    }

    let (coverage, owner_records, bucket_records) = if level == 0 {
        if resolved.len() != 1 || end_run != start_run + 1 {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let producer = resolved[0].ok_or(WorkerError::UnitBindingMismatch)?;
        let finalized = decode_finalized_output_run(producer.phase_payload(limits)?, limits)?;
        let header = producer.output_header(limits)?;
        let expected_start = start_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let expected_end = end_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(WorkerError::UnitBindingMismatch)?
            .min(plan.tribute_count);
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
        if spec.phase == UnitPhase::OwnerShuffle {
            let owner_run = shuffle_owners(&finalized).map_err(LysisArtifactErrorV1::from)?;
            let records = owner_run
                .ordered_contributors
                .into_iter()
                .map(|record| {
                    Ok(ProtocolContributorActionV1 {
                        owner: record.owner,
                        source_tribute_id: *record.source_tribute_id,
                        nominal_amount_minor: record.nominal_amount_minor,
                    })
                })
                .collect::<Vec<_>>();
            (coverage, Some(records), None)
        } else {
            let bucket_run = shuffle_buckets(&finalized).map_err(LysisArtifactErrorV1::from)?;
            let records = bucket_run
                .ordered_records
                .into_iter()
                .map(|record| {
                    Ok(ShuffleBucketRecordV1 {
                        bucket_key: record.bucket_key,
                        raw_ordinal: record.raw_ordinal,
                        tribute_id: *record.tribute_id,
                        nod_id: *record.nod_id,
                    })
                })
                .collect::<Vec<_>>();
            (coverage, None, Some(records))
        }
    } else {
        if resolved.len() != 2 {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let expected_kind = if spec.phase == UnitPhase::OwnerShuffle {
            ShuffleRunKindV1::Owner
        } else {
            ShuffleRunKindV1::Bucket
        };
        let left = decode_shuffle_producer_root(
            spec,
            resolved[0].ok_or(WorkerError::UnitBindingMismatch)?,
            expected_kind,
            expected[0],
            limits,
        )?;
        let right = decode_shuffle_producer_root(
            spec,
            resolved[1].ok_or(WorkerError::UnitBindingMismatch)?,
            expected_kind,
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
        let left_records = verified_shuffle_run_records(left, limits, |reference| {
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
        })?;
        let right_records = verified_shuffle_run_records(right, limits, |reference| {
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
        })?;
        let merged = merge_verified_shuffle_runs(expected_kind, left_records, right_records);
        if spec.phase == UnitPhase::OwnerShuffle {
            let records = merged.map(|record| match record? {
                VerifiedShuffleRecordV1::Owner(record) => Ok(record),
                VerifiedShuffleRecordV1::Bucket(_) => {
                    Err(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                        "owner shuffle producer kind",
                    ))
                }
            });
            return build_owner_shuffle_unit_artifact(
                spec, run_span, coverage, records, limits, inbox,
            );
        }
        let records = merged.map(|record| match record? {
            VerifiedShuffleRecordV1::Bucket(record) => Ok(record),
            VerifiedShuffleRecordV1::Owner(_) => {
                Err(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                    "bucket shuffle producer kind",
                ))
            }
        });
        return build_bucket_shuffle_unit_artifact(
            spec, run_span, coverage, records, limits, inbox,
        );
    };

    if let Some(records) = owner_records {
        build_owner_shuffle_unit_artifact(spec, run_span, coverage, records, limits, inbox)
    } else {
        build_bucket_shuffle_unit_artifact(
            spec,
            run_span,
            coverage,
            bucket_records.ok_or(WorkerError::UnitBindingMismatch)?,
            limits,
            inbox,
        )
    }
}

fn build_owner_shuffle_unit_artifact<I>(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    coverage: ShuffleSourceCoverageV1,
    records: I,
    limits: &SchemaLimits,
    inbox: &WorkerInbox,
) -> Result<UnitArtifactV1, WorkerError>
where
    I: IntoIterator<
        Item = Result<ProtocolContributorActionV1, outbe_ocomp_protocol::ProtocolError>,
    >,
{
    if spec.phase != UnitPhase::OwnerShuffle {
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
    let root = build_owner_shuffle_run(context, records, limits, &mut stage)?;
    finish_shuffle_unit_artifact(spec, root, limits)
}

fn build_bucket_shuffle_unit_artifact<I>(
    spec: &UnitSpecV1,
    run_span: outbe_ocomp_protocol::unit::CanonicalRunSpan,
    coverage: ShuffleSourceCoverageV1,
    records: I,
    limits: &SchemaLimits,
    inbox: &WorkerInbox,
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
    expected_kind: ShuffleRunKindV1,
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
    let expected_phase = match expected_kind {
        ShuffleRunKindV1::Owner => UnitPhase::OwnerShuffle,
        ShuffleRunKindV1::Bucket => UnitPhase::BucketShuffle,
    };
    if phase != expected_phase || artifact.phase != expected_phase {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root = ShuffleRunArtifactV1::decode_canonical(artifact.phase_payload(limits)?, limits)?;
    root.validate_root_semantics(limits)?;
    let header = artifact.output_header(limits)?;
    if root.protocol_bundle_hash != consumer.protocol_bundle_hash
        || root.job_id != consumer.job_id
        || root.attempt != consumer.attempt
        || root.unit_id != artifact.unit_id
        || root.kind != expected_kind
        || root.run_span.start_run != start_run
        || root.run_span.end_run != end_run
        || root.source_coverage_root != header.source_coverage_root
        || root.source_coverage_count != header.source_coverage_count
        || root.ordered_record_root != header.output_coverage_root
        || root.record_count != header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(root)
}
