//! relations obligations for the offline OCOMP audit.
use super::*;

fn seed_canonical_work_union(
    canonical: &canonical::CanonicalOcompAudit,
    work: &PresentJobUnion,
    report: &mut super::super::report::ValidationReport,
) -> eyre::Result<()> {
    report.observed.p = Some(outbe_snapshot::manifest::BlockIdentity {
        number: canonical.projection.block_number,
        hash: hex::encode(canonical.projection.block_hash),
    });
    for (name, count) in [
        ("verified_active_intents", canonical.bounds.active_intents),
        ("verified_closure_frames", canonical.closure.replay.blocks),
        ("verified_source_leases", canonical.source_leases),
        ("verified_complete_exports", canonical.complete_exports),
        ("verified_export_input_chunks", canonical.input_chunks),
        ("verified_pending_nod_jobs", canonical.nod.jobs),
        ("verified_pending_nod_batches", canonical.nod.batches),
        ("verified_pending_nod_actions", canonical.nod.actions),
    ] {
        present_count(report, name, count);
    }
    for active in &canonical.active {
        work.save_job(b'a', &active.job)?;
    }
    for pin in &canonical.pins {
        work.save_job(b'a', &pin.authority.job)?;
        if let (Some(finalized), Some(export)) =
            (&pin.authority.job.finalized, pin.authority.export)
        {
            work.save_export(finalized.job_id, export)?;
        }
    }
    Ok(())
}

fn collect_present_discovery_and_results(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    ocomp_root: &Path,
    work: &PresentJobUnion,
    (errors, report): (
        &mut PresentJoinErrors,
        &mut super::super::report::ValidationReport,
    ),
) -> eyre::Result<()> {
    if let Some(audit) = errors.observe(verify_present_discovery(
        state,
        view,
        ocomp_root,
        None,
        &mut |record, job| {
            if let outbe_ocomp::discovery_spool::DiscoverySpoolRecordV1::Ack(ack) = record {
                work.save_ack(&ack)?;
            }
            if let Some(job) = job {
                work.save_job(b'd', &job)?;
            }
            Ok(())
        },
    )) {
        work.publish_jobs(b'd')?;
        work.publish_evidence(b'k', b'c', PRESENT_ACK)?;
        present_count(report, "present_discovery_records", audit.records);
    }
    if let Some(audit) = errors.observe(verify_present_local_results(
        state,
        view,
        ocomp_root,
        None,
        &mut |job, result| {
            work.save_job(b'l', job)?;
            work.save_result_binding(result.result.job_id, &result.result)
        },
    )) {
        work.publish_jobs(b'l')?;
        work.publish_evidence(b'm', b'v', 0)?;
        present_count(report, "present_local_results", audit.results);
        present_count(report, "present_terminal_digests", audit.terminal_digests);
    }
    Ok(())
}

fn scan_present_job_directories(
    ocomp_root: &Path,
    work: &PresentJobUnion,
    errors: &mut PresentJoinErrors,
    report: &mut super::super::report::ValidationReport,
) {
    for (prefix, flag, name) in [
        (
            "exporter-v1/receipts",
            PRESENT_RECEIPT,
            "receipt_job_directories",
        ),
        (
            "supervisor-v1/export-bindings",
            PRESENT_BINDING,
            "binding_job_directories",
        ),
        (
            "exporter-v1/input-refs",
            PRESENT_INPUTS,
            "input_job_directories",
        ),
        (
            "supervisor-v1/jobs",
            PRESENT_ADMISSIONS,
            "public_job_directories",
        ),
    ] {
        if let Some(count) = errors.observe(scan_present_jobs(&ocomp_root.join(prefix), flag, work))
        {
            present_count(report, name, count);
        }
    }
    if let Some(count) = errors.observe(collect_present_references(ocomp_root, work)) {
        present_count(report, "present_reference_records", count);
    }
}

