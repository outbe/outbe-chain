//! canonical obligations for the offline OCOMP audit.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CanonicalLocalPinStage {
    Absent,
    AwaitingJobFinalization,
    Finalized,
    Exported,
    Terminal,
    GcPending,
    Released,
}

#[derive(Debug)]
pub(crate) struct CanonicalActiveAudit {
    pub intent_id: B256,
    pub job: OcompJobRecordV1,
    pub pin_stage: CanonicalLocalPinStage,
    pub projection_before_request: bool,
    pub source_verified: bool,
    pub export_verified: bool,
}

impl From<&CanonicalActiveAudit> for super::super::report::ActiveOcompObservation {
    fn from(audit: &CanonicalActiveAudit) -> Self {
        Self {
            intent_id: hex::encode(audit.intent_id),
            job_id: audit
                .job
                .finalized
                .as_ref()
                .map(|job| hex::encode(job.job_id)),
            request_height: audit.job.intent_height,
            worldwide_day: audit.job.intent.wwd,
            canonical_status: format!("{:?}", audit.job.status),
            pin_stage: format!("{:?}", audit.pin_stage),
            projection_before_request: Some(audit.projection_before_request),
            source_verified: audit.source_verified,
            export_verified: audit.export_verified,
        }
    }
}

pub(crate) struct CanonicalPinAudit {
    pub record: outbe_node::ocomp::retention::PinRecordV1,
    pub authority: VerifiedPin,
}

pub(crate) struct CanonicalOcompAudit {
    pub projection: outbe_primitives::projection::ProjectionCheckpoint,
    pub closure: ClosureAudit,
    pub bounds: InventoryBounds,
    pub active: Vec<CanonicalActiveAudit>,
    pub pins: Vec<CanonicalPinAudit>,
    pub source_leases: u64,
    pub complete_exports: u64,
    pub input_chunks: u64,
    pub nod: NodInputsAudit,
    pub payout_days: u64,
}

