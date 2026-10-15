//! inventory obligations for the offline OCOMP audit.
use super::*;

/// Scratch-only key/value rows, kept out of RAM.
#[derive(Debug)]
pub(super) struct InventoryRows;
impl Table for InventoryRows {
    const NAME: &'static str = "SnapshotOcompInventory";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for InventoryRows {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        false
    }
}
impl TableSet for InventoryRows {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        Box::new(std::iter::once(Box::new(Self) as Box<dyn TableInfo>))
    }
}

/// Disposable membership of refs emitted by successful native item audits.
/// A set entry is scoped to a job and preserves every native reference field.
pub(crate) struct ReferenceMembership {
    pub(super) db: DatabaseEnv,
    // Release MDBX before removing its external directory.
    pub(super) _directory: tempfile::TempDir,
}

impl ReferenceMembership {
    pub(crate) fn create(scratch_parent: &Path, protected: &ProtectedPaths) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
        let directory = tempfile::Builder::new()
            .prefix("outbe-ocomp-refs-")
            .tempdir_in(scratch_parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        Ok(Self {
            db,
            _directory: directory,
        })
    }

    pub(super) fn key(job: B256, reference: &outbe_ocomp_protocol::CasObjectRefV1) -> Vec<u8> {
        let mut key = Vec::with_capacity(75);
        key.extend_from_slice(job.as_slice());
        key.extend_from_slice(reference.transport_digest.as_slice());
        key.extend_from_slice(&reference.encoded_bytes.to_be_bytes());
        match reference.expected_ocb1_kind {
            None => key.push(0),
            Some(kind) => {
                key.push(1);
                key.extend_from_slice(&kind.to_be_bytes());
            }
        }
        key
    }

