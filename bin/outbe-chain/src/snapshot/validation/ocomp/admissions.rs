//! admissions obligations for the offline OCOMP audit.
use super::*;

#[derive(Debug, Default)]
pub(crate) struct NodInputsAudit {
    pub jobs: u64,
    pub batches: u64,
    pub actions: u64,
}

#[derive(Debug)]
pub(crate) struct ExportInputsAudit {
    #[cfg(all(test, feature = "snapshot-integration"))]
    pub receipt: outbe_ocomp::export_receipt::VerifiedExportReceipt,
    #[cfg(all(test, feature = "snapshot-integration"))]
    pub binding: outbe_ocomp::export_binding::VerifiedExportedManifestBinding,
    pub input_chunks: u64,
}

#[derive(Debug)]
pub(crate) struct LocalResultAudit {
    pub result: outbe_ocomp_protocol::result::LysisResultV1,
    pub terminal_digest_checked: bool,
}

#[derive(Debug)]
pub(crate) struct AdmissionAudit {
    pub expected: u32,
    pub present: u32,
    pub result_chunks: u32,
}

/// Validate every present admission without inventing completed local work.
/// The caller supplies canonical export and frozen plan inputs. Emitted refs
/// are provisional until the entire walk succeeds, and prove item membership,
/// not reexecution of the worker program or complete result-catalog closure.
pub(crate) fn verify_present_admissions(
    ocomp_root: &Path,
    expected_manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    expected_lysis_limit: U256,
    expected_evaluation_time: u64,
    cas_limits: CasLimits,
    maximum_records: Option<u64>,
    visitor: &mut impl FnMut(&outbe_ocomp_protocol::CasObjectRefV1) -> eyre::Result<()>,
) -> eyre::Result<AdmissionAudit> {
    use outbe_lysis::program_v1::planner::{LysisPlanTopologyV1, PlannedUnitPositionV1};
    use outbe_ocomp::lysis_result_catalog::verified_result_chunk_at;
    use outbe_ocomp_protocol::unit::{UnitArtifactV1, UnitPhase};

    let mut check = || -> eyre::Result<AdmissionAudit> {
        let limits = poc_schema_limits();
        let job = hex::encode(expected_manifest.job_id);
        let cas = FilesystemCasReader::open(ocomp_root.join("cas-v1"), cas_limits)?;
        let bundle = read_pinned_bundle(ocomp_root, expected_manifest.protocol_bundle_hash)?;
        let inputs = VerifiedInputChunkRefCatalog::reopen(
            ocomp_root.join("exporter-v1/input-refs").join(&job),
            &cas,
            limits,
            poc_input_list_limits(),
        )?;
        let root = ocomp_root
            .join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions");
        let admissions = AdmissionCatalogReader::open_existing(&root, &cas, limits)?;
        let audit =
            LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &bundle, &limits)?;
        ensure!(
            audit.manifest() == expected_manifest
                && audit.plan().lysis_limit_minor == expected_lysis_limit
                && audit.plan().logical_evaluation_time == expected_evaluation_time,
            "local plan differs from canonical export or frozen inputs"
        );
        let plan = audit.plan();
        let plan_hash = plan.plan_hash(&limits)?;
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
        let mut counts = AdmissionAudit {
            expected: topology.total_unit_count(),
            present: 0,
            result_chunks: 0,
        };
        // The native complete cursor demands all ordinals. Walk only present
        // locators here, retaining native record decoding and item validation.
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| eyre::eyre!("invalid admission locator"))?;
            if matches!(name, "catalog.header" | "catalog.lock") {
                continue;
            }
            let ordinal: u32 = name
                .strip_suffix(".admission")
                .ok_or_else(|| eyre::eyre!("unexpected admission entry {name}"))?
                .parse()?;
            ensure!(
                name == format!("{ordinal:010}.admission")
                    && ordinal < counts.expected
                    && entry.file_type()?.is_file(),
                "invalid admission locator {name}"
            );
            if maximum_records.is_some_and(|maximum| u64::from(counts.present) >= maximum) {
                return Err(Incomplete(format!(
                    "present admission walk stopped after {} records; plan has {} positions",
                    counts.present, counts.expected
                ))
                .into());
            }
            let record = admissions.read(ordinal)?;
            ensure!(
                record.protocol_bundle_hash == plan.protocol_bundle_hash
                    && record.job_id == plan.job_id
                    && record.attempt == plan.attempt
                    && record.plan_hash == plan_hash
                    && !record.unit_id.is_zero(),
                "present admission differs from plan authority at ordinal {ordinal}"
            );
            match (
                topology.plan_position_at(ordinal)?,
                record.result_chunk.as_ref(),
            ) {
                (
                    PlannedUnitPositionV1::TreeNode {
                        phase: UnitPhase::RootReduce,
                        level: 0,
                        index,
                    },
                    Some(result),
                ) if result.output_manifest_entry.chunk_ordinal == index => {
                    let chunk = verified_result_chunk_at(&audit, index)?;
                    visitor(chunk.producer_artifact_ref())?;
                    visitor(&chunk.output_manifest_entry().result_chunk_ref)?;
                    counts.result_chunks = counts
                        .result_chunks
                        .checked_add(1)
                        .ok_or_else(|| eyre::eyre!("result chunk count overflow"))?;
                }
                (
                    PlannedUnitPositionV1::TreeNode {
                        phase: UnitPhase::RootReduce,
                        level: 0,
                        ..
                    },
                    _,
                ) => eyre::bail!("ROOT_REDUCE leaf lacks its exact admitted result entry"),
                (_, Some(_)) => eyre::bail!("non-leaf admission contains a result entry"),
                (_, None) => {
                    let spec = audit.candidate_spec_at(ordinal)?;
                    ensure!(
                        record.unit_id == spec.unit_id(&limits)?,
                        "present admission UnitId differs at ordinal {ordinal}"
                    );
                    let object = cas.read_verified(&record.artifact_ref)?;
                    let artifact = UnitArtifactV1::decode_canonical(object.bytes(), &limits)?;
                    artifact.validate_against(&spec, &limits)?;
                    visitor(&record.artifact_ref)?;
                }
            }
            counts.present = counts
                .present
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("present admission count overflow"))?;
        }
        Ok(counts)
    };
    check().map_err(|error| {
        if error.downcast_ref::<Incomplete>().is_some() {
            error
        } else if missing_native_input(error.as_ref()) {
            error.wrap_err(Incomplete(
                "missing input for a present OCOMP admission".into(),
            ))
        } else {
            error.wrap_err("present OCOMP admissions")
        }
    })
}