fn audit_present_job_union(
    context: &PresentJobContext<'_, '_>,
    canonical: Option<&canonical::CanonicalOcompAudit>,
    work: &PresentJobUnion,
    errors: &mut PresentJoinErrors,
    report: &mut super::super::report::ValidationReport,
) -> eyre::Result<()> {
    let mut counts = PresentArtifactCounts::default();
    let mut jobs = 0_u64;
    work.visit_prefix(b"u", &mut |key, value| {
        if key.len() != 33 || value.len() != 1 {
            return Err(SnapshotOcompError::InvalidScratchJobUnion.into());
        }
        let job = B256::from_slice(&key[1..]);
        if let Some(canonical) = canonical {
            let active = canonical.active.iter().any(|active| {
                active
                    .job
                    .finalized
                    .as_ref()
                    .is_some_and(|finalized| finalized.job_id == job)
            });
            errors.observe(verify_present_job(
                context,
                job,
                value[0],
                active,
                &mut counts,
            ));
        }
        jobs = jobs
            .checked_add(1)
            .ok_or(SnapshotOcompError::PresentJobUnionOverflow)?;
        Ok(())
    })?;
    present_count(report, "present_job_union", jobs);
    for (name, count) in [
        ("present_receipts", counts.receipts),
        ("present_bindings", counts.bindings),
        ("present_input_chunks", counts.inputs),
        ("present_admissions", counts.admissions),
        ("present_result_chunks", counts.results),
        ("verified_materialization_references", counts.references),
    ] {
        present_count(report, name, count);
    }
    Ok(())
}

/// Final selected OCOMP composition. Caller records the Ocomp status.
pub(crate) fn verify_ocomp_relations(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    scratch_parent: &Path,
    report: &mut super::super::report::ValidationReport,
) -> eyre::Result<()> {
    let limits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: u64::MAX,
    };
    let mut errors = PresentJoinErrors::default();
    let canonical = errors.observe(verify_canonical_obligations(
        state,
        view,
        (layout, scratch_parent),
        None,
        Some(report),
    ));
    let mut protected = layout.protected.clone();
    protected.0.extend([
        layout.chain_root.clone(),
        layout.consensus_root.clone(),
        layout.ocomp_root.clone(),
        layout.static_files_root.clone(),
        layout.execution_rocksdb_root.clone(),
    ]);
    protected
        .0
        .extend(layout.projection.as_ref().map(|p| p.root.clone()));
    let work = PresentJobUnion::create(scratch_parent, &protected)?;
    if let Some(canonical) = &canonical {
        seed_canonical_work_union(canonical, &work, report)?;
    }
    collect_present_discovery_and_results(
        state,
        view,
        &layout.ocomp_root,
        &work,
        (&mut errors, report),
    )?;
    if let Some(audit) = errors.observe(verify_present_cas(&layout.ocomp_root, limits, None)) {
        present_count(report, "present_cas_objects", audit.objects);
        present_count(report, "present_cas_bytes", audit.bytes);
    }
    scan_present_job_directories(&layout.ocomp_root, &work, &mut errors, report);
    let context = PresentJobContext {
        state,
        view,
        layout,
        scratch: scratch_parent,
        protected: &protected,
        work: &work,
        cas_limits: limits,
    };
    audit_present_job_union(&context, canonical.as_ref(), &work, &mut errors, report)?;
    if let Some((live, retained)) =
        errors.observe(verify_present_projection_structure(layout, scratch_parent))
    {
        present_count(report, "ocomp_live_body_records", live);
        present_count(report, "ocomp_retained_body_records", retained);
    }
    errors.finish()
}

pub(super) fn collect_present_references(root: &Path, work: &PresentJobUnion) -> eyre::Result<u64> {
    use outbe_ocomp::nod_materialization::{
        MaterializationReferenceErrorV1, MaterializationReferenceReaderV1,
    };
    let path = root.join("supervisor-v1/materialization-references");
    if !existing_directory(&path)? {
        return Ok(0);
    }
    let reader = MaterializationReferenceReaderV1::open_existing(path)?;
    let mut callback_error = None;
    let mut count = 0_u64;
    let result = reader.visit_references(&mut |job, ordinal, refs| {
        let result = (|| -> eyre::Result<()> {
            work.save_refs(job, ordinal, &refs)?;
            count = count
                .checked_add(1)
                .ok_or(SnapshotOcompError::ReferenceRecordCountOverflow)?;
            Ok(())
        })();
        result.map_err(|error| {
            callback_error = Some(error);
            MaterializationReferenceErrorV1::InvalidRecord
        })
    });
    if let Some(error) = callback_error {
        return Err(error);
    }
    result?;
    Ok(count)
}