    /// Call only for refs emitted by the native plan/result item validation.
    /// Discard the job's membership observations if their enclosing audit fails.
    pub(crate) fn insert(
        &self,
        job: B256,
        reference: &outbe_ocomp_protocol::CasObjectRefV1,
    ) -> eyre::Result<()> {
        let tx = self.db.tx_mut()?;
        tx.put::<InventoryRows>(Self::key(job, reference), Vec::new())?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn verify(
        &self,
        job: B256,
        reference: &outbe_ocomp_protocol::CasObjectRefV1,
        cas: &FilesystemCasReader,
        complete_evidence: bool,
    ) -> eyre::Result<()> {
        cas.read_verified(reference).map_err(|error| {
            if missing_native_input(&error) {
                eyre::Report::new(error).wrap_err(Incomplete(format!(
                    "missing retained CAS object {} for job {job}",
                    reference.transport_digest
                )))
            } else {
                eyre::Report::new(error)
            }
        })?;
        let present = self
            .db
            .tx()?
            .get::<InventoryRows>(Self::key(job, reference))?
            .is_some();
        require(
            present || complete_evidence,
            Incomplete(format!(
                "reference {} has no verified membership in partial job {job}",
                reference.transport_digest
            )),
        )?;
        require(
            present,
            SnapshotOcompError::RetainedReferenceNotInJob { job },
        )?;
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct InventoryBounds {
    pub active_intents: u64,
    pub nod_head: u64,
    pub nod_tail: u64,
    pub nod_entries: u64,
}

/// Canonical obligation discovery is independent of local job directories.
/// All observations borrow the same immutable verified E. Visiting this inventory
/// does not infer that the corresponding local artifacts have been validated.
pub(crate) struct CanonicalInventory<'a, 'b> {
    pub(super) state: &'a CanonicalState<'b>,
    // Keep MDBX alive until before its backing temporary directory is removed.
    pub(super) _db: DatabaseEnv,
    pub(super) active_jobs: Vec<(B256, OcompJobRecordV1)>,
    pub bounds: InventoryBounds,
    pub(super) _directory: tempfile::TempDir,
}

pub(super) fn inventory_key(kind: u8, identity: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + identity.len());
    key.push(kind);
    key.extend_from_slice(identity);
    key
}

pub(super) fn scan_budget(
    label: &str,
    visited: u64,
    total: u64,
    maximum: Option<u64>,
) -> eyre::Result<()> {
    require(
        maximum.is_none_or(|maximum| visited < maximum),
        Incomplete(format!("{label} scan stopped at {visited}/{total}")),
    )?;
    Ok(())
}

/// Keep observations from successful reads even when a later relation fails.
/// A visited count is not a passed check or a claim about an unvisited suffix.
pub(super) fn observe_inventory_bound(
    report: Option<&mut super::super::report::ValidationReport>,
    name: &str,
    start: u64,
    end_exclusive: u64,
    visited: u64,
) {
    let Some(report) = report else {
        return;
    };
    if let Some(bound) = report
        .inventory_bounds
        .iter_mut()
        .find(|bound| bound.name == name)
    {
        bound.start = start;
        bound.end_exclusive = end_exclusive;
        bound.visited = visited;
    } else {
        report
            .inventory_bounds
            .push(super::super::report::InventoryBounds {
                name: name.into(),
                start,
                end_exclusive,
                visited,
            });
    }
}

impl<'a, 'b> CanonicalInventory<'a, 'b> {
    #[cfg(all(test, feature = "snapshot-integration"))]
    pub(crate) fn scan(
        state: &'a CanonicalState<'b>,
        scratch_parent: &Path,
        protected: &ProtectedPaths,
        maximum_records: Option<u64>,
    ) -> eyre::Result<Self> {
        Self::scan_with_report(state, scratch_parent, protected, maximum_records, None)
    }

    pub(super) fn scan_with_report(
        state: &'a CanonicalState<'b>,
        scratch_parent: &Path,
        protected: &ProtectedPaths,
        maximum_records: Option<u64>,
        mut report: Option<&mut super::super::report::ValidationReport>,
    ) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
        // The owner bounds its native aggregate before allocation and validates
        // live scheduler/FSM/job equivalence. No local directory seeds this list.
        let active_jobs = state.live_ocomp_jobs()?;
        let active_intents = u64::try_from(active_jobs.len())?;
        if let Some(report) = report.as_deref_mut() {
            report.active_ocomp = active_jobs
                .iter()
                .map(
                    |(intent_id, job)| super::super::report::ActiveOcompObservation {
                        intent_id: hex::encode(intent_id),
                        job_id: job.finalized.as_ref().map(|job| hex::encode(job.job_id)),
                        request_height: job.intent_height,
                        worldwide_day: job.intent.wwd,
                        canonical_status: format!("{:?}", job.status),
                        pin_stage: "NotInspected".into(),
                        projection_before_request: None,
                        source_verified: false,
                        export_verified: false,
                    },
                )
                .collect();
        }
        observe_inventory_bound(
            report.as_deref_mut(),
            "active_intents",
            0,
            active_intents,
            active_intents,
        );

        require(
            maximum_records.is_none_or(|maximum| active_intents <= maximum),
            Incomplete(format!(
                "active intent scan requires {active_intents} records, exceeding configured budget"
            )),
        )?;
        let directory = tempfile::Builder::new()
            .prefix("outbe-ocomp-inventory-")
            .tempdir_in(scratch_parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        let tx = db.tx_mut()?;
        let (nod_head, nod_tail) = state.nod_materialization_bounds()?;
        require(
            nod_head != 0 && nod_head <= nod_tail,
            SnapshotOcompError::InvalidNodFifoBounds,
        )?;
        observe_inventory_bound(report.as_deref_mut(), "nod_fifo", nod_head, nod_tail, 0);
        require(
            state.nod_materialization_day(nod_tail)?.value() == 0,
            SnapshotOcompError::NodFifoTailOccupied,
        )?;
        let head = state.nod_materialization_head()?;
        require(
            head.is_some() == (nod_head != nod_tail),
            SnapshotOcompError::NodFifoHeadPresenceMismatch,
        )?;
        let mut bounds = InventoryBounds {
            active_intents,
            nod_head,
            nod_tail,
            ..Default::default()
        };
        for sequence in nod_head..nod_tail {
            scan_budget(
                "NOD FIFO",
                bounds.nod_entries,
                nod_tail - nod_head,
                maximum_records,
            )?;
            let day = state.nod_materialization_day(sequence)?;
            require(
                day.value() != 0 && day.is_valid(),
                SnapshotOcompError::InvalidNodFifoDay { sequence },
            )?;
            let projection = state
                .nod_certified_generation(day)?
                .ok_or(SnapshotOcompError::MissingCertifiedNodGeneration { sequence })?;
            require(
                (
                    projection.worldwide_day == day,
                    !projection.job_id.is_zero(),
                    !projection.protocol_bundle_hash.is_zero(),
                    !projection.program_semantics_hash.is_zero(),
                    projection.next_nod_ordinal < projection.nod_count,
                ) == (true, true, true, true, true),
                SnapshotOcompError::InvalidOrCompletedNodGenerationQueued { sequence },
            )?;
            require(
                sequence != nod_head
                    || head.as_ref()
                        == Some(&NodMaterializationHeadV1 {
                            queue_sequence: sequence,
                            job_id: projection.job_id,
                            program_semantics_hash: projection.program_semantics_hash,
                            worldwide_day: day.value(),
                            generation: projection.generation,
                            nod_root: projection.nod_root,
                            nod_count: projection.nod_count,
                            next_nod_ordinal: projection.next_nod_ordinal,
                            last_progress_height: projection.last_progress_height,
                        }),
                SnapshotOcompError::NodFifoFirstProjectionMismatch,
            )?;
            for key in [
                inventory_key(b'd', &day.value().to_be_bytes()),
                inventory_key(b'j', projection.job_id.as_slice()),
            ] {
                require(
                    tx.get::<InventoryRows>(key.clone())?.is_none(),
                    SnapshotOcompError::DuplicateNodFifoDayOrJob,
                )?;
                tx.put::<InventoryRows>(key, Vec::new())?;
            }
            bounds.nod_entries += 1;
            observe_inventory_bound(
                report.as_deref_mut(),
                "nod_fifo",
                nod_head,
                nod_tail,
                bounds.nod_entries,
            );
        }

        tx.commit()?;
        Ok(Self {
            state,
            _db: db,
            active_jobs,
            bounds,
            _directory: directory,
        })
    }

    pub(crate) fn active_jobs(&self) -> &[(B256, OcompJobRecordV1)] {
        &self.active_jobs
    }

    /// Prove the remaining canonical NOD inputs with the same pure batch builder
    /// used by the node. This advances only a local copy of each queue cursor.
    pub(crate) fn verify_nod_inputs(
        &self,
        ocomp_root: &Path,
        cas_limits: CasLimits,
        subtree_height: u8,
        maximum_batches: Option<u64>,
    ) -> eyre::Result<NodInputsAudit> {
        let mut result = NodInputsAudit::default();
        self.visit_nod(&mut |sequence, projection| {
            let mut verify = || -> eyre::Result<()> {
                let limits = poc_schema_limits();
                let job = hex::encode(projection.job_id);
                let cas = FilesystemCasReader::open(ocomp_root.join("cas-v1"), cas_limits)?;
                let bundle = read_pinned_bundle(ocomp_root, projection.protocol_bundle_hash)?;
                require(
                    bundle.bundle().lysis_program_semantics_hash
                        == projection.program_semantics_hash,
                    SnapshotOcompError::NodProgramSemanticsMismatch,
                )?;
                let inputs = VerifiedInputChunkRefCatalog::reopen(
                    ocomp_root.join("exporter-v1/input-refs").join(&job),
                    &cas,
                    limits,
                    poc_input_list_limits(),
                )?;
                let admissions = AdmissionCatalogReader::open_existing(
                    ocomp_root
                        .join("supervisor-v1/jobs")
                        .join(&job)
                        .join("admissions"),
                    &cas,
                    limits,
                )?;
                let audit =
                    outbe_ocomp::lysis_plan_audit::open_read_only_local_plan_integrity_audit(
                        &admissions,
                        &inputs,
                        &cas,
                        &bundle,
                        &limits,
                    )?;
                require(
                    (
                        audit.plan().job_id,
                        audit.plan().protocol_bundle_hash,
                        audit.plan().wwd,
                        audit.plan().tribute_count,
                        projection.nod_count,
                    ) == (
                        projection.job_id,
                        projection.protocol_bundle_hash,
                        projection.worldwide_day.value(),
                        projection.tribute_count,
                        projection.tribute_count,
                    ),
                    SnapshotOcompError::NodGenerationPlanMismatch,
                )?;
                let mut head = NodMaterializationHeadV1 {
                    queue_sequence: sequence,
                    job_id: projection.job_id,
                    program_semantics_hash: projection.program_semantics_hash,
                    worldwide_day: projection.worldwide_day.value(),
                    generation: projection.generation,
                    nod_root: projection.nod_root,
                    nod_count: projection.nod_count,
                    next_nod_ordinal: projection.next_nod_ordinal,
                    last_progress_height: projection.last_progress_height,
                };
                while head.next_nod_ordinal < head.nod_count {
                    require(
                        maximum_batches.is_none_or(|maximum| result.batches < maximum),
                        Incomplete(format!(
                            "NOD job {job} stopped at {}/{} ordinals after {} batches",
                            head.next_nod_ordinal, head.nod_count, result.batches
                        )),
                    )?;
                    let built = build_nod_materialization_batch_with_references(
                        &audit,
                        &head,
                        subtree_height,
                    )?;
                    let count = u32::try_from(built.batch.actions.len())?;
                    let next = head
                        .next_nod_ordinal
                        .checked_add(count)
                        .ok_or(SnapshotOcompError::NodOrdinalOverflow)?;
                    require(
                        count != 0 && next <= head.nod_count,
                        SnapshotOcompError::InvalidNodBatchProgress,
                    )?;
                    head.next_nod_ordinal = next;
                    result.batches = result
                        .batches
                        .checked_add(1)
                        .ok_or(SnapshotOcompError::NodBatchCountOverflow)?;
                    result.actions = result
                        .actions
                        .checked_add(u64::from(count))
                        .ok_or(SnapshotOcompError::NodActionCountOverflow)?;
                }
                result.jobs = result
                    .jobs
                    .checked_add(1)
                    .ok_or(SnapshotOcompError::NodJobCountOverflow)?;
                Ok(())
            };
            verify().map_err(|error| classify_nod_input_error(error, projection.job_id))
        })?;
        Ok(result)
    }

    pub(crate) fn visit_nod(
        &self,
        visitor: &mut impl FnMut(u64, NodCertifiedGenerationProjection) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        for sequence in self.bounds.nod_head..self.bounds.nod_tail {
            let day = self.state.nod_materialization_day(sequence)?;
            let projection = self
                .state
                .nod_certified_generation(day)?
                .ok_or(SnapshotOcompError::ValidatedNodGenerationDisappeared)?;
            visitor(sequence, projection)?;
        }
        Ok(())
    }
}
