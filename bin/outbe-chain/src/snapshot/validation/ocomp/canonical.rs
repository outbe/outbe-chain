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
}

fn pin_stage(pin: Option<&CanonicalPinAudit>) -> CanonicalLocalPinStage {
    use outbe_node::ocomp::retention::PinStateV1;
    match pin.map(|pin| pin.record.state) {
        None => CanonicalLocalPinStage::Absent,
        Some(PinStateV1::AwaitingJobFinalization { .. }) => {
            CanonicalLocalPinStage::AwaitingJobFinalization
        }
        Some(PinStateV1::Finalized { .. }) => CanonicalLocalPinStage::Finalized,
        Some(PinStateV1::Exported { .. }) => CanonicalLocalPinStage::Exported,
        Some(PinStateV1::Terminal { .. }) => CanonicalLocalPinStage::Terminal,
        Some(PinStateV1::GcPending { .. }) => CanonicalLocalPinStage::GcPending,
        Some(PinStateV1::Released { .. }) => CanonicalLocalPinStage::Released,
    }
}

fn open_verified_projection_checkpoint(
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    scratch_parent: &Path,
    report: Option<&mut super::super::report::ValidationReport>,
) -> eyre::Result<(
    tempfile::TempDir,
    outbe_offchain_storage::StorageReaderHandle,
    outbe_primitives::projection::ProjectionCheckpoint,
)> {
    use alloy_consensus::Sealable;
    use outbe_offchain_data::{read_projection_state, ProjectionConfig};
    use outbe_offchain_storage::partitioned::adapters::RocksPartitionReadView;
    use outbe_offchain_storage::{PartitionedStorage, StorageReaderHandle};
    use std::sync::Arc;

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
    if p_header.inner.number != projection.block_number
        || p_header.hash_slow() != p_hash
        || p_hash != projection.block_hash
    {
        return Err(SnapshotOcompError::ProjectionCheckpointHeaderMismatch.into());
    }
    if let Some(report) = report {
        report.observed.p = Some(outbe_snapshot::manifest::BlockIdentity {
            number: projection.block_number,
            hash: hex::encode(projection.block_hash),
        });
        for observation in &mut report.active_ocomp {
            observation.projection_before_request =
                Some(projection.block_number < observation.request_height);
        }
    }
    Ok((secondary, reader, projection))
}

fn verify_and_record_closure(
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    projection: outbe_primitives::projection::ProjectionCheckpoint,
    maximum_records: Option<u64>,
    report: Option<&mut super::super::report::ValidationReport>,
) -> eyre::Result<ClosureAudit> {
    let closure = verify_closure(
        view,
        &layout
            .ocomp_root
            .join("exporter-v1/discovery/closure-checkpoint-v1"),
        Some(projection),
        maximum_records,
    )?;
    if let Some(report) = report {
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
    Ok(closure)
}

fn load_and_verify_pins(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    maximum_records: Option<u64>,
) -> eyre::Result<(
    Vec<CanonicalPinAudit>,
    std::collections::BTreeMap<B256, usize>,
)> {
    use outbe_node::ocomp::retention::{inspect_retention_journal, RetentionError};
    use std::collections::BTreeMap;

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
        if pin_by_intent
            .insert(authority.candidate.intent_id, index)
            .is_some()
        {
            return Err(SnapshotOcompError::MultipleRetainedPinsForIntent.into());
        }
        pins.push(CanonicalPinAudit { record, authority });
    }
    Ok((pins, pin_by_intent))
}

struct CanonicalObligationVerifier<'a> {
    view: &'a crate::snapshot::native::RethReadOnlyView,
    layout: &'a crate::snapshot::config::RequestedLayout,
    reader: outbe_offchain_storage::StorageReaderHandle,
    scratch_parent: &'a Path,
    protected: &'a ProtectedPaths,
    cas_limits: CasLimits,
    maximum_records: Option<u64>,
    checked_requests: std::collections::BTreeSet<u64>,
    checked_leases: std::collections::BTreeSet<B256>,
    checked_exports: std::collections::BTreeSet<B256>,
    input_chunks: u64,
}