#[derive(Default)]
pub(super) struct PresentArtifactCounts {
    pub(super) receipts: u64,
    pub(super) bindings: u64,
    pub(super) inputs: u64,
    pub(super) admissions: u64,
    pub(super) results: u64,
    pub(super) references: u64,
}
pub(super) fn increment_present(count: &mut u64, add: u64) -> eyre::Result<()> {
    *count = count
        .checked_add(add)
        .ok_or(SnapshotOcompError::PresentArtifactCountOverflow)?;
    Ok(())
}

pub(super) fn validate_present_manifest(
    manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    job: &OcompJobRecordV1,
    bundle: &PinnedProtocolBundle,
) -> eyre::Result<()> {
    let finalized = job
        .finalized
        .as_ref()
        .ok_or(SnapshotOcompError::PresentManifestLacksFinalizedJob)?;
    require(
        (
            manifest.job_id,
            manifest.protocol_bundle_hash,
            manifest.attempt,
            manifest.wwd,
            manifest.sealed_tribute_collection_key,
            manifest.sealed_tribute_collection_root,
            manifest.tribute_count,
            manifest.tribute_nominal_total,
        ) == (
            finalized.job_id,
            job.intent.protocol_bundle_hash,
            job.intent.attempt,
            job.intent.wwd,
            job.intent.sealed_tribute_collection_key,
            job.intent.sealed_tribute_collection_root,
            job.intent.authenticated_day_count,
            job.intent.authenticated_day_nominal,
        ),
        SnapshotOcompError::PresentManifestAuthorityMismatch,
    )?;
    let checkpoint = &manifest.checkpoint;
    require(
        (
            checkpoint.finalized_block_number,
            checkpoint.finalized_block_hash,
            checkpoint.finalized_state_root,
            checkpoint.finalized_ce_root,
            checkpoint.ce_schema_version,
        ) == (
            job.intent_height,
            finalized.finalized_request_block_hash,
            finalized.finalized_request_state_root,
            job.intent.ce_sealed_root,
            u16::try_from(outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION)?,
        ),
        SnapshotOcompError::PresentManifestCheckpointMismatch,
    )?;
    manifest.validate_against_bundle(bundle.bundle(), &poc_schema_limits())?;
    Ok(())
}

pub(super) struct PresentJobContext<'a, 'state> {
    pub(super) state: &'a CanonicalState<'state>,
    pub(super) view: &'a crate::snapshot::native::RethReadOnlyView,
    pub(super) layout: &'a crate::snapshot::config::RequestedLayout,
    pub(super) scratch: &'a Path,
    pub(super) protected: &'a ProtectedPaths,
    pub(super) work: &'a PresentJobUnion,
    pub(super) cas_limits: CasLimits,
}

pub(super) fn compare_surviving_ack_export(
    ack: &outbe_ocomp::discovery_spool::StoredDiscoveryAckV1,
    export: outbe_node::ocomp::retention::ExportAuthorityV1,
) -> eyre::Result<()> {
    if ack.reference.generation != export.source_generation
        || ack.lease_generation != export.lease_generation
        || ack.manifest_hash != export.manifest_hash
    {
        return Err(SnapshotOcompError::SurvivingAckExportMismatch.into());
    }
    Ok(())
}

pub(super) fn compare_surviving_result_binding(
    result: (B256, B256),
    manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    plan_hash: Option<B256>,
) -> eyre::Result<()> {
    if result.0 != manifest.manifest_hash(&poc_schema_limits())? {
        return Err(SnapshotOcompError::SurvivingLocalResultManifestMismatch.into());
    }
    if plan_hash.is_some_and(|expected| result.1 != expected) {
        return Err(SnapshotOcompError::SurvivingLocalResultPlanMismatch.into());
    }
    Ok(())
}
