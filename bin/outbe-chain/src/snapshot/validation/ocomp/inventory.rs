//! inventory obligations for the offline OCOMP audit.
use super::*;

/// Scratch-only sets avoid retaining the permanent series index in RAM.
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
        if !present && !complete_evidence {
            return Err(Incomplete(format!(
                "reference {} has no verified membership in partial job {job}",
                reference.transport_digest
            ))
            .into());
        }
        ensure!(
            present,
            "retained reference does not belong to verified job {job}"
        );
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct InventoryBounds {
    pub active_intents: u64,
    pub nod_head: u64,
    pub nod_tail: u64,
    pub nod_entries: u64,
    pub series: u64,
    pub days: u64,
    pub unpaid_days: u64,
    pub bitmap_words: u64,
}

/// Canonical obligation discovery is independent of local job directories.
/// All observations borrow the same immutable verified E; visiting this inventory
/// does not infer that the corresponding local artifacts have been validated.
pub(crate) struct CanonicalInventory<'a, 'b> {
    pub(super) state: &'a CanonicalState<'b>,
    pub(super) db: DatabaseEnv,
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
    if maximum.is_some_and(|maximum| visited >= maximum) {
        return Err(Incomplete(format!("{label} scan stopped at {visited}/{total}")).into());
    }
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

