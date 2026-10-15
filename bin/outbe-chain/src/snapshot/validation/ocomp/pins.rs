//! pins obligations for the offline OCOMP audit.
use super::*;

pub(crate) struct VerifiedPin {
    pub candidate: outbe_node::ocomp::retention::CandidatePinV1,
    pub job: OcompJobRecordV1,
    /// Retention required by this local pin only. A false value cannot waive
    /// independent canonical active-job or shared-lease obligations.
    pub requires_source: bool,
    pub export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
}

/// Bind a natively decoded journal record to immutable current-E authority.
/// Local lifecycle progress may lag canonical completion. It is not a claim
/// that the donor executed, exported or materialized the completed job.
pub(crate) fn verify_pin_authority(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    key: B256,
    record: &outbe_node::ocomp::retention::PinRecordV1,
) -> eyre::Result<VerifiedPin> {
    use alloy_consensus::Sealable;
    use outbe_node::ocomp::retention::PinStateV1;
    let candidate = match record.state {
        PinStateV1::AwaitingJobFinalization { candidate }
        | PinStateV1::Finalized { candidate, .. }
        | PinStateV1::Exported { candidate, .. }
        | PinStateV1::Terminal { candidate, .. }
        | PinStateV1::GcPending { candidate, .. }
        | PinStateV1::Released { candidate, .. } => candidate,
    };
    if key != candidate.block_hash {
        return Err(SnapshotOcompError::RetentionKeyBlockHashMismatch.into());
    }
    let expected_job = match record.state {
        PinStateV1::AwaitingJobFinalization { .. } => None,
        PinStateV1::Finalized { job_id, .. }
        | PinStateV1::Exported { job_id, .. }
        | PinStateV1::Terminal { job_id, .. }
        | PinStateV1::GcPending { job_id, .. }
        | PinStateV1::Released { job_id, .. } => Some(job_id),
    };
    let job = state.metadosis_job(
        candidate.intent_id,
        WorldwideDay::new(candidate.wwd),
        expected_job,
    )?;
    if job.intent_height != candidate.block_number
        || job.intent.ce_sealed_root != candidate.ce_sealed_root
        || job.intent.protocol_bundle_hash != candidate.protocol_bundle_hash
        || job.intent.input_lease_id()? != candidate.input_lease_id
    {
        return Err(SnapshotOcompError::RetainedCandidateRequestMismatch.into());
    }
    // The canonical getter validates finalized B bindings. Awaiting pins also
    // need the retained request header even before canonical finality exists.
    let number = candidate.block_number;
    let header = view.header(number)?.ok_or_else(|| {
        SnapshotIncompleteError::MissingRetainedPinRequestHeader { number }.into_incomplete()
    })?;
    let canonical_hash = view.canonical_hash(number)?.ok_or_else(|| {
        SnapshotIncompleteError::MissingCanonicalPinRequestHash { number }.into_incomplete()
    })?;
    if header.inner.number != number
        || header.hash_slow() != canonical_hash
        || canonical_hash != candidate.block_hash
        || header.inner.state_root != candidate.state_root
    {
        return Err(SnapshotOcompError::RetainedCandidateHeaderMismatch { number }.into());
    }
    match record.state {
        PinStateV1::Finalized {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::Exported {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::Terminal {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::GcPending {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        } => {
            let finalized = job
                .finalized
                .as_ref()
                .ok_or(SnapshotOcompError::RetainedFinalizedPinLacksFinality)?;
            if finalized.finality_recorded_height != finality_recorded_height
                || finalized.open_height != open_height
                || finalized.deadline_height != deadline_height
            {
                return Err(SnapshotOcompError::RetainedPinFinalityWindowMismatch.into());
            }
        }
        PinStateV1::AwaitingJobFinalization { .. } | PinStateV1::Released { .. } => {}
    }
    let export = match record.state {
        PinStateV1::Exported { export, .. } => Some(export),
        PinStateV1::Terminal { export, .. }
        | PinStateV1::GcPending { export, .. }
        | PinStateV1::Released { export, .. } => export,
        _ => None,
    };
    Ok(VerifiedPin {
        candidate,
        job,
        requires_source: !matches!(
            record.state,
            PinStateV1::GcPending { .. } | PinStateV1::Released { .. }
        ),
        export,
    })
}

/// Close a required lease's live/retained body union to its canonical JobIntent.
/// Callers decide which leases remain required. Historical GC alone does not
/// create a requirement to reproduce a released population.
pub(crate) fn verify_lease_inputs(
    reader: outbe_offchain_storage::StorageReaderHandle,
    intent: &outbe_ocomp_protocol::intent::JobIntentV1,
    scratch_parent: &Path,
    protected: &ProtectedPaths,
    maximum_records: Option<u64>,
) -> eyre::Result<()> {
    use outbe_compressed_entities::{
        BoundedTributePartitionVerifier, Commitment, TributePartitionExpectationV1,
        TributePartitionWorkConfig, ACTIVE_COMMITMENT_SCHEME,
    };
    use outbe_ocomp::exporter::{FinalizedTributeError, FinalizedTributeSource};
    use outbe_tribute::RetainedTributePin;

    validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
    let pin = RetainedTributePin {
        input_lease_id: intent.input_lease_id()?,
        worldwide_day: WorldwideDay::new(intent.wwd),
    };
    let classify = |error: FinalizedTributeError| {
        if matches!(error, FinalizedTributeError::MissingBody(_))
            || matches!(error, FinalizedTributeError::CountMismatch { expected, actual } if actual < expected)
            || missing_native_input(&error)
        {
            eyre::Report::new(error).wrap_err(
                SnapshotIncompleteError::MissingRequiredTributeInputs {
                    input_lease_id: pin.input_lease_id,
                    wwd: intent.wwd,
                }
                .into_incomplete(),
            )
        } else {
            eyre::Report::new(error).wrap_err(SnapshotOcompError::TributeInputsForLease {
                input_lease_id: pin.input_lease_id,
                wwd: intent.wwd,
            })
        }
    };
    let source = FinalizedTributeSource::new(reader, outbe_offchain_storage::MAX_SCAN_ENTRIES)
        .map_err(&classify)?;
    let mut stream = source
        .commitment_reconstruction_stream(pin, intent.authenticated_day_count)
        .map_err(&classify)?;
    let directory = tempfile::Builder::new()
        .prefix("outbe-lease-audit-")
        .tempdir_in(scratch_parent)?;
    let mut partition = BoundedTributePartitionVerifier::create(
        directory.path().join("partition"),
        TributePartitionExpectationV1 {
            day: pin.worldwide_day,
            exact_leaf_count: intent.authenticated_day_count,
            expected_collection_root: intent.sealed_tribute_collection_root,
            commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
        },
        TributePartitionWorkConfig::default(),
    )?;
    let mut visited = 0_u64;
    while let Some(record) = stream.next_commitment().map_err(&classify)? {
        if maximum_records.is_some_and(|maximum| visited >= maximum) {
            return Err(SnapshotIncompleteError::TributeLeaseScanStopped {
                input_lease_id: pin.input_lease_id,
                visited,
                authenticated_day_count: intent.authenticated_day_count,
            }
            .into_report());
        }
        partition.push(
            record.tribute_id,
            Commitment::try_from(record.commitment.0)?,
        )?;
        visited = visited
            .checked_add(1)
            .ok_or(SnapshotOcompError::TributeVisitCountOverflow)?;
    }
    // Count underflow is unavailable input. A different complete population
    // fails the finalized collection root check.
    stream.finish().map_err(classify)?;
    partition.finish()?;
    Ok(())
}