impl CanonicalObligationVerifier<'_> {
    fn require_source(
        &mut self,
        job: &OcompJobRecordV1,
        request_frame_required: bool,
    ) -> eyre::Result<()> {
        use alloy_consensus::Sealable;
        let b = job.intent_height;
        let header = self
            .view
            .header(b)?
            .ok_or_else(|| Incomplete(format!("missing active request header B={b}")))?;
        let hash = self
            .view
            .canonical_hash(b)?
            .ok_or_else(|| Incomplete(format!("missing active request canonical hash B={b}")))?;
        if header.inner.number != b
            || header.hash_slow() != hash
            || job.intent.logical_evaluation_height != b
            || job.intent.logical_evaluation_time != header.inner.timestamp
        {
            return Err(SnapshotOcompError::SourceConflictingRequestIdentity.into());
        }
        if request_frame_required && !self.checked_requests.contains(&b) {
            verify_retained_frames(
                self.view,
                b,
                outbe_snapshot::manifest::BlockIdentity {
                    number: b,
                    hash: hex::encode(hash),
                },
                self.maximum_records,
            )?;
            self.checked_requests.insert(b);
        }
        let lease = job.intent.input_lease_id()?;
        if !self.checked_leases.contains(&lease) {
            verify_lease_inputs(
                self.reader.clone(),
                &job.intent,
                self.scratch_parent,
                self.protected,
                self.maximum_records,
            )?;
            self.checked_leases.insert(lease);
        }
        Ok(())
    }

    fn require_export(
        &mut self,
        job: &OcompJobRecordV1,
        authority: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
    ) -> eyre::Result<()> {
        let intent = job.intent.intent_id(&poc_schema_limits())?;
        if !self.checked_exports.contains(&intent) {
            let export =
                verify_export_inputs(&self.layout.ocomp_root, job, authority, self.cas_limits)?;
            self.input_chunks = self
                .input_chunks
                .checked_add(export.input_chunks)
                .ok_or(SnapshotOcompError::InputChunkCountOverflow)?;
            self.checked_exports.insert(intent);
        }
        Ok(())
    }
    fn verify_pin_obligations(&mut self, pins: &[CanonicalPinAudit]) -> eyre::Result<()> {
        use outbe_node::ocomp::retention::PinStateV1;
        for pin in pins {
            if pin.authority.requires_source {
                self.require_source(&pin.authority.job, false)?;
            }
            if matches!(
                pin.record.state,
                PinStateV1::Exported { .. }
                    | PinStateV1::Terminal {
                        export: Some(_),
                        ..
                    }
            ) {
                self.require_export(&pin.authority.job, pin.authority.export)?;
            }
        }
        Ok(())
    }

    fn verify_active_obligations(
        &mut self,
        state: &CanonicalState<'_>,
        active_jobs: &[(B256, OcompJobRecordV1)],
        (pins, pin_by_intent, projection_height): (
            &[CanonicalPinAudit],
            &std::collections::BTreeMap<B256, usize>,
            u64,
        ),
        mut report: Option<&mut super::super::report::ValidationReport>,
    ) -> eyre::Result<Vec<CanonicalActiveAudit>> {
        let mut active = Vec::new();
        for (active_index, (intent_id, listed)) in active_jobs.iter().enumerate() {
            let job = state.metadosis_job(
                *intent_id,
                WorldwideDay::new(listed.intent.wwd),
                listed.finalized.as_ref().map(|f| f.job_id),
            )?;
            let pin = pin_by_intent.get(intent_id).map(|index| &pins[*index]);
            let saved_export = pin.and_then(|pin| pin.authority.export);
            let receipt_present = match &job.finalized {
                Some(finalized) => existing_file(
                    &self
                        .layout
                        .ocomp_root
                        .join("exporter-v1/receipts")
                        .join(hex::encode(finalized.job_id))
                        .join("receipt.ref"),
                )?,
                None => false,
            };
            let export_recorded = saved_export.is_some() || receipt_present;
            self.require_source(&job, !export_recorded)?;
            if let Some(report) = report.as_deref_mut() {
                report.active_ocomp[active_index].source_verified = true;
            }
            let mut export_verified = false;
            if export_recorded {
                self.require_export(&job, saved_export)?;
                export_verified = true;
                if let Some(report) = report.as_deref_mut() {
                    report.active_ocomp[active_index].export_verified = true;
                }
            }
            active.push(CanonicalActiveAudit {
                intent_id: *intent_id,
                projection_before_request: projection_height < job.intent_height,
                job,
                pin_stage: pin_stage(pin),
                source_verified: true,
                export_verified,
            });
        }
        Ok(active)
    }
}

pub(crate) fn verify_canonical_obligations(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    (layout, scratch_parent): (&crate::snapshot::config::RequestedLayout, &Path),
    maximum_records: Option<u64>,
    mut report: Option<&mut super::super::report::ValidationReport>,
) -> eyre::Result<CanonicalOcompAudit> {
    use std::collections::BTreeSet;

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
    let mut inventory = CanonicalInventory::scan_with_report(
        state,
        scratch_parent,
        &protected,
        maximum_records,
        report.as_deref_mut(),
    )?;

    let (_secondary, reader, projection) =
        open_verified_projection_checkpoint(view, layout, scratch_parent, report.as_deref_mut())?;
    let closure = verify_and_record_closure(
        view,
        layout,
        projection,
        maximum_records,
        report.as_deref_mut(),
    )?;

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
    let cas_limits = outbe_ocomp::cas::CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: u64::MAX,
    };

    let (pins, pin_by_intent) = load_and_verify_pins(state, view, layout, maximum_records)?;
    if let Some(report) = report.as_deref_mut() {
        for ((intent_id, _), observation) in
            inventory.active_jobs().iter().zip(&mut report.active_ocomp)
        {
            let pin = pin_by_intent.get(intent_id).map(|index| &pins[*index]);
            observation.pin_stage = format!("{:?}", pin_stage(pin));
        }
    }

    let mut verifier = CanonicalObligationVerifier {
        view,
        layout,
        reader,
        scratch_parent,
        protected: &protected,
        cas_limits,
        maximum_records,
        checked_requests: BTreeSet::new(),
        checked_leases: BTreeSet::new(),
        checked_exports: BTreeSet::new(),
        input_chunks: 0,
    };
    verifier.verify_pin_obligations(&pins)?;
    let active = verifier.verify_active_obligations(
        state,
        inventory.active_jobs(),
        (&pins, &pin_by_intent, projection.block_number),
        report,
    )?;
    let source_leases = u64::try_from(verifier.checked_leases.len())?;
    let complete_exports = u64::try_from(verifier.checked_exports.len())?;
    let input_chunks = verifier.input_chunks;
    let nod = inventory.verify_nod_inputs(
        &layout.ocomp_root,
        cas_limits,
        parameters.nod_materialization_batch_subtree_height,
        maximum_records,
    )?;
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
    })
}