        if maximum_records.is_some_and(|maximum| active_intents > maximum) {
            return Err(Incomplete(format!(
                "active intent scan requires {active_intents} records, exceeding configured budget"
            ))
            .into());
        }
        let directory = tempfile::Builder::new()
            .prefix("outbe-ocomp-inventory-")
            .tempdir_in(scratch_parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        let tx = db.tx_mut()?;
        let (nod_head, nod_tail) = state.nod_materialization_bounds()?;
        ensure!(
            nod_head > 0 && nod_head <= nod_tail,
            "invalid NOD FIFO bounds"
        );
        observe_inventory_bound(report.as_deref_mut(), "nod_fifo", nod_head, nod_tail, 0);
        ensure!(
            state.nod_materialization_day(nod_tail)?.value() == 0,
            "NOD FIFO next-free tail is occupied"
        );
        let head = state.nod_materialization_head()?;
        ensure!(
            head.is_some() == (nod_head != nod_tail),
            "NOD FIFO head presence differs"
        );
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
            ensure!(
                day.value() != 0 && day.is_valid(),
                "invalid NOD FIFO day at {sequence}"
            );
            let projection = state
                .nod_certified_generation(day)?
                .ok_or_else(|| eyre::eyre!("missing certified NOD generation at {sequence}"))?;
            ensure!(
                projection.worldwide_day == day
                    && !projection.job_id.is_zero()
                    && !projection.protocol_bundle_hash.is_zero()
                    && !projection.program_semantics_hash.is_zero()
                    && projection.next_nod_ordinal < projection.nod_count,
                "invalid or completed NOD generation remains queued at {sequence}"
            );
            if sequence == nod_head {
                ensure!(
                    head.as_ref()
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
                    "NOD FIFO first projection differs from native head"
                );
            }
            for key in [
                inventory_key(b'd', &day.value().to_be_bytes()),
                inventory_key(b'j', projection.job_id.as_slice()),
            ] {
                ensure!(
                    tx.get::<InventoryRows>(key.clone())?.is_none(),
                    "duplicate NOD FIFO day/job"
                );
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

        let total_series = state.intex_total_series()?;
        observe_inventory_bound(report.as_deref_mut(), "intex_series", 0, total_series, 0);
        observe_inventory_bound(report.as_deref_mut(), "intex_days", 0, 0, 0);
        observe_inventory_bound(report.as_deref_mut(), "payout_bitmap_words", 0, 0, 0);
        let mut bitmap_expected = 0_u64;
        let mut bitmap_visited = 0_u64;
        for index in 0..total_series {
            scan_budget("Intex series", index, total_series, maximum_records)?;
            let id = state.intex_series_id_at(index)?;
            let day = verify_series_day(id)?;
            let record = state.intex_read_series(id)?;
            ensure!(
                record.series_id == id && record.worldwide_day == day,
                "Intex series record identity/day differs from permanent index"
            );
            let key = inventory_key(b's', id.as_bytes());
            ensure!(
                tx.get::<InventoryRows>(key.clone())?.is_none(),
                "duplicate Intex series identity"
            );
            tx.put::<InventoryRows>(key, Vec::new())?;
            bounds.series += 1;
            observe_inventory_bound(
                report.as_deref_mut(),
                "intex_series",
                0,
                total_series,
                bounds.series,
            );
            let day_key = inventory_key(b'w', &day.value().to_be_bytes());
            if tx.get::<InventoryRows>(day_key.clone())?.is_some() {
                continue;
            }
            tx.put::<InventoryRows>(day_key, Vec::new())?;
            bounds.days += 1;
            // Distinct days are discovered through the permanent series index;
            // this records the population reached, not an unobserved final count.
            observe_inventory_bound(
                report.as_deref_mut(),
                "intex_days",
                0,
                bounds.days,
                bounds.days,
            );
            let certified = state.intex_certified_contributor_generation(day)?;
            let Some(round) = state.intex_certified_payout_round(day.value())? else {
                continue;
            };
            let certified =
                certified.ok_or_else(|| eyre::eyre!("payout round lacks certified generation"))?;
            ensure!(
                round.wwd == day.value()
                    && round.active != 0
                    && certified.worldwide_day == day.value()
                    && certified.contributor_count > 0
                    && round.paid_so_far <= round.amount,
                "inconsistent certified payout round"
            );
            bitmap_expected = bitmap_expected
                .checked_add(u64::from(certified.contributor_count).div_ceil(256))
                .ok_or_else(|| eyre::eyre!("payout bitmap expected count overflow"))?;
            observe_inventory_bound(
                report.as_deref_mut(),
                "payout_bitmap_words",
                0,
                bitmap_expected,
                bitmap_visited,
            );
            let bitmap = verify_paid_bitmap(
                certified.contributor_count,
                round.paid_leaf_count,
                maximum_records,
                |word| {
                    let value = state.intex_paid_leaves_word(day.value(), word)?;
                    bitmap_visited = bitmap_visited
                        .checked_add(1)
                        .ok_or_else(|| eyre::eyre!("payout bitmap visited count overflow"))?;
                    observe_inventory_bound(
                        report.as_deref_mut(),
                        "payout_bitmap_words",
                        0,
                        bitmap_expected,
                        bitmap_visited,
                    );
                    Ok(value)
                },
            )?;
            bounds.bitmap_words = bounds
                .bitmap_words
                .checked_add(bitmap.words)
                .ok_or_else(|| eyre::eyre!("payout bitmap word count overflow"))?;
            if bitmap.unpaid > 0 {
                tx.put::<InventoryRows>(
                    inventory_key(b'p', &day.value().to_be_bytes()),
                    Vec::new(),
                )?;
                bounds.unpaid_days += 1;
            }
        }
        tx.commit()?;
        Ok(Self {
            state,
            db,
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
                ensure!(
                    bundle.bundle().lysis_program_semantics_hash
                        == projection.program_semantics_hash,
                    "canonical NOD program semantics differ from protocol bundle"
                );
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
                let audit = LocalLysisPlanAuditV1::open_read_only(
                    &admissions,
                    &inputs,
                    &cas,
                    &bundle,
                    &limits,
                )?;
                ensure!(
                    audit.plan().job_id == projection.job_id
                        && audit.plan().protocol_bundle_hash == projection.protocol_bundle_hash
                        && audit.plan().wwd == projection.worldwide_day.value()
                        && audit.plan().tribute_count == projection.tribute_count
                        && projection.nod_count == projection.tribute_count,
                    "canonical NOD generation differs from native plan"
                );
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
                    if maximum_batches.is_some_and(|maximum| result.batches >= maximum) {
                        return Err(Incomplete(format!(
                            "NOD job {job} stopped at {}/{} ordinals after {} batches",
                            head.next_nod_ordinal, head.nod_count, result.batches
                        ))
                        .into());
                    }
                    let built = build_nod_materialization_batch_with_references(
                        &audit,
                        &head,
                        subtree_height,
                    )?;
                    let count = u32::try_from(built.batch.actions.len())?;
                    let next = head
                        .next_nod_ordinal
                        .checked_add(count)
                        .ok_or_else(|| eyre::eyre!("NOD ordinal overflow"))?;
                    ensure!(
                        count > 0 && next <= head.nod_count,
                        "invalid NOD batch progress"
                    );
                    head.next_nod_ordinal = next;
                    result.batches = result
                        .batches
                        .checked_add(1)
                        .ok_or_else(|| eyre::eyre!("NOD batch count overflow"))?;
                    result.actions = result
                        .actions
                        .checked_add(u64::from(count))
                        .ok_or_else(|| eyre::eyre!("NOD action count overflow"))?;
                }
                result.jobs = result
                    .jobs
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("NOD job count overflow"))?;
                Ok(())
            };
            verify().map_err(|error| classify_nod_input_error(error, projection.job_id))
        })?;
        Ok(result)
    }

    /// Check the public handoff for every unpaid canonical day. Intermediate
    /// plans, CE bodies and signing journals are not authority for this file.
    pub(crate) fn verify_payout_files(&self, ocomp_root: &Path) -> eyre::Result<u64> {
        let mut verified = 0_u64;
        self.visit_payouts(&mut |day, certified| {
            let active = self
                .state
                .metadosis_active_lysis_generation(day)?
                .ok_or_else(|| {
                    Incomplete(format!(
                        "missing active Lysis generation for unpaid day {}",
                        day.value()
                    ))
                })?;
            ensure!(
                !active.job_id.is_zero()
                    && active.contributor_root == certified.contributor_root
                    && active.exact_counts.contributor_count == certified.contributor_count,
                "active Lysis generation differs from certified contributors for day {}",
                day.value()
            );
            let path = ocomp_root
                .join("supervisor-v1")
                .join("jobs")
                .join(hex::encode(active.job_id))
                .join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
            match verify_contributor_payout_artifact(&path, &certified) {
                Ok(_) => {}
                Err(PayoutArtifactError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Err(Incomplete(format!(
                        "missing payout file for day {}, job {}: {}",
                        day.value(),
                        active.job_id,
                        path.display()
                    ))
                    .into());
                }
                Err(error) => {
                    return Err(eyre::Report::new(error).wrap_err(format!(
                        "payout file for day {}, job {}",
                        day.value(),
                        active.job_id
                    )))
                }
            }
            verified = verified
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("verified payout count overflow"))?;
            Ok(())
        })?;
        Ok(verified)
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
                .ok_or_else(|| eyre::eyre!("validated NOD generation disappeared"))?;
            visitor(sequence, projection)?;
        }
        Ok(())
    }

    pub(crate) fn visit_payouts(
        &self,
        visitor: &mut impl FnMut(
            WorldwideDay,
            CertifiedContributorGenerationProjection,
        ) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let mut tx = self.db.tx()?;
        // A callback may stream a large payout file. This immutable scratch
        // snapshot has no writer whose growth would need the live-node timeout.
        tx.disable_long_read_transaction_safety();
        let mut cursor = tx.cursor_read::<InventoryRows>()?;
        for row in cursor.walk(Some(vec![b'p']))? {
            let (key, _) = row?;
            if key.first() != Some(&b'p') {
                break;
            }
            let day = WorldwideDay::new(u32::from_be_bytes(key[1..].try_into()?));
            let certified = self
                .state
                .intex_certified_contributor_generation(day)?
                .ok_or_else(|| eyre::eyre!("validated contributor generation disappeared"))?;
            visitor(day, certified)?;
        }
        Ok(())
    }
}