/// Observe an optional local result for an authenticated finalized job. Network
/// completion does not imply that this node produced a local result.
#[cfg(all(test, feature = "snapshot-integration"))]
pub(crate) fn verify_local_result(
    ocomp_root: &Path,
    job: &OcompJobRecordV1,
) -> eyre::Result<Option<LocalResultAudit>> {
    use outbe_node::ocomp::local_result::LocalLysisResultReader;

    let root = ocomp_root.join("node-v1/local-results");
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let finalized = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("local result comparison requires canonical finality"))?;
    let limits = poc_schema_limits();
    let reader = LocalLysisResultReader::open_existing(root, limits)?;
    let Some(loaded) = reader.load(finalized.job_id)? else {
        return Ok(None);
    };
    Ok(Some(verify_loaded_local_result(job, &loaded)?))
}

pub(super) fn verify_loaded_local_result(
    job: &OcompJobRecordV1,
    loaded: &outbe_node::ocomp::local_result::LoadedLocalLysisResultV1,
) -> eyre::Result<LocalResultAudit> {
    use outbe_ocomp_protocol::result::LysisResultV1;
    let finalized = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("local result lacks canonical finality"))?;
    let result = LysisResultV1::decode_canonical(&loaded.canonical_result, &poc_schema_limits())?;
    ensure!(
        result.job_id == finalized.job_id,
        "local result differs from canonical JobId"
    );
    result.validate_finalized_intent(&job.intent)?;
    let completed = job
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.completed_binding.as_ref());
    if let Some(binding) = completed {
        ensure!(
            binding.job_id == finalized.job_id
                && binding.result_digest == loaded.committed.result_digest,
            "local result differs from canonical completed binding"
        );
    }
    Ok(LocalResultAudit {
        result,
        terminal_digest_checked: completed.is_some(),
    })
}

#[derive(Debug, Default)]
pub(crate) struct LocalResultsAudit {
    pub results: u64,
    pub terminal_digests: u64,
}

/// Authenticate every surviving local result, including retired jobs absent
/// from the live canonical inventory. Open the native reader once; its initial
/// whole-directory validation must not be repeated for every result.
/// Callback observations remain provisional until the entire traversal succeeds.
pub(crate) fn verify_present_local_results(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    ocomp_root: &Path,
    maximum_records: Option<u64>,
    visitor: &mut impl FnMut(&OcompJobRecordV1, &LocalResultAudit) -> eyre::Result<()>,
) -> eyre::Result<LocalResultsAudit> {
    use outbe_node::ocomp::local_result::LocalLysisResultReader;
    use outbe_ocomp_protocol::result::LysisResultV1;
    let root = ocomp_root.join("node-v1/local-results");
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalResultsAudit::default());
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let limits = poc_schema_limits();
    let reader = LocalLysisResultReader::open_existing(&root, limits)?;
    let mut counts = LocalResultsAudit::default();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if maximum_records.is_some_and(|maximum| counts.results >= maximum) {
            return Err(Incomplete(format!(
                "local result inventory stopped after {} records",
                counts.results
            ))
            .into());
        }
        // The owner has checked exact native filename/record identity. Decode
        // that locator to use its typed read API, never a caller-supplied path.
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| eyre::eyre!("invalid native result filename"))?;
        let encoded_job = name
            .strip_suffix(".lysis-result-v1.ocb1")
            .ok_or_else(|| eyre::eyre!("invalid native result filename"))?;
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(encoded_job, &mut bytes)?;
        let job_id = B256::from(bytes);
        let loaded = reader
            .load(job_id)?
            .ok_or_else(|| Incomplete(format!("local result disappeared for job {job_id}")))?;
        let result = LysisResultV1::decode_canonical(&loaded.canonical_result, &limits)?;
        let summary = &result.metadosis_completion_summary;
        let job = locate_request_job(
            state,
            view,
            summary.logical_evaluation_height,
            job_id,
            WorldwideDay::new(summary.wwd),
            maximum_records,
        )?;
        let observation = verify_loaded_local_result(&job, &loaded)?;
        visitor(&job, &observation)?;
        counts.results = counts
            .results
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("local result count overflow"))?;
        counts.terminal_digests = counts
            .terminal_digests
            .checked_add(u64::from(observation.terminal_digest_checked))
            .ok_or_else(|| eyre::eyre!("local result digest count overflow"))?;
    }
    Ok(counts)
}