pub(crate) fn verify_canonical_obligations(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    scratch_parent: &Path,
    maximum_records: Option<u64>,
    mut report: Option<&mut super::super::report::ValidationReport>,
) -> eyre::Result<CanonicalOcompAudit> {
    use alloy_consensus::Sealable;
    use outbe_node::ocomp::retention::{inspect_retention_journal, PinStateV1, RetentionError};
    use outbe_offchain_data::{read_projection_state, ProjectionConfig};
    use outbe_offchain_storage::partitioned::adapters::RocksPartitionReadView;
    use outbe_offchain_storage::{PartitionedStorage, StorageReaderHandle};
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    let mut protected = layout.protected.clone();
    protected.0.extend([
        layout.chain_root.clone(),
        layout.consensus_root.clone(),
        layout.ocomp_root.clone(),
        layout.static_files_root.clone(),
        layout.execution_rocksdb_root.clone(),
    ]);
    if let Some(projection) = &layout.projection {
        protected.0.push(projection.root.clone());
    }
    // Independent canonical discovery precedes every local pin/job population.
    let mut inventory = CanonicalInventory::scan_with_report(
        state,
        scratch_parent,
        &protected,
        maximum_records,
        report.as_deref_mut(),
    )?;

    let location = layout
        .projection
        .as_ref()
        .ok_or_else(|| Incomplete("missing OCOMP projection configuration".into()))?;
    let database = projection_database(&location.root);
    match std::fs::metadata(database.join("CURRENT")) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                Incomplete("missing OCOMP projection database or CURRENT file".into()).into(),
            );
        }
        Err(error) => return Err(error.into()),
    }
    // Distinct from Task05's secondary. The immutable reader drops before this
    // local TempDir. No shared source handle or mutable catch-up API is exposed.
    let secondary = tempfile::Builder::new()
        .prefix("ocomp-projection-audit-")
        .tempdir_in(scratch_parent)?;
    let reader: StorageReaderHandle = Arc::new(PartitionedStorage::read_only(
        Arc::new(RocksPartitionReadView::open(
            &location.root,
            secondary.path(),
        )?),
        outbe_offchain_data::entity_partition_routing()?,
    ));
    let projection = read_projection_state(
        ProjectionConfig {
            chain_id: layout.chain.chain().id(),
            genesis_hash: layout.chain.genesis_hash(),
            start_block: location.start_block,
        },
        reader.clone(),
    )?
    .and_then(|state| state.checkpoint)
    .ok_or_else(|| Incomplete("missing initialized OCOMP projection checkpoint".into()))?;
    let p_header = view.header(projection.block_number)?.ok_or_else(|| {
        Incomplete(format!(
            "missing projection header P={}",
            projection.block_number
        ))
    })?;
    let p_hash = view
        .canonical_hash(projection.block_number)?
        .ok_or_else(|| {
            Incomplete(format!(
                "missing projection canonical hash P={}",
                projection.block_number
            ))
        })?;
    ensure!(
        p_header.inner.number == projection.block_number
            && p_header.hash_slow() == p_hash
            && p_hash == projection.block_hash,
        "OCOMP projection checkpoint differs from retained canonical header"
    );
    if let Some(report) = report.as_deref_mut() {
        report.observed.p = Some(outbe_snapshot::manifest::BlockIdentity {
            number: projection.block_number,
            hash: hex::encode(projection.block_hash),
        });
        for observation in &mut report.active_ocomp {
            observation.projection_before_request =
                Some(projection.block_number < observation.request_height);
        }
    }
    let closure = verify_closure(
        view,
        &layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1"),
        Some(projection),
        maximum_records,
    )?;

    if let Some(report) = report.as_deref_mut() {
        let identity = |point: outbe_primitives::projection::ProjectionCheckpoint| {
            outbe_snapshot::manifest::BlockIdentity {
                number: point.block_number,
                hash: hex::encode(point.block_hash),
            }
        };
        report.observed.c_baseline = Some(identity(closure.checkpoint.baseline));
        report.observed.c_previous = Some(identity(closure.checkpoint.previous));
        report.observed.c_current = Some(identity(closure.checkpoint.current));
        if closure.replay.blocks > 0 {
            report
                .retained_ranges
                .push(super::super::report::RetainedRange {
                    domain: "ocomp_replay_frames".into(),
                    start: closure.checkpoint.current.block_number + 1,
                    end_inclusive: view.progress.finalized.number,
                });
        }
    }

    let native_parameters = layout
        .chain
        .genesis
        .config
        .extra_fields
        .get_deserialized::<serde_json::Value>(outbe_chain_constants::GENESIS_CONFIG_KEY)
        .transpose()?;
    let parameters = outbe_chain_constants::GenesisProtocolParametersV1::select_for_build(
        native_parameters.as_ref(),
    )?;
    // This matches embedded_runtime's private native CAS policy.
    let cas_limits = outbe_ocomp::cas::CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: u64::MAX,
    };

    let records = match inspect_retention_journal(layout.consensus_root.join("ocomp_retention")) {
        Ok(journal) => journal.records,
        Err(RetentionError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    let mut pins = Vec::new();
    let mut pin_by_intent = BTreeMap::new();
    let total_pins = u64::try_from(records.len())?;
    for (index, (key, record)) in records.into_iter().enumerate() {
        scan_budget(
            "OCOMP pins",
            u64::try_from(index)?,
            total_pins,
            maximum_records,
        )?;
        let authority = verify_pin_authority(state, view, key, &record)?;
        ensure!(
            pin_by_intent
                .insert(authority.candidate.intent_id, index)
                .is_none(),
            "multiple retained pins bind the same canonical intent"
        );
        pins.push(CanonicalPinAudit { record, authority });
    }

    let pin_stage = |pin: Option<&CanonicalPinAudit>| match pin.map(|pin| pin.record.state) {
        None => CanonicalLocalPinStage::Absent,
        Some(PinStateV1::AwaitingJobFinalization { .. }) => {
            CanonicalLocalPinStage::AwaitingJobFinalization
        }
        Some(PinStateV1::Finalized { .. }) => CanonicalLocalPinStage::Finalized,
        Some(PinStateV1::Exported { .. }) => CanonicalLocalPinStage::Exported,
        Some(PinStateV1::Terminal { .. }) => CanonicalLocalPinStage::Terminal,
        Some(PinStateV1::GcPending { .. }) => CanonicalLocalPinStage::GcPending,
        Some(PinStateV1::Released { .. }) => CanonicalLocalPinStage::Released,
    };
    if let Some(report) = report.as_deref_mut() {
        for ((intent_id, _), observation) in
            inventory.active_jobs().iter().zip(&mut report.active_ocomp)
        {
            let pin = pin_by_intent.get(intent_id).map(|index| &pins[*index]);
            observation.pin_stage = format!("{:?}", pin_stage(pin));
        }
    }

    let mut checked_requests = BTreeSet::new();
    let mut checked_leases = BTreeSet::new();
    let mut require_source =
        |job: &OcompJobRecordV1, request_frame_required: bool| -> eyre::Result<()> {
            let b = job.intent_height;
            let header = view
                .header(b)?
                .ok_or_else(|| Incomplete(format!("missing active request header B={b}")))?;
            let hash = view.canonical_hash(b)?.ok_or_else(|| {
                Incomplete(format!("missing active request canonical hash B={b}"))
            })?;
            ensure!(
                header.inner.number == b
                    && header.hash_slow() == hash
                    && job.intent.logical_evaluation_height == b
                    && job.intent.logical_evaluation_time == header.inner.timestamp,
                "required OCOMP source has a conflicting frozen request identity"
            );
            if request_frame_required && !checked_requests.contains(&b) {
                verify_retained_frames(
                    view,
                    b,
                    outbe_snapshot::manifest::BlockIdentity {
                        number: b,
                        hash: hex::encode(hash),
                    },
                    maximum_records,
                )?;
                checked_requests.insert(b);
            }
            let lease = job.intent.input_lease_id()?;
            if !checked_leases.contains(&lease) {
                verify_lease_inputs(
                    reader.clone(),
                    &job.intent,
                    scratch_parent,
                    &protected,
                    maximum_records,
                )?;
                checked_leases.insert(lease);
            }
            Ok(())
        };
    let mut checked_exports = BTreeSet::new();
    let mut input_chunks = 0_u64;
    let mut require_export = |job: &OcompJobRecordV1, authority| -> eyre::Result<()> {
        let intent = job.intent.intent_id(&poc_schema_limits())?;
        if !checked_exports.contains(&intent) {
            let export = verify_export_inputs(&layout.ocomp_root, job, authority, cas_limits)?;
            input_chunks = input_chunks
                .checked_add(export.input_chunks)
                .ok_or_else(|| eyre::eyre!("OCOMP input chunk count overflow"))?;
            checked_exports.insert(intent);
        }
        Ok(())
    };
    for pin in &pins {
        if pin.authority.requires_source {
            require_source(&pin.authority.job, false)?;
        }
        if matches!(
            pin.record.state,
            PinStateV1::Exported { .. }
                | PinStateV1::Terminal {
                    export: Some(_),
                    ..
                }
        ) {
            require_export(&pin.authority.job, pin.authority.export)?;
        }
    }

    let mut active = Vec::new();
    for (active_index, (intent_id, listed)) in inventory.active_jobs().iter().enumerate() {
        let job = state.metadosis_job(
            *intent_id,
            WorldwideDay::new(listed.intent.wwd),
            listed.finalized.as_ref().map(|f| f.job_id),
        )?;
        let pin = pin_by_intent.get(intent_id).map(|index| &pins[*index]);
        // Missing pin is observed. P>=B alone cannot imply corruption across all
        // native runtime policies. Current source capability is still mandatory.
        let saved_export = pin.and_then(|pin| pin.authority.export);
        // A surviving complete receipt records local export progress independently
        // of the retention pin. Only current active jobs acquire this obligation.
        let receipt_present = match &job.finalized {
            Some(finalized) => existing_file(
                &layout
                    .ocomp_root
                    .join("exporter-v1/receipts")
                    .join(hex::encode(finalized.job_id))
                    .join("receipt.ref"),
            )?,
            None => false,
        };
        let export_recorded = saved_export.is_some() || receipt_present;
        require_source(&job, !export_recorded)?;
        if let Some(report) = report.as_deref_mut() {
            report.active_ocomp[active_index].source_verified = true;
        }
        let mut export_verified = false;
        if export_recorded {
            // A historical GC exemption cannot waive a current active obligation.
            require_export(&job, saved_export)?;
            export_verified = true;
            if let Some(report) = report.as_deref_mut() {
                report.active_ocomp[active_index].export_verified = true;
            }
        }
        active.push(CanonicalActiveAudit {
            intent_id: *intent_id,
            projection_before_request: projection.block_number < job.intent_height,
            job,
            pin_stage: pin_stage(pin),
            source_verified: true,
            export_verified,
        });
    }
    let source_leases = u64::try_from(checked_leases.len())?;
    let complete_exports = u64::try_from(checked_exports.len())?;
    let nod = inventory.verify_nod_inputs(
        &layout.ocomp_root,
        cas_limits,
        parameters.nod_materialization_batch_subtree_height,
        maximum_records,
    )?;
    let payout_days = inventory.verify_payout_files(&layout.ocomp_root)?;
    let bounds = std::mem::take(&mut inventory.bounds);
    Ok(CanonicalOcompAudit {
        projection,
        closure,
        bounds,
        active,
        pins,
        source_leases,
        complete_exports,
        input_chunks,
        nod,
        payout_days,
    })
}
