//! Offline OCOMP observations at the verified current canonical state.

use alloy_primitives::{B256, U256};
use eyre::ensure;
use outbe_intex::schema::SeriesId;
use outbe_primitives::time::WorldwideDay;

use super::Incomplete;

use super::canonical_state::CanonicalState;
use outbe_intex::schema::CertifiedContributorGenerationProjection;
use outbe_nod::schema::NodCertifiedGenerationProjection;
use outbe_ocomp::payout_artifact::{
    verify_contributor_payout_artifact, PayoutArtifactError, CONTRIBUTOR_PAYOUT_ARTIFACT_FILE,
};
use outbe_ocomp::{
    admission_catalog::AdmissionCatalogReader,
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::poc_input_list_limits,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    lysis_plan_audit::LocalLysisPlanAuditV1,
    nod_materialization::build_nod_materialization_batch_with_references,
};
use outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1;
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_snapshot::layout::{validate_layout, ProtectedPaths};
use reth_ethereum::provider::db::{
    cursor::DbCursorRO,
    database::Database,
    mdbx::{create_db, DatabaseArguments},
    table::{Table, TableInfo},
    transaction::{DbTx, DbTxMut},
    DatabaseEnv, TableSet,
};
use std::path::Path;

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

fn verify_loaded_local_result(
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
    view: &super::super::native::RethReadOnlyView,
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

/// Check a complete public export against an already authenticated canonical
/// job. This compares saved authorities; it never replays an export operation.
pub(crate) fn verify_export_inputs(
    ocomp_root: &Path,
    job: &OcompJobRecordV1,
    expected_export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
    cas_limits: CasLimits,
) -> eyre::Result<ExportInputsAudit> {
    use outbe_ocomp::{
        export_binding::ExportedManifestBindingReader,
        export_receipt::{ExportReceiptError, ExportReceiptReader},
    };
    use outbe_ocomp_protocol::input::CheckpointIdentityV1;
    let check = || -> eyre::Result<ExportInputsAudit> {
        let limits = poc_schema_limits();
        let finalized = job.finalized.as_ref().ok_or_else(|| {
            eyre::eyre!("complete export lacks canonical finalized job authority")
        })?;
        let spec = canonical_job_spec(job)?;
        let job_hex = hex::encode(finalized.job_id);
        let cas = FilesystemCasReader::open(ocomp_root.join("cas-v1"), cas_limits)?;
        let bundle = read_pinned_bundle(ocomp_root, job.intent.protocol_bundle_hash)?;
        let receipt = ExportReceiptReader::try_open(
            ocomp_root.join("exporter-v1/receipts"),
            finalized.job_id,
            limits,
        )?
        .ok_or(ExportReceiptError::MissingReceipt)?
        .load_exact(&cas)?;
        let inputs = VerifiedInputChunkRefCatalog::reopen(
            ocomp_root.join("exporter-v1/input-refs").join(&job_hex),
            &cas,
            limits,
            poc_input_list_limits(),
        )?;
        let binding = ExportedManifestBindingReader::open_existing(
            ocomp_root
                .join("supervisor-v1/export-bindings")
                .join(&job_hex),
            limits,
        )?
        .load_exact(&cas, &spec, bundle.bundle(), &inputs)?;
        let checkpoint = CheckpointIdentityV1 {
            finalized_block_number: job.intent_height,
            finalized_block_hash: finalized.finalized_request_block_hash,
            finalized_state_root: finalized.finalized_request_state_root,
            finalized_ce_root: job.intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )?,
        };
        ensure!(
            receipt.checkpoint() == &checkpoint && binding.manifest().checkpoint == checkpoint,
            "export checkpoint differs from canonical request or CE schema"
        );
        ensure!(
            binding.commit_replay_request() == receipt.commit_replay_request(),
            "receipt and binding describe different export authorities"
        );
        binding.require_exact_node_replay(&receipt.committed())?;
        if let Some(expected) = expected_export {
            ensure!(
                expected.source_generation == receipt.source_pin_generation()
                    && expected.lease_generation == receipt.lease_generation()
                    && expected.manifest_hash == receipt.manifest_hash(),
                "saved pin export differs from receipt authority"
            );
        }
        // load_exact already consumes the native exact verified input cursor.
        let input_chunks = u64::from(binding.manifest().input_chunk_count);
        Ok(ExportInputsAudit {
            #[cfg(all(test, feature = "snapshot-integration"))]
            receipt,
            #[cfg(all(test, feature = "snapshot-integration"))]
            binding,
            input_chunks,
        })
    };
    check().map_err(|error| {
        if missing_native_input(error.as_ref()) {
            error.wrap_err(Incomplete("missing required OCOMP export input".into()))
        } else {
            error.wrap_err("OCOMP export inputs")
        }
    })
}

fn canonical_job_spec(
    job: &OcompJobRecordV1,
) -> eyre::Result<outbe_ocomp_protocol::control::FinalizedJobSpecV1> {
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
    };
    let finalized = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("job spec lacks canonical finality"))?;
    let limits = poc_schema_limits();
    let spec = FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: job.intent_height,
            job_id: finalized.job_id,
            intent_id: job.intent.intent_id(&limits)?,
            finalized_block_hash: finalized.finalized_request_block_hash,
            finalized_state_root: finalized.finalized_request_state_root,
            protocol_bundle_hash: job.intent.protocol_bundle_hash,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
        },
        canonical_job_intent: BoundedBytes(job.intent.encode_canonical(&limits)?),
    };
    spec.encode_body(&limits)?;
    Ok(spec)
}

#[derive(Debug, Default)]
pub(crate) struct DiscoveryAudit {
    pub spools: u64,
    pub records: u64,
    pub offers: u64,
}

/// Decode complete surviving native spools and bind each full offer to immutable
/// current-E authority. Old absent spools are optional. Other native record stages
/// retain their identity/status for the caller's relation checks; this reader does
/// not require or recreate a retired export merely because an offer survives.
pub(crate) fn verify_present_discovery(
    state: &CanonicalState<'_>,
    view: &super::super::native::RethReadOnlyView,
    ocomp_root: &Path,
    maximum_records: Option<u64>,
    visitor: &mut impl FnMut(
        outbe_ocomp::discovery_spool::DiscoverySpoolRecordV1,
        Option<OcompJobRecordV1>,
    ) -> eyre::Result<()>,
) -> eyre::Result<DiscoveryAudit> {
    use outbe_ocomp::discovery_spool::{
        DiscoverySpoolError, DiscoverySpoolReaderV1, DiscoverySpoolRecordV1,
    };
    use outbe_ocomp_protocol::intent::JobIntentV1;
    let root = ocomp_root.join("exporter-v1/discovery");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiscoveryAudit::default())
        }
        Err(error) => return Err(error.into()),
    };
    let mut counts = DiscoveryAudit::default();
    let limits = poc_schema_limits();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name == "closure-checkpoint-v1" {
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| eyre::eyre!("invalid discovery bundle locator"))?;
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(name, &mut bytes)?;
        let bundle = B256::from(bytes);
        let reader = DiscoverySpoolReaderV1::open_existing(
            entry.path(),
            view.chain.chain().id(),
            view.chain.genesis_hash(),
            limits,
        )?;
        let mut callback_error = None;
        let result = reader.visit_records(&mut |record| {
            let inspect = || -> eyre::Result<()> {
                if maximum_records.is_some_and(|maximum| counts.records >= maximum) {
                    return Err(Incomplete(format!(
                        "discovery walk stopped after {} records",
                        counts.records
                    ))
                    .into());
                }
                let authority = if let DiscoverySpoolRecordV1::Offer(offer) = &record {
                    let intent =
                        JobIntentV1::decode_canonical(&offer.spec.canonical_job_intent.0, &limits)?;
                    ensure!(
                        intent.protocol_bundle_hash == bundle,
                        "discovery offer differs from bundle locator"
                    );
                    let job = state.metadosis_job(
                        offer.spec.summary.intent_id,
                        WorldwideDay::new(intent.wwd),
                        Some(offer.spec.summary.job_id),
                    )?;
                    ensure!(
                        offer.spec == canonical_job_spec(&job)?,
                        "discovery offer differs from canonical job spec"
                    );
                    counts.offers = counts
                        .offers
                        .checked_add(1)
                        .ok_or_else(|| eyre::eyre!("discovery offer count overflow"))?;
                    Some(job)
                } else {
                    None
                };
                visitor(record, authority)?;
                counts.records = counts
                    .records
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("discovery record count overflow"))?;
                Ok(())
            };
            inspect().map_err(|error| {
                callback_error = Some(error);
                DiscoverySpoolError::InvalidIdentity
            })
        });
        if let Some(error) = callback_error {
            return Err(error);
        }
        result?;
        counts.spools = counts
            .spools
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("discovery spool count overflow"))?;
    }
    Ok(counts)
}

pub(crate) struct VerifiedPin {
    pub candidate: outbe_node::ocomp::retention::CandidatePinV1,
    pub job: OcompJobRecordV1,
    /// Retention required by this local pin only. A false value cannot waive
    /// independent canonical active-job or shared-lease obligations.
    pub requires_source: bool,
    pub export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
}

/// Bind a natively decoded journal record to immutable current-E authority.
/// Local lifecycle progress may lag canonical completion; it is not a claim
/// that the donor executed, exported or materialized the completed job.
pub(crate) fn verify_pin_authority(
    state: &CanonicalState<'_>,
    view: &super::super::native::RethReadOnlyView,
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
    ensure!(
        key == candidate.block_hash,
        "retention journal key differs from request block hash"
    );
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
    ensure!(
        job.intent_height == candidate.block_number
            && job.intent.ce_sealed_root == candidate.ce_sealed_root
            && job.intent.protocol_bundle_hash == candidate.protocol_bundle_hash
            && job.intent.input_lease_id()? == candidate.input_lease_id,
        "retained candidate differs from canonical request or input lease"
    );
    // The canonical getter validates finalized B bindings. Awaiting pins also
    // need the retained request header even before canonical finality exists.
    let number = candidate.block_number;
    let header = view
        .header(number)?
        .ok_or_else(|| Incomplete(format!("missing retained pin request header B={number}")))?;
    let canonical_hash = view
        .canonical_hash(number)?
        .ok_or_else(|| Incomplete(format!("missing canonical pin request hash B={number}")))?;
    ensure!(
        header.inner.number == number
            && header.hash_slow() == canonical_hash
            && canonical_hash == candidate.block_hash
            && header.inner.state_root == candidate.state_root,
        "retained candidate differs from canonical request header B={number}"
    );
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
                .ok_or_else(|| eyre::eyre!("retained finalized pin lacks canonical finality"))?;
            ensure!(
                finalized.finality_recorded_height == finality_recorded_height
                    && finalized.open_height == open_height
                    && finalized.deadline_height == deadline_height,
                "retained pin finality window differs from canonical finality"
            );
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
/// Callers decide which leases remain required; historical GC alone does not
/// create a requirement to reproduce a released population.
pub(crate) fn verify_lease_inputs(
    reader: outbe_offchain_storage::StorageReaderHandle,
    intent: &outbe_ocomp_protocol::intent::JobIntentV1,
    scratch_parent: &Path,
    protected: &ProtectedPaths,
    maximum_records: Option<u64>,
) -> eyre::Result<outbe_ocomp::exporter::TributeStreamSummary> {
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
            eyre::Report::new(error).wrap_err(Incomplete(format!(
                "missing required Tribute inputs for lease {}, day {}",
                pin.input_lease_id, intent.wwd
            )))
        } else {
            eyre::Report::new(error).wrap_err(format!(
                "Tribute inputs for lease {}, day {}",
                pin.input_lease_id, intent.wwd
            ))
        }
    };
    let source = FinalizedTributeSource::new(reader, outbe_offchain_storage::MAX_SCAN_ENTRIES)
        .map_err(&classify)?;
    let mut stream = source
        .reconstruction_stream(
            pin,
            intent.authenticated_day_count,
            intent.authenticated_day_nominal,
        )
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
    while let Some(record) = stream.next_record().map_err(&classify)? {
        if maximum_records.is_some_and(|maximum| visited >= maximum) {
            return Err(Incomplete(format!(
                "Tribute lease {} scan stopped at {visited}/{} records",
                pin.input_lease_id, intent.authenticated_day_count
            ))
            .into());
        }
        partition.push(
            record.tribute_id,
            Commitment::try_from(record.commitment.0)?,
        )?;
        visited = visited
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("Tribute visit count overflow"))?;
    }
    // Count underflow is unavailable input, whereas a complete but different
    // population is a failed root/nominal comparison. Keep that distinction.
    let summary = stream.finish().map_err(classify)?;
    partition.finish()?;
    Ok(summary)
}

/// Scratch-only sets avoid retaining the permanent series index in RAM.
#[derive(Debug)]
struct InventoryRows;
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
    db: DatabaseEnv,
    // Release MDBX before removing its external directory.
    _directory: tempfile::TempDir,
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

    fn key(job: B256, reference: &outbe_ocomp_protocol::CasObjectRefV1) -> Vec<u8> {
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
    state: &'a CanonicalState<'b>,
    db: DatabaseEnv,
    active_jobs: Vec<(B256, OcompJobRecordV1)>,
    pub bounds: InventoryBounds,
    _directory: tempfile::TempDir,
}

fn inventory_key(kind: u8, identity: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + identity.len());
    key.push(kind);
    key.extend_from_slice(identity);
    key
}

fn scan_budget(label: &str, visited: u64, total: u64, maximum: Option<u64>) -> eyre::Result<()> {
    if maximum.is_some_and(|maximum| visited >= maximum) {
        return Err(Incomplete(format!("{label} scan stopped at {visited}/{total}")).into());
    }
    Ok(())
}

/// Keep observations from successful reads even when a later relation fails.
/// A visited count is not a passed check or a claim about an unvisited suffix.
fn observe_inventory_bound(
    report: Option<&mut super::report::ValidationReport>,
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
            .push(super::report::InventoryBounds {
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

    fn scan_with_report(
        state: &'a CanonicalState<'b>,
        scratch_parent: &Path,
        protected: &ProtectedPaths,
        maximum_records: Option<u64>,
        mut report: Option<&mut super::report::ValidationReport>,
    ) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
        // The owner bounds its native aggregate before allocation and validates
        // live scheduler/FSM/job equivalence. No local directory seeds this list.
        let active_jobs = state.live_ocomp_jobs()?;
        let active_intents = u64::try_from(active_jobs.len())?;
        if let Some(report) = report.as_deref_mut() {
            report.active_ocomp = active_jobs
                .iter()
                .map(|(intent_id, job)| super::report::ActiveOcompObservation {
                    intent_id: hex::encode(intent_id),
                    job_id: job.finalized.as_ref().map(|job| hex::encode(job.job_id)),
                    request_height: job.intent_height,
                    worldwide_day: job.intent.wwd,
                    canonical_status: format!("{:?}", job.status),
                    pin_stage: "NotInspected".into(),
                    projection_before_request: None,
                    source_verified: false,
                    export_verified: false,
                })
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

fn read_pinned_bundle(root: &Path, hash: B256) -> eyre::Result<PinnedProtocolBundle> {
    use std::io::Read;
    let limits = poc_schema_limits();
    let source = outbe_snapshot::fs::SourceRoot::open(root)?;
    let catalog =
        std::path::PathBuf::from("protocol-bundles-v1").join(format!("{}.ocb1", hex::encode(hash)));
    let (path, mut entry) = match source.open_entry(&catalog) {
        Ok(entry) => (catalog, entry),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = std::path::PathBuf::from("protocol-bundle-v1.ocb1");
            let entry = source.open_entry(&path)?;
            (path, entry)
        }
        Err(error) => return Err(error.into()),
    };
    let cap = limits
        .codec
        .max_body_bytes
        .checked_add(outbe_ocomp_protocol::codec::OCB1_HEADER_LEN)
        .ok_or_else(|| eyre::eyre!("bundle codec limit overflow"))?;
    ensure!(
        !entry.identity.is_directory,
        "protocol bundle is not a file"
    );
    ensure!(
        entry.identity.size <= u64::try_from(cap)?,
        "protocol bundle exceeds codec limit"
    );
    let mut bytes = Vec::new();
    (&mut entry.file)
        .take(entry.identity.size + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == entry.identity.size,
        "protocol bundle changed size"
    );
    entry.verify_unchanged()?;
    source.reopen(&path, &entry.identity)?;
    source.verify_unchanged()?;
    Ok(PinnedProtocolBundle::decode(&bytes, hash, &limits)?)
}

fn classify_nod_input_error(error: eyre::Report, job: B256) -> eyre::Report {
    if error.downcast_ref::<Incomplete>().is_some() {
        return error;
    }
    if missing_native_input(error.as_ref()) {
        return error.wrap_err(Incomplete(format!(
            "missing required NOD input for job {job}"
        )));
    }
    error.wrap_err(format!("NOD inputs for job {job}"))
}

/// Transparent native wrappers can omit the wrapped enum from Error::source().
/// Follow their typed edges so missing records remain distinct from corrupt data.
fn missing_native_input(error: &(dyn std::error::Error + 'static)) -> bool {
    use outbe_ocomp::{
        admission_catalog::AdmissionCatalogError as Admission,
        export_binding::ExportBindingError as Binding,
        export_receipt::ExportReceiptError as Receipt,
        input_artifacts::InputArtifactError as Input,
        input_ref_catalog::InputRefCatalogError as Refs,
        lysis_plan_audit::ExactLysisPlanError as Plan,
        lysis_result_catalog::LysisResultCatalogError as ResultCatalog,
        nod_materialization::NodMaterializationBuildErrorV1 as Build,
    };
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return error.kind() == std::io::ErrorKind::NotFound;
    }
    if let Some(error) = error.downcast_ref::<Binding>() {
        return match error {
            Binding::MissingBinding => true,
            Binding::Cas(error) => missing_native_input(error),
            Binding::InputCatalog(error) => missing_native_input(error),
            Binding::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Receipt>() {
        return match error {
            Receipt::MissingReceipt | Receipt::MissingPreparation => true,
            Receipt::Cas(error) => missing_native_input(error),
            Receipt::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Admission>() {
        return match error {
            Admission::MissingHeader | Admission::MissingAdmission { .. } => true,
            Admission::Cas(error) => missing_native_input(error),
            Admission::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Refs>() {
        return match error {
            Refs::MissingHeader | Refs::MissingReference { .. } => true,
            Refs::Cas(error) => missing_native_input(error),
            Refs::Io { source, .. } => missing_native_input(source),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Input>() {
        return match error {
            Input::Cas(error) => missing_native_input(error),
            Input::InputRefCatalog(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Plan>() {
        return match error {
            Plan::Admission(error) => missing_native_input(error),
            Plan::InputRef(error) => missing_native_input(error),
            Plan::InputArtifact(error) => missing_native_input(error),
            Plan::Cas(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<ResultCatalog>() {
        return match error {
            ResultCatalog::Plan(error) => missing_native_input(error),
            ResultCatalog::Admission(error) => missing_native_input(error),
            ResultCatalog::Cas(error) => missing_native_input(error),
            _ => false,
        };
    }
    if let Some(error) = error.downcast_ref::<Build>() {
        return match error {
            Build::Plan(error) => missing_native_input(error),
            Build::ResultCatalog(error) => missing_native_input(error),
            _ => false,
        };
    }
    error.source().is_some_and(missing_native_input)
}

/// Counts from a complete native payout bitmap, including non-prefix payments.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BitmapAudit {
    pub words: u64,
    pub paid: u64,
    pub unpaid: u64,
}

pub(crate) fn verify_paid_bitmap(
    contributor_count: u32,
    paid_leaf_count: u32,
    maximum_words: Option<u64>,
    mut read_word: impl FnMut(u32) -> eyre::Result<U256>,
) -> eyre::Result<BitmapAudit> {
    ensure!(
        paid_leaf_count <= contributor_count,
        "paid leaf count exceeds certified count"
    );
    let words = u64::from(contributor_count).div_ceil(256);
    let mut paid = 0_u64;
    for index in 0..words {
        if maximum_words.is_some_and(|maximum| index >= maximum) {
            return Err(Incomplete(format!(
                "payout bitmap scan stopped at {index}/{words} words"
            ))
            .into());
        }
        let word = read_word(u32::try_from(index)?)?;
        let bits = (u64::from(contributor_count) - index * 256).min(256);
        if bits < 256 {
            ensure!(
                (word >> bits).is_zero(),
                "paid bitmap contains bits above certified contributor count"
            );
        }
        paid = paid
            .checked_add(word.count_ones() as u64)
            .ok_or_else(|| eyre::eyre!("paid bitmap count overflow"))?;
    }
    ensure!(
        paid == u64::from(paid_leaf_count),
        "paid bitmap popcount differs from paid leaf count"
    );
    Ok(BitmapAudit {
        words,
        paid,
        unpaid: u64::from(contributor_count) - paid,
    })
}

/// Native SeriesId::worldwide_day is intentionally permissive; validate its
/// stored spelling and date before using it to discover canonical obligations.
pub(crate) fn verify_series_day(id: SeriesId) -> eyre::Result<WorldwideDay> {
    let bytes = id.as_bytes();
    ensure!(
        bytes[..8].iter().all(u8::is_ascii_digit) && bytes[8] == b'-' && bytes[12] == b'-',
        "noncanonical series identity"
    );
    let day = id.worldwide_day();
    ensure!(
        day.value() != 0 && day.is_valid(),
        "invalid series worldwide day"
    );
    let issuance = [bytes[9], bytes[10], bytes[11]];
    ensure!(
        SeriesId::pack(day, issuance, bytes[13])? == id,
        "noncanonical series codes"
    );
    Ok(day)
}
#[derive(Debug, Default)]
pub(crate) struct FrameAvailability {
    pub blocks: u64,
    pub transactions: u64,
}

#[derive(Debug)]
pub(crate) struct ClosureAudit {
    pub checkpoint: outbe_ocomp::discovery_spool::ClosureCheckpointInspectionV1,
    pub replay: FrameAvailability,
}

/// Observe the native closure positions without opening its writable store.
/// Historical previous may be sparse; only the saved identities are required.
pub(crate) fn verify_closure(
    view: &super::super::native::RethReadOnlyView,
    root: &Path,
    projection: Option<outbe_primitives::projection::ProjectionCheckpoint>,
    maximum_transactions: Option<u64>,
) -> eyre::Result<ClosureAudit> {
    use alloy_consensus::Sealable;
    use outbe_ocomp::discovery_spool::inspect_closure_checkpoint;
    use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader};
    use reth_ethereum::provider::db::tables;
    use reth_provider::{BlockHashReader, HeaderProvider};

    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: view.chain.genesis_hash(),
    };
    let checkpoint = inspect_closure_checkpoint(root, baseline).map_err(|error| {
        if missing_native_input(&error) {
            eyre::Report::new(error).wrap_err(Incomplete(format!(
                "missing native closure checkpoint at {}",
                root.display()
            )))
        } else {
            error.into()
        }
    })?;
    let tx = view.read_transaction()?;
    for (name, point) in [
        ("baseline", checkpoint.baseline),
        ("previous", checkpoint.previous),
        ("current", checkpoint.current),
    ] {
        let number = point.block_number;
        let header = match tx.get::<tables::Headers<OutbeHeader>>(number)? {
            Some(header) => Some(header),
            None => view.static_files.header_by_number(number)?,
        }
        .ok_or_else(|| Incomplete(format!("missing closure {name} header at {number}")))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(number)?,
        }
        .ok_or_else(|| Incomplete(format!("missing closure {name} canonical hash at {number}")))?;
        ensure!(
            header.inner.number == number && header.hash_slow() == hash && point.block_hash == hash,
            "closure {name} differs from canonical header at {number}"
        );
    }
    match projection {
        Some(projected) => ensure!(
            projected.block_number >= checkpoint.current.block_number
                && (projected.block_number != checkpoint.current.block_number
                    || projected.block_hash == checkpoint.current.block_hash),
            "closure checkpoint is ahead of or conflicts with durable projection"
        ),
        None => ensure!(
            checkpoint.current.block_number == 0,
            "nonzero closure checkpoint exists without durable projection"
        ),
    }
    let replay = if checkpoint.current.block_number < view.progress.finalized.number {
        // The strict comparison proves addition cannot overflow, even at MAX.
        verify_retained_frames(
            view,
            checkpoint.current.block_number + 1,
            view.progress.finalized.clone(),
            maximum_transactions,
        )?
    } else {
        FrameAvailability::default()
    };
    Ok(ClosureAudit { checkpoint, replay })
}

/// Check the retained inputs used by ordinary OCOMP replay, one transaction and
/// receipt at a time. This is availability/identity validation, not EVM replay.
pub(crate) fn verify_retained_frames(
    view: &super::super::native::RethReadOnlyView,
    start: u64,
    end: outbe_snapshot::manifest::BlockIdentity,
    maximum_transactions: Option<u64>,
) -> eyre::Result<FrameAvailability> {
    visit_retained_frames(view, start, end, maximum_transactions, &mut |_| Ok(()))
}

fn visit_retained_frames(
    view: &super::super::native::RethReadOnlyView,
    start: u64,
    end: outbe_snapshot::manifest::BlockIdentity,
    maximum_transactions: Option<u64>,
    visitor: &mut impl FnMut(&outbe_primitives::OutbeReceipt) -> eyre::Result<()>,
) -> eyre::Result<FrameAvailability> {
    use alloy_consensus::Sealable;
    use outbe_primitives::{OutbeHeader, OutbeReceipt};
    use reth_ethereum::provider::db::tables;
    use reth_provider::{
        BlockHashReader, HeaderProvider, ReceiptProvider, StaticFileSegment, TransactionsProvider,
    };

    let mut result = FrameAvailability::default();
    if start > end.number {
        return Ok(result);
    }
    let expected_end = B256::try_from(hex::decode(&end.hash)?.as_slice())?;
    let tx = view.read_transaction()?;
    let mut previous = None;
    for height in start..=end.number {
        let header = match tx.get::<tables::Headers<OutbeHeader>>(height)? {
            Some(header) => Some(header),
            None => view.static_files.header_by_number(height)?,
        }
        .ok_or_else(|| Incomplete(format!("missing replay header at {height}")))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(height)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(height)?,
        }
        .ok_or_else(|| Incomplete(format!("missing replay canonical hash at {height}")))?;
        ensure!(
            header.inner.number == height && header.hash_slow() == hash,
            "replay header differs from canonical identity at {height}"
        );
        if let Some(previous) = previous {
            ensure!(
                header.inner.parent_hash == previous,
                "replay header parent differs at {height}"
            );
        }
        previous = Some(hash);
        if height == end.number {
            ensure!(
                hash == expected_end,
                "replay target hash differs at {height}"
            );
        }
        let indices = tx
            .get::<tables::BlockBodyIndices>(height)?
            .ok_or_else(|| Incomplete(format!("missing replay body indices at {height}")))?;
        let tx_end = indices
            .first_tx_num
            .checked_add(indices.tx_count)
            .ok_or_else(|| eyre::eyre!("replay transaction range overflows at {height}"))?;
        for number in indices.first_tx_num..tx_end {
            if maximum_transactions.is_some_and(|maximum| result.transactions >= maximum) {
                return Err(Incomplete(format!(
                    "replay frame scan stopped at block {height}/{}, transaction {number}/{tx_end}; {} blocks, {} transactions visited",
                    end.number, result.blocks, result.transactions
                )).into());
            }
            // Match the pinned Reth provider: transactions are static-only;
            // receipts choose one native backend by its high-water mark. A hole
            // below that mark must not be disguised by a different backend.
            view.static_files
                .transaction_by_id(number)?
                .ok_or_else(|| {
                    Incomplete(format!(
                        "missing replay transaction {number} at block {height}"
                    ))
                })?;
            let receipt = view
                .static_files
                .get_with_static_file_or_database(
                    StaticFileSegment::Receipts,
                    number,
                    |files| files.receipt(number),
                    || Ok(tx.get::<tables::Receipts<OutbeReceipt>>(number)?),
                )?
                .ok_or_else(|| {
                    Incomplete(format!("missing replay receipt {number} at block {height}"))
                })?;
            visitor(&receipt)?;
            result.transactions = result
                .transactions
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("replay transaction count overflow"))?;
        }
        result.blocks = result
            .blocks
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("replay block count overflow"))?;
    }
    Ok(result)
}

/// Use one retained request frame as a locator, then authenticate its intent
/// against verified current E. No historical state or retired spool is needed.
pub(crate) fn locate_request_job(
    state: &CanonicalState<'_>,
    view: &super::super::native::RethReadOnlyView,
    request_height: u64,
    expected_job: B256,
    expected_day: WorldwideDay,
    maximum_transactions: Option<u64>,
) -> eyre::Result<OcompJobRecordV1> {
    use alloy_consensus::{Sealable, TxReceipt};
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;
    use outbe_primitives::addresses::METADOSIS_ADDRESS;

    let header = view
        .header(request_height)?
        .ok_or_else(|| Incomplete(format!("missing OCOMP request header B={request_height}")))?;
    let hash = header.hash_slow();
    let mut request = None;
    visit_retained_frames(
        view,
        request_height,
        outbe_snapshot::manifest::BlockIdentity {
            number: request_height,
            hash: hex::encode(hash),
        },
        maximum_transactions,
        &mut |receipt| {
            if !receipt.status() {
                return Ok(());
            }
            for log in receipt.logs() {
                if log.address != METADOSIS_ADDRESS
                    || log.data.topics().first()
                        != Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
                {
                    continue;
                }
                let event = IMetadosis::OffchainJobRequested::decode_log(log)?;
                ensure!(
                    request.replace(event.data).is_none(),
                    "request frame B={request_height} contains multiple OCOMP requests"
                );
            }
            Ok(())
        },
    )?;
    let event = request.ok_or_else(|| {
        eyre::eyre!("complete request frame B={request_height} contains no OCOMP request")
    })?;
    ensure!(
        event.wwd == expected_day.value(),
        "request event day differs from artifact"
    );
    let job = state.metadosis_job(event.intentId, expected_day, Some(expected_job))?;
    let limits = poc_schema_limits();
    ensure!(
        job.intent_height == request_height
            && job.intent.logical_evaluation_height == request_height
            && job.intent.logical_evaluation_time == header.inner.timestamp
            && job.intent.pending_nonce == event.pendingNonce
            && job.intent.attempt == event.attempt
            && job
                .intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)?
                == event.activationPreconditionsHash,
        "request event or frozen block context differs from canonical job"
    );
    ensure!(
        job.intent.job_id(hash, header.inner.state_root, &limits)? == expected_job,
        "artifact JobId differs from canonical request identity"
    );
    Ok(job)
}

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

impl From<&CanonicalActiveAudit> for super::report::ActiveOcompObservation {
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
    mut report: Option<&mut super::report::ValidationReport>,
) -> eyre::Result<CanonicalOcompAudit> {
    use alloy_consensus::Sealable;
    use outbe_node::ocomp::retention::{inspect_retention_journal, PinStateV1, RetentionError};
    use outbe_offchain_data::{read_projection_state, ProjectionConfig};
    use outbe_offchain_storage::{RocksDbReader, StorageReaderHandle};
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
    match std::fs::metadata(location.root.join("CURRENT")) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                Incomplete("missing OCOMP projection database or CURRENT file".into()).into(),
            );
        }
        Err(error) => return Err(error.into()),
    }
    // Distinct from Task05's secondary. The immutable reader drops before this
    // local TempDir; no shared source handle or mutable catch-up API is exposed.
    let secondary = tempfile::Builder::new()
        .prefix("ocomp-projection-audit-")
        .tempdir_in(scratch_parent)?;
    let reader: StorageReaderHandle =
        Arc::new(RocksDbReader::open(&location.root, secondary.path())?);
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
            report.retained_ranges.push(super::report::RetainedRange {
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
        // Missing pin is observed; P>=B alone cannot imply corruption across all
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

#[derive(Debug, Default)]
pub(crate) struct CasAudit {
    pub objects: u64,
    pub bytes: u64,
}

/// Check transport integrity of every published native CAS object, including
/// unreferenced historical objects. Typed artifact/job relationships are checked
/// separately through their native references; this does not infer an OCB1 kind.
pub(crate) fn verify_present_cas(
    ocomp_root: &Path,
    limits: CasLimits,
    maximum_objects: Option<u64>,
) -> eyre::Result<CasAudit> {
    let root = ocomp_root.join("cas-v1");
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CasAudit::default());
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(metadata.is_dir(), "CAS root is not a native directory"),
    }
    let scan = || -> eyre::Result<CasAudit> {
        let cas = FilesystemCasReader::open(&root, limits)?;
        let mut result = CasAudit::default();
        for shard in std::fs::read_dir(root.join("objects"))? {
            let shard = shard?;
            let prefix = shard.file_name();
            let prefix = prefix
                .to_str()
                .ok_or_else(|| eyre::eyre!("invalid CAS shard"))?;
            ensure!(
                shard.file_type()?.is_dir() && prefix.len() == 2,
                "invalid native CAS shard"
            );
            for entry in std::fs::read_dir(shard.path())? {
                let entry = entry?;
                if maximum_objects.is_some_and(|maximum| result.objects >= maximum) {
                    return Err(Incomplete(format!(
                        "CAS scan stopped after {} objects",
                        result.objects
                    ))
                    .into());
                }
                ensure!(
                    entry.file_type()?.is_file(),
                    "CAS object is not a regular file"
                );
                let suffix = entry.file_name();
                let suffix = suffix
                    .to_str()
                    .ok_or_else(|| eyre::eyre!("invalid CAS object locator"))?;
                let encoded = format!("{prefix}{suffix}");
                let mut digest = [0_u8; 32];
                hex::decode_to_slice(&encoded, &mut digest)?;
                ensure!(
                    hex::encode(digest) == encoded,
                    "noncanonical CAS digest locator"
                );
                let length = entry.metadata()?.len();
                let total = result
                    .bytes
                    .checked_add(length)
                    .ok_or_else(|| eyre::eyre!("CAS total byte count overflow"))?;
                if total > limits.max_total_bytes {
                    return Err(Incomplete(format!(
                        "CAS scan exceeds {} byte budget after {} objects and {} bytes",
                        limits.max_total_bytes, result.objects, result.bytes
                    ))
                    .into());
                }
                let reference = outbe_ocomp_protocol::CasObjectRefV1 {
                    transport_digest: B256::from(digest),
                    encoded_bytes: length,
                    expected_ocb1_kind: None,
                };
                // Native reader checks digest, exact size and descriptor identity.
                // It buffers one bounded object, not the entire CAS population.
                cas.read_verified(&reference)?;
                result.objects = result
                    .objects
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("CAS object count overflow"))?;
                result.bytes = total;
            }
        }
        Ok(result)
    };
    scan().map_err(|error| {
        if error.downcast_ref::<Incomplete>().is_some() {
            error
        } else if missing_native_input(error.as_ref()) {
            error.wrap_err(Incomplete(
                "missing native CAS data during selected scan".into(),
            ))
        } else {
            error
        }
    })
}

/// Verify a surviving native receipt against authenticated canonical job authority.
/// Historical binding/catalog/chunk absence does not invalidate this observation.
/// Complete-export obligations still use `verify_export_inputs`.
pub(crate) fn verify_present_receipt(
    ocomp_root: &Path,
    job: &OcompJobRecordV1,
    expected_export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
    cas_limits: CasLimits,
) -> eyre::Result<outbe_ocomp::export_receipt::VerifiedExportReceipt> {
    use outbe_ocomp::export_receipt::{ExportReceiptError, ExportReceiptReader};
    use outbe_ocomp_protocol::input::CheckpointIdentityV1;

    let check = || -> eyre::Result<_> {
        let limits = poc_schema_limits();
        let finalized = job.finalized.as_ref().ok_or_else(|| {
            eyre::eyre!("present receipt lacks canonical finalized job authority")
        })?;
        let spec = canonical_job_spec(job)?;
        let cas = FilesystemCasReader::open(ocomp_root.join("cas-v1"), cas_limits)?;
        let receipt = ExportReceiptReader::try_open(
            ocomp_root.join("exporter-v1/receipts"),
            finalized.job_id,
            limits,
        )?
        .ok_or(ExportReceiptError::MissingReceipt)?
        .load_exact(&cas)?;
        let manifest = receipt.manifest();
        // These are the public authority fields compared by the native complete
        // export binding. The native reader owns prepared/receipt/manifest codecs.
        ensure!(
            receipt.job_id() == spec.summary.job_id && manifest.job_id == spec.summary.job_id,
            "receipt manifest differs from canonical JobId"
        );
        ensure!(
            manifest.protocol_bundle_hash == spec.summary.protocol_bundle_hash
                && manifest.protocol_bundle_hash == job.intent.protocol_bundle_hash,
            "receipt manifest differs from canonical protocol bundle"
        );
        ensure!(
            manifest.attempt == job.intent.attempt && manifest.wwd == job.intent.wwd,
            "receipt manifest differs from canonical attempt or WWD"
        );
        ensure!(
            manifest.sealed_tribute_collection_key == job.intent.sealed_tribute_collection_key
                && manifest.sealed_tribute_collection_root
                    == job.intent.sealed_tribute_collection_root
                && manifest.tribute_count == job.intent.authenticated_day_count
                && manifest.tribute_nominal_total == job.intent.authenticated_day_nominal,
            "receipt manifest differs from canonical frozen Tribute authority"
        );
        let checkpoint = CheckpointIdentityV1 {
            finalized_block_number: job.intent_height,
            finalized_block_hash: finalized.finalized_request_block_hash,
            finalized_state_root: finalized.finalized_request_state_root,
            finalized_ce_root: job.intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )?,
        };
        ensure!(
            receipt.checkpoint() == &checkpoint && manifest.checkpoint == checkpoint,
            "receipt checkpoint differs from canonical request or CE schema"
        );
        let bundle = read_pinned_bundle(ocomp_root, job.intent.protocol_bundle_hash)?;
        manifest.validate_against_bundle(bundle.bundle(), &limits)?;
        if let Some(expected) = expected_export {
            ensure!(
                expected.source_generation == receipt.source_pin_generation()
                    && expected.lease_generation == receipt.lease_generation()
                    && expected.manifest_hash == receipt.manifest_hash(),
                "saved pin export differs from receipt authority"
            );
        }
        Ok(receipt)
    };
    check().map_err(|error| {
        if error.downcast_ref::<Incomplete>().is_some() {
            error
        } else if missing_native_input(error.as_ref()) {
            error.wrap_err(Incomplete(
                "missing evidence for a present OCOMP receipt".into(),
            ))
        } else {
            error.wrap_err("present OCOMP receipt")
        }
    })
}

const PRESENT_RECEIPT: u8 = 1;
const PRESENT_BINDING: u8 = 2;
const PRESENT_INPUTS: u8 = 4;
const PRESENT_ADMISSIONS: u8 = 8;
const PRESENT_REFERENCES: u8 = 16;
const PRESENT_ACK: u8 = 32;

/// Scratch-only population union. Native file paths never seed canonical inventory.
struct PresentJobUnion {
    db: DatabaseEnv,
    _directory: tempfile::TempDir,
}

impl PresentJobUnion {
    fn create(parent: &Path, protected: &ProtectedPaths) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[parent.to_path_buf()])?;
        let directory = tempfile::Builder::new()
            .prefix("ocomp-present-")
            .tempdir_in(parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        Ok(Self {
            db,
            _directory: directory,
        })
    }

    fn key(prefix: u8, job: B256) -> Vec<u8> {
        let mut key = vec![prefix];
        key.extend_from_slice(job.as_slice());
        key
    }

    fn add(&self, job: B256, flag: u8) -> eyre::Result<()> {
        let key = Self::key(b'u', job);
        let tx = self.db.tx_mut()?;
        let previous = tx.get::<InventoryRows>(key.clone())?.map_or(0, |v| v[0]);
        tx.put::<InventoryRows>(key, vec![previous | flag])?;
        tx.commit()?;
        Ok(())
    }

    // d/l are provisional authorities emitted by discovery/local-result walkers.
    // Publish to a only when their enclosing native walk has succeeded.
    fn save_job(&self, prefix: u8, job: &OcompJobRecordV1) -> eyre::Result<()> {
        let Some(finalized) = &job.finalized else {
            return Ok(());
        };
        let key = Self::key(prefix, finalized.job_id);
        let encoded = job.encode_canonical(&poc_schema_limits())?;
        let tx = self.db.tx_mut()?;
        if let Some(previous) = tx.get::<InventoryRows>(key.clone())? {
            ensure!(
                previous == encoded,
                "conflicting canonical authority for present job"
            );
        }
        tx.put::<InventoryRows>(key, encoded)?;
        tx.commit()?;
        Ok(())
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visitor: &mut impl FnMut(Vec<u8>, Vec<u8>) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let mut next = prefix.to_vec();
        loop {
            // Release the read transaction before callback-side scratch writes.
            let row = {
                let tx = self.db.tx()?;
                let mut cursor = tx.cursor_read::<InventoryRows>()?;
                cursor.seek(next.clone())?
            };
            let Some((key, value)) = row else { break };
            if !key.starts_with(prefix) {
                break;
            }
            next = key.clone();
            next.push(0);
            visitor(key, value)?;
        }
        Ok(())
    }

    fn publish_jobs(&self, prefix: u8) -> eyre::Result<()> {
        self.visit_prefix(&[prefix], &mut |_, bytes| {
            let job = OcompJobRecordV1::decode_canonical(&bytes, &poc_schema_limits())?;
            self.save_job(b'a', &job)
        })
    }

    fn job(&self, id: B256) -> eyre::Result<Option<OcompJobRecordV1>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'a', id))?
            .map(|bytes| {
                OcompJobRecordV1::decode_canonical(&bytes, &poc_schema_limits()).map_err(Into::into)
            })
            .transpose()
    }

    fn save_evidence(&self, prefix: u8, job: B256, bytes: Vec<u8>) -> eyre::Result<()> {
        let key = Self::key(prefix, job);
        let tx = self.db.tx_mut()?;
        if let Some(previous) = tx.get::<InventoryRows>(key.clone())? {
            ensure!(
                previous == bytes,
                "conflicting surviving evidence for same job"
            );
        }
        tx.put::<InventoryRows>(key, bytes)?;
        tx.commit()?;
        Ok(())
    }

    fn publish_evidence(&self, from: u8, to: u8, flag: u8) -> eyre::Result<()> {
        self.visit_prefix(&[from], &mut |key, bytes| {
            ensure!(key.len() == 33, "invalid scratch evidence key");
            let job = B256::from_slice(&key[1..]);
            self.save_evidence(to, job, bytes)?;
            if flag != 0 {
                self.add(job, flag)?;
            }
            Ok(())
        })
    }

    fn save_ack(
        &self,
        ack: &outbe_ocomp::discovery_spool::StoredDiscoveryAckV1,
    ) -> eyre::Result<()> {
        let mut bytes = ack.reference.encode_fixed();
        bytes.extend_from_slice(&ack.lease_generation.to_be_bytes());
        bytes.extend_from_slice(ack.manifest_hash.as_slice());
        bytes.extend_from_slice(&ack.committed.encode_body(&poc_schema_limits())?);
        self.save_evidence(b'k', ack.committed.job_id, bytes)
    }

    fn ack(
        &self,
        job: B256,
    ) -> eyre::Result<Option<outbe_ocomp::discovery_spool::StoredDiscoveryAckV1>> {
        use outbe_ocomp::{
            discovery_control::DiscoveryAckRefV1, discovery_spool::StoredDiscoveryAckV1,
        };
        let fixed = DiscoveryAckRefV1::FIXED_BYTES;
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'c', job))?
            .map(|bytes| {
                ensure!(bytes.len() > fixed + 40, "invalid scratch discovery ACK");
                Ok(StoredDiscoveryAckV1 {
                    reference: DiscoveryAckRefV1::decode_fixed(&bytes[..fixed])?,
                    lease_generation: u64::from_be_bytes(bytes[fixed..fixed + 8].try_into()?),
                    manifest_hash: B256::from_slice(&bytes[fixed + 8..fixed + 40]),
                    committed: outbe_ocomp_protocol::SnapshotExportCommittedV1::decode_body(
                        &bytes[fixed + 40..],
                        &poc_schema_limits(),
                    )?,
                })
            })
            .transpose()
    }

    fn save_result_binding(
        &self,
        job: B256,
        result: &outbe_ocomp_protocol::result::LysisResultV1,
    ) -> eyre::Result<()> {
        let mut bytes = result.input_manifest_hash.as_slice().to_vec();
        bytes.extend_from_slice(result.plan_hash.as_slice());
        self.save_evidence(b'm', job, bytes)
    }

    fn result_binding(&self, job: B256) -> eyre::Result<Option<(B256, B256)>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'v', job))?
            .map(|bytes| {
                ensure!(bytes.len() == 64, "invalid scratch local-result binding");
                Ok((
                    B256::from_slice(&bytes[..32]),
                    B256::from_slice(&bytes[32..]),
                ))
            })
            .transpose()
    }

    fn save_export(
        &self,
        job: B256,
        export: outbe_node::ocomp::retention::ExportAuthorityV1,
    ) -> eyre::Result<()> {
        let mut bytes = export.source_generation.to_be_bytes().to_vec();
        bytes.extend_from_slice(&export.lease_generation.to_be_bytes());
        bytes.extend_from_slice(export.manifest_hash.as_slice());
        let tx = self.db.tx_mut()?;
        tx.put::<InventoryRows>(Self::key(b'e', job), bytes)?;
        tx.commit()?;
        Ok(())
    }

    fn export(
        &self,
        job: B256,
    ) -> eyre::Result<Option<outbe_node::ocomp::retention::ExportAuthorityV1>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'e', job))?
            .map(|bytes| {
                ensure!(bytes.len() == 48, "invalid scratch export authority");
                Ok(outbe_node::ocomp::retention::ExportAuthorityV1 {
                    source_generation: u64::from_be_bytes(bytes[..8].try_into()?),
                    lease_generation: u64::from_be_bytes(bytes[8..16].try_into()?),
                    manifest_hash: B256::from_slice(&bytes[16..]),
                })
            })
            .transpose()
    }

    fn save_refs(
        &self,
        job: B256,
        ordinal: u32,
        refs: &[outbe_ocomp_protocol::CasObjectRefV1],
    ) -> eyre::Result<()> {
        self.add(job, PRESENT_REFERENCES)?;
        let tx = self.db.tx_mut()?;
        for (index, reference) in refs.iter().enumerate() {
            let mut key = Self::key(b'r', job);
            key.extend_from_slice(&ordinal.to_be_bytes());
            key.extend_from_slice(&u32::try_from(index)?.to_be_bytes());
            let mut bytes = reference.transport_digest.as_slice().to_vec();
            bytes.extend_from_slice(&reference.encoded_bytes.to_be_bytes());
            match reference.expected_ocb1_kind {
                None => bytes.push(0),
                Some(kind) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&kind.to_be_bytes());
                }
            }
            tx.put::<InventoryRows>(key, bytes)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn visit_refs(
        &self,
        job: B256,
        visitor: &mut impl FnMut(outbe_ocomp_protocol::CasObjectRefV1) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        self.visit_prefix(&Self::key(b'r', job), &mut |_, bytes| {
            ensure!(
                bytes.len() == 41 || bytes.len() == 43,
                "invalid scratch reference"
            );
            let expected_ocb1_kind = match bytes[40] {
                0 if bytes.len() == 41 => None,
                1 if bytes.len() == 43 => Some(u16::from_be_bytes(bytes[41..].try_into()?)),
                _ => eyre::bail!("invalid scratch reference kind"),
            };
            visitor(outbe_ocomp_protocol::CasObjectRefV1 {
                transport_digest: B256::from_slice(&bytes[..32]),
                encoded_bytes: u64::from_be_bytes(bytes[32..40].try_into()?),
                expected_ocb1_kind,
            })
        })
    }
}

#[derive(Default)]
struct PresentJoinErrors {
    first: Option<eyre::Report>,
    failed: bool,
}
impl PresentJoinErrors {
    fn observe<T>(&mut self, result: eyre::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                let error = present_join_error(error);
                let failed = error.downcast_ref::<Incomplete>().is_none();
                if self.first.is_none() || (failed && !self.failed) {
                    self.first = Some(error);
                }
                self.failed |= failed;
                None
            }
        }
    }
    fn finish(self) -> eyre::Result<()> {
        self.first.map_or(Ok(()), Err)
    }
}
fn present_join_error(error: eyre::Report) -> eyre::Report {
    if error.downcast_ref::<Incomplete>().is_some() {
        error
    } else if missing_native_input(error.as_ref()) {
        error.wrap_err(Incomplete(
            "missing evidence for present OCOMP relation".into(),
        ))
    } else {
        error
    }
}
fn present_count(report: &mut super::report::ValidationReport, name: &str, count: u64) {
    report
        .inventory_bounds
        .push(super::report::InventoryBounds {
            name: name.into(),
            start: 0,
            end_exclusive: count,
            visited: count,
        });
}
fn existing_directory(path: &Path) -> eyre::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "not a native directory: {}",
                path.display()
            );
            Ok(true)
        }
    }
}
fn existing_file(path: &Path) -> eyre::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(metadata.is_file(), "not a native file: {}", path.display());
            Ok(true)
        }
    }
}
fn directory_has_entries(path: &Path) -> eyre::Result<bool> {
    if !existing_directory(path)? {
        return Ok(false);
    }
    Ok(std::fs::read_dir(path)?.next().transpose()?.is_some())
}

fn scan_present_jobs(root: &Path, flag: u8, work: &PresentJobUnion) -> eyre::Result<u64> {
    if !existing_directory(root)? {
        return Ok(0);
    }
    let mut count = 0_u64;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_dir(),
            "present job locator is not a directory"
        );
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| eyre::eyre!("invalid present job locator"))?;
        // The exporter retains inventory/opening work beside published input catalogs.
        if flag == PRESENT_INPUTS && name == ".work" {
            continue;
        }
        let mut bytes = [0; 32];
        hex::decode_to_slice(name, &mut bytes)?;
        let job = B256::from(bytes);
        ensure!(
            !job.is_zero() && name == hex::encode(job),
            "noncanonical present job locator"
        );
        let path = if flag == PRESENT_ADMISSIONS {
            entry.path().join("admissions")
        } else {
            entry.path()
        };
        // Empty directories do not fabricate a complete local stage.
        if directory_has_entries(&path)? {
            work.add(job, flag)?;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("present job count overflow"))?;
    }
    Ok(count)
}

/// Final selected OCOMP composition. Caller records the Ocomp status.
pub(crate) fn verify_ocomp_relations(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    layout: &crate::snapshot::config::RequestedLayout,
    scratch_parent: &Path,
    report: &mut super::report::ValidationReport,
) -> eyre::Result<()> {
    let limits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: u64::MAX,
    };
    let mut errors = PresentJoinErrors::default();
    // Exactly one canonical inventory/obligation scan, before local populations.
    let canonical = errors.observe(verify_canonical_obligations(
        state,
        view,
        layout,
        scratch_parent,
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
            ("verified_pending_payout_days", canonical.payout_days),
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
    }
    if let Some(audit) = errors.observe(verify_present_discovery(
        state,
        view,
        &layout.ocomp_root,
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
        &layout.ocomp_root,
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
    if let Some(audit) = errors.observe(verify_present_cas(&layout.ocomp_root, limits, None)) {
        present_count(report, "present_cas_objects", audit.objects);
        present_count(report, "present_cas_bytes", audit.bytes);
    }
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
        if let Some(count) = errors.observe(scan_present_jobs(
            &layout.ocomp_root.join(prefix),
            flag,
            &work,
        )) {
            present_count(report, name, count);
        }
    }
    if let Some(count) = errors.observe(collect_present_references(&layout.ocomp_root, &work)) {
        present_count(report, "present_reference_records", count);
    }
    let mut counts = PresentArtifactCounts::default();
    let mut jobs = 0_u64;
    let context = PresentJobContext {
        state,
        view,
        layout,
        scratch: scratch_parent,
        protected: &protected,
        work: &work,
        cas_limits: limits,
    };
    // Each job callback runs after the union read transaction is released.
    work.visit_prefix(b"u", &mut |key, value| {
        ensure!(
            key.len() == 33 && value.len() == 1,
            "invalid scratch job union"
        );
        let job = B256::from_slice(&key[1..]);
        if let Some(canonical) = &canonical {
            let active = canonical.active.iter().any(|active| {
                active
                    .job
                    .finalized
                    .as_ref()
                    .is_some_and(|finalized| finalized.job_id == job)
            });
            errors.observe(verify_present_job(
                &context,
                job,
                value[0],
                active,
                &mut counts,
            ));
        }
        jobs = jobs
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("present job union overflow"))?;
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
    if let Some((live, retained)) =
        errors.observe(verify_present_projection_structure(layout, scratch_parent))
    {
        present_count(report, "ocomp_live_body_records", live);
        present_count(report, "ocomp_retained_body_records", retained);
    }
    errors.finish()
}

fn collect_present_references(root: &Path, work: &PresentJobUnion) -> eyre::Result<u64> {
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
                .ok_or_else(|| eyre::eyre!("reference record count overflow"))?;
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
struct PresentArtifactCounts {
    receipts: u64,
    bindings: u64,
    inputs: u64,
    admissions: u64,
    results: u64,
    references: u64,
}
fn increment_present(count: &mut u64, add: u64) -> eyre::Result<()> {
    *count = count
        .checked_add(add)
        .ok_or_else(|| eyre::eyre!("present artifact count overflow"))?;
    Ok(())
}

fn validate_present_manifest(
    manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    job: &OcompJobRecordV1,
    bundle: &PinnedProtocolBundle,
) -> eyre::Result<()> {
    let finalized = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("present manifest has no canonical finalized job"))?;
    ensure!(
        manifest.job_id == finalized.job_id
            && manifest.protocol_bundle_hash == job.intent.protocol_bundle_hash
            && manifest.attempt == job.intent.attempt
            && manifest.wwd == job.intent.wwd
            && manifest.sealed_tribute_collection_key == job.intent.sealed_tribute_collection_key
            && manifest.sealed_tribute_collection_root == job.intent.sealed_tribute_collection_root
            && manifest.tribute_count == job.intent.authenticated_day_count
            && manifest.tribute_nominal_total == job.intent.authenticated_day_nominal,
        "present manifest differs from canonical frozen job authority"
    );
    let checkpoint = &manifest.checkpoint;
    ensure!(
        checkpoint.finalized_block_number == job.intent_height
            && checkpoint.finalized_block_hash == finalized.finalized_request_block_hash
            && checkpoint.finalized_state_root == finalized.finalized_request_state_root
            && checkpoint.finalized_ce_root == job.intent.ce_sealed_root
            && checkpoint.ce_schema_version
                == u16::try_from(outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION)?,
        "present manifest differs from canonical checkpoint"
    );
    manifest.validate_against_bundle(bundle.bundle(), &poc_schema_limits())?;
    Ok(())
}

struct PresentJobContext<'a, 'state> {
    state: &'a CanonicalState<'state>,
    view: &'a crate::snapshot::native::RethReadOnlyView,
    layout: &'a crate::snapshot::config::RequestedLayout,
    scratch: &'a Path,
    protected: &'a ProtectedPaths,
    work: &'a PresentJobUnion,
    cas_limits: CasLimits,
}

fn compare_surviving_ack_export(
    ack: &outbe_ocomp::discovery_spool::StoredDiscoveryAckV1,
    export: outbe_node::ocomp::retention::ExportAuthorityV1,
) -> eyre::Result<()> {
    ensure!(
        ack.reference.generation == export.source_generation
            && ack.lease_generation == export.lease_generation
            && ack.manifest_hash == export.manifest_hash,
        "surviving discovery ACK differs from export authority"
    );
    Ok(())
}

fn compare_surviving_result_binding(
    result: (B256, B256),
    manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    plan_hash: Option<B256>,
) -> eyre::Result<()> {
    ensure!(
        result.0 == manifest.manifest_hash(&poc_schema_limits())?,
        "surviving local result differs from input manifest"
    );
    if let Some(plan_hash) = plan_hash {
        ensure!(
            result.1 == plan_hash,
            "surviving local result differs from plan"
        );
    }
    Ok(())
}

fn verify_present_job(
    context: &PresentJobContext<'_, '_>,
    id: B256,
    flags: u8,
    active: bool,
    counts: &mut PresentArtifactCounts,
) -> eyre::Result<()> {
    let PresentJobContext {
        state,
        view,
        layout,
        scratch,
        protected,
        work,
        cas_limits,
    } = *context;
    use outbe_ocomp::{
        export_binding::ExportedManifestBindingReader, export_receipt::ExportReceiptReader,
    };
    let root = &layout.ocomp_root;
    let name = hex::encode(id);
    let receipt_path = root.join("exporter-v1/receipts").join(&name);
    let binding_path = root.join("supervisor-v1/export-bindings").join(&name);
    let input_path = root.join("exporter-v1/input-refs").join(&name);
    let admission_path = root
        .join("supervisor-v1/jobs")
        .join(&name)
        .join("admissions");
    let schema = poc_schema_limits();
    let mut errors = PresentJoinErrors::default();
    let mut job = work.job(id)?;
    let ack = work.ack(id)?;
    if let (Some(ack), Some(export)) = (&ack, work.export(id)?) {
        errors.observe(compare_surviving_ack_export(ack, export));
    }
    let result_binding = work.result_binding(id)?;
    let mut manifest = None;
    let receipt_reader = if flags & PRESENT_RECEIPT != 0 {
        errors.observe(
            ExportReceiptReader::open(root.join("exporter-v1/receipts"), id, schema)
                .map_err(Into::into),
        )
    } else {
        None
    };
    let binding_reader = if flags & PRESENT_BINDING != 0 {
        errors.observe(
            ExportedManifestBindingReader::open_existing(&binding_path, schema).map_err(Into::into),
        )
    } else {
        None
    };
    let receipt_present =
        flags & PRESENT_RECEIPT != 0 && existing_file(&receipt_path.join("receipt.ref"))?;
    let binding_present =
        flags & PRESENT_BINDING != 0 && existing_file(&binding_path.join("binding.ref"))?;
    let input_present =
        flags & PRESENT_INPUTS != 0 && existing_file(&input_path.join("catalog.header"))?;
    if flags & PRESENT_RECEIPT != 0
        && !receipt_present
        && existing_file(&receipt_path.join("prepared.ref"))?
    {
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: prepared receipt observed; complete receipt comparison unavailable"
        ))
        .into()));
    }
    if flags & PRESENT_INPUTS != 0 && !input_present {
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: input catalog has no sealed header; exact comparison unavailable"
        ))
        .into()));
    }
    if active && ack.is_some() && !receipt_present {
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: active acknowledged export lacks its required complete receipt"
        ))
        .into()));
    }
    let needs_cas = receipt_present
        || binding_present
        || input_present
        || flags & (PRESENT_ADMISSIONS | PRESENT_REFERENCES) != 0;
    if !needs_cas {
        return errors.finish();
    }
    let cas = FilesystemCasReader::open(root.join("cas-v1"), cas_limits)?;
    if receipt_present {
        if let Some(reader) = &receipt_reader {
            if let Some(receipt) = errors.observe(reader.load_exact(&cas).map_err(Into::into)) {
                if job.is_none() {
                    job = errors.observe(locate_request_job(
                        state,
                        view,
                        receipt.checkpoint().finalized_block_number,
                        id,
                        WorldwideDay::new(receipt.manifest().wwd),
                        None,
                    ));
                }
                if let Some(authority) = &job {
                    if let Some(receipt) = errors.observe(verify_present_receipt(
                        root,
                        authority,
                        work.export(id)?,
                        cas_limits,
                    )) {
                        if let Some(ack) = &ack {
                            errors.observe((|| -> eyre::Result<()> {
                                compare_surviving_ack_export(
                                    ack,
                                    outbe_node::ocomp::retention::ExportAuthorityV1 {
                                        source_generation: receipt.source_pin_generation(),
                                        lease_generation: receipt.lease_generation(),
                                        manifest_hash: receipt.manifest_hash(),
                                    },
                                )?;
                                ensure!(
                                    ack.reference.export_receipt_digest
                                        == receipt.receipt_ref().transport_digest
                                        && ack.committed == receipt.committed(),
                                    "surviving discovery ACK differs from receipt commitment"
                                );
                                Ok(())
                            })());
                        }
                        manifest = Some(receipt.manifest().clone());
                        increment_present(&mut counts.receipts, 1)?;
                    }
                }
            }
        }
    }
    // Reopen/close the independent sealed input catalog even without job authority.
    let inputs = if input_present {
        errors.observe(
            VerifiedInputChunkRefCatalog::reopen(
                &input_path,
                &cas,
                schema,
                poc_input_list_limits(),
            )
            .map_err(Into::into),
        )
    } else {
        None
    };
    if let Some(inputs) = &inputs {
        errors.observe((|| -> eyre::Result<()> {
            for reference in inputs.exact_cursor()? {
                reference?;
            }
            Ok(())
        })());
    }
    let Some(job) = job else {
        // A bare JobId does not justify historical state scans or invented B/day.
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: canonical comparison lacks retained job locator evidence"
        ))
        .into()));
        return errors.finish();
    };
    let bundle = read_pinned_bundle(root, job.intent.protocol_bundle_hash)?;
    if binding_present {
        match (&binding_reader, &inputs) {
            (Some(reader), Some(inputs)) => {
                let loaded = (|| -> eyre::Result<_> {
                    let binding = reader.load_exact(
                        &cas,
                        &canonical_job_spec(&job)?,
                        bundle.bundle(),
                        inputs,
                    )?;
                    validate_present_manifest(binding.manifest(), &job, &bundle)?;
                    if let Some(ack) = &ack {
                        let request = binding.commit_replay_request();
                        compare_surviving_ack_export(
                            ack,
                            outbe_node::ocomp::retention::ExportAuthorityV1 {
                                source_generation: request.pin_generation,
                                lease_generation: request.lease_generation,
                                manifest_hash: request.manifest_hash,
                            },
                        )?;
                        binding.require_exact_node_replay(&ack.committed)?;
                    }
                    if let Some(receipt_manifest) = &manifest {
                        ensure!(
                            binding.manifest() == receipt_manifest,
                            "present receipt/binding manifest mismatch"
                        );
                    }
                    if let Some(reader) = &receipt_reader {
                        if receipt_present {
                            let receipt = reader.load_exact(&cas)?;
                            ensure!(
                                binding.commit_replay_request() == receipt.commit_replay_request(),
                                "present receipt/binding export authority mismatch"
                            );
                            binding.require_exact_node_replay(&receipt.committed())?;
                        }
                    }
                    if let Some(expected) = work.export(id)? {
                        let request = binding.commit_replay_request();
                        ensure!(
                            request.pin_generation == expected.source_generation
                                && request.lease_generation == expected.lease_generation
                                && request.manifest_hash == expected.manifest_hash,
                            "present binding differs from saved pin export"
                        );
                    }
                    Ok(binding)
                })();
                if let Some(binding) = errors.observe(loaded) {
                    manifest = Some(binding.manifest().clone());
                    increment_present(&mut counts.bindings, 1)?;
                }
            }
            _ => {
                errors.observe::<()>(Err(Incomplete(format!(
                    "job {id}: present binding comparison lacks complete input catalog"
                ))
                .into()));
            }
        }
    }
    // Per-job membership is discarded if the enclosing admission audit fails.
    let membership = ReferenceMembership::create(scratch, protected)?;
    let mut membership_valid = false;
    let mut membership_complete = false;
    if flags & PRESENT_ADMISSIONS != 0 {
        let audit_admissions = (|| -> eyre::Result<()> {
            let inputs = inputs.as_ref().ok_or_else(|| {
                Incomplete(format!(
                    "job {id}: admissions comparison lacks input catalog"
                ))
            })?;
            let admissions = AdmissionCatalogReader::open_existing(&admission_path, &cas, schema)?;
            let audit =
                LocalLysisPlanAuditV1::open_read_only(&admissions, inputs, &cas, &bundle, &schema)?;
            validate_present_manifest(audit.manifest(), &job, &bundle)?;
            if let Some(result) = result_binding {
                compare_surviving_result_binding(
                    result,
                    audit.manifest(),
                    Some(audit.plan().plan_hash(&schema)?),
                )?;
            }
            if let Some(expected) = &manifest {
                ensure!(
                    audit.manifest() == expected,
                    "present admission/receipt manifest mismatch"
                );
            }
            manifest = Some(audit.manifest().clone());
            let present = verify_present_admissions(
                root,
                audit.manifest(),
                job.intent.frozen_metadosis_values.lysis_limit_minor,
                job.intent.logical_evaluation_time,
                cas_limits,
                None,
                &mut |reference| membership.insert(id, reference),
            )?;
            increment_present(&mut counts.admissions, u64::from(present.present))?;
            membership_valid = true;
            if present.present == present.expected {
                let chunks = verify_complete_present_results(
                    context,
                    &job,
                    &bundle,
                    &audit,
                    &membership,
                    flags & PRESENT_REFERENCES != 0,
                )?;
                increment_present(&mut counts.results, chunks)?;
                membership_complete = true;
            }
            Ok(())
        })();
        if errors.observe(audit_admissions).is_none() {
            membership_valid = false;
        }
    }
    if let (Some(result), Some(manifest)) = (result_binding, manifest.as_ref()) {
        errors.observe(compare_surviving_result_binding(result, manifest, None));
    }
    if let Some(inputs) = &inputs {
        let verify = (|| -> eyre::Result<u64> {
            let mut cursor = inputs.exact_verified_cursor(&cas, bundle.bundle())?;
            let expected_count = u32::try_from(cursor.len())?;
            let mut ordered = outbe_ocomp_protocol::StreamingOrderedListRoot::new(
                outbe_ocomp_protocol::ListKind::InputChunkReferences,
                expected_count,
            )?;
            let (mut bytes, mut records, mut tribute) = (0_u64, 0_u64, 0_u64);
            for entry in &mut cursor {
                let entry = entry?;
                ensure!(
                    entry.chunk.job_id == id
                        && entry.chunk.protocol_bundle_hash == job.intent.protocol_bundle_hash,
                    "present input chunk differs from canonical job"
                );
                ordered.push(
                    &entry.reference.encode_canonical_record(&schema)?,
                    schema.max_bounded_bytes,
                )?;
                increment_present(&mut bytes, entry.reference.encoded_bytes)?;
                increment_present(&mut records, u64::from(entry.reference.record_count))?;
                if entry.reference.kind == outbe_ocomp_protocol::input::InputChunkKind::Tribute {
                    increment_present(&mut tribute, u64::from(entry.reference.record_count))?;
                }
            }
            let list_root = ordered.finish()?;
            let expected = manifest.as_ref().ok_or_else(|| Incomplete(format!("job {id}: sealed catalog internally valid; full manifest authority comparison unavailable")))?;
            validate_present_manifest(expected, &job, &bundle)?;
            ensure!(
                expected.input_chunk_count == expected_count
                    && expected.input_chunk_list_root == list_root
                    && expected.exact_encoded_bytes == bytes
                    && u64::from(expected.exact_record_count) == records
                    && u64::from(expected.tribute_count) == tribute,
                "present input catalog differs from authenticated manifest"
            );
            Ok(u64::from(expected_count))
        })();
        if let Some(chunks) = errors.observe(verify) {
            increment_present(&mut counts.inputs, chunks)?;
        }
    }
    if flags & PRESENT_REFERENCES != 0 {
        // Each full ref is checked against same-job native plan/result evidence.
        // A successful partial catalog proves positive membership only.
        let verified = (|| -> eyre::Result<()> {
            let mut count = 0_u64;
            work.visit_refs(id, &mut |reference| {
                if !membership_valid {
                    cas.read_verified(&reference)?;
                    return Err(Incomplete(format!("job {id}: materialization reference lacks available plan membership evidence")).into());
                }
                membership.verify(id, &reference, &cas, membership_complete)?;
                increment_present(&mut count, 1)
            })?;
            increment_present(&mut counts.references, count)?;
            if !membership_complete {
                return Err(Incomplete(format!(
                    "job {id}: retained reference membership is partial; certified output-root comparison unavailable"
                )).into());
            }
            Ok(())
        })();
        errors.observe(verified);
    }
    errors.finish()
}

fn verify_complete_present_results<'a>(
    context: &PresentJobContext<'_, '_>,
    job: &OcompJobRecordV1,
    bundle: &PinnedProtocolBundle,
    audit: &'a LocalLysisPlanAuditV1<'a>,
    membership: &ReferenceMembership,
    require_certified: bool,
) -> eyre::Result<u64> {
    let state = context.state;
    let scratch = context.scratch;
    let protected = context.protected;
    use outbe_ocomp::lysis_result_catalog::{
        ExactLysisResultCatalogCursorV1, LysisResultCatalogStepV1,
    };
    use outbe_ocomp_protocol::{
        shuffle::ShuffleBucketRecordV1, ListKind, StreamingOrderedListRoot,
    };
    let limits = poc_schema_limits();
    let id = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("result catalog without finalized job"))?
        .job_id;
    let certified = state.nod_certified_generation(WorldwideDay::new(job.intent.wwd))?;
    if require_certified {
        let expected = certified.as_ref().ok_or_else(|| Incomplete(format!("job {id}: retained materialization reference lacks certified generation comparison")))?;
        ensure!(
            expected.job_id == id,
            "materialization reference job differs from canonical certified generation"
        );
    }
    let sorted = PresentJobUnion::create(scratch, protected)?;
    let mut nod = StreamingOrderedListRoot::new(ListKind::NodActions, audit.plan().tribute_count)?;
    let mut output = StreamingOrderedListRoot::new(
        ListKind::CompleteOutputManifest,
        audit.plan().primary_work_unit_count,
    )?;
    let mut chunks = 0_u64;
    let (mut allocation, mut cost) = (U256::ZERO, U256::ZERO);
    let mut complete = false;
    for step in ExactLysisResultCatalogCursorV1::open(audit)? {
        match step? {
            LysisResultCatalogStepV1::Chunk(chunk) => {
                membership.insert(id, chunk.producer_artifact_ref())?;
                membership.insert(id, &chunk.output_manifest_entry().result_chunk_ref)?;
                output.push(
                    &chunk
                        .output_manifest_entry()
                        .encode_canonical_record(&limits)?,
                    limits.max_bounded_bytes,
                )?;
                let tx = sorted.db.tx_mut()?;
                for action in &chunk.chunk().ordered_nod_actions {
                    nod.push(
                        &action.encode_canonical_record(&limits)?,
                        limits.max_bounded_bytes,
                    )?;
                    // Same native record projection as Lysis finalizer::stream_result_chunks.
                    // Scratch sorting replaces its external globally ordered record input.
                    let record = ShuffleBucketRecordV1 {
                        bucket_key: action.bucket_key,
                        raw_ordinal: action.raw_ordinal,
                        tribute_id: action.tribute_id,
                        nod_id: action.nod_id,
                    };
                    let mut key = vec![b'b'];
                    key.extend_from_slice(record.bucket_key.as_slice());
                    key.extend_from_slice(&record.raw_ordinal.to_be_bytes());
                    ensure!(
                        tx.get::<InventoryRows>(key.clone())?.is_none(),
                        "duplicate native bucket record sort key"
                    );
                    tx.put::<InventoryRows>(key, record.encode_canonical_record(&limits)?)?;
                    allocation = allocation
                        .checked_add(action.gratis_load_minor)
                        .ok_or_else(|| eyre::eyre!("result allocation overflow"))?;
                    cost = cost
                        .checked_add(action.settlement_cost_minor)
                        .ok_or_else(|| eyre::eyre!("result NOD cost overflow"))?;
                }
                tx.commit()?;
                increment_present(&mut chunks, 1)?;
            }
            LysisResultCatalogStepV1::Complete => complete = true,
            _ => {}
        }
    }
    ensure!(complete, "native result catalog did not reach Complete");
    let mut bucket =
        StreamingOrderedListRoot::new(ListKind::BucketRecords, audit.plan().tribute_count)?;
    sorted.visit_prefix(b"b", &mut |_, record| {
        bucket.push(&record, limits.max_bounded_bytes)?;
        Ok(())
    })?;
    let (nod_root, bucket_root, output_root) = (nod.finish()?, bucket.finish()?, output.finish()?);
    if let Some(expected) = certified.filter(|expected| expected.job_id == id) {
        ensure!(
            expected.protocol_bundle_hash == job.intent.protocol_bundle_hash
                && expected.program_semantics_hash == bundle.bundle().lysis_program_semantics_hash
                && expected.tribute_count == audit.plan().tribute_count
                && expected.nod_count == audit.plan().tribute_count
                && expected.bucket_count == audit.plan().tribute_count
                && expected.nod_root == nod_root
                && expected.bucket_root == bucket_root
                && expected.output_manifest_root == output_root
                && expected.lysis_allocation_minor == allocation
                && expected.nod_amount_total == cost,
            "complete present result catalog differs from canonical certified outputs"
        );
    }
    Ok(chunks)
}

struct PresentRetainedCount(u64);
impl outbe_tribute::RetainedTributeAuditVisitor for PresentRetainedCount {
    fn visit_retained(
        &mut self,
        _: outbe_tribute::RetainedTributeAuditEntry,
    ) -> Result<(), outbe_compressed_entities::CeAuditError> {
        self.0 = self.0.checked_add(1).ok_or_else(|| {
            outbe_compressed_entities::CeAuditError::Invalid("retained body count overflow".into())
        })?;
        Ok(())
    }
}

fn verify_present_projection_structure(
    layout: &crate::snapshot::config::RequestedLayout,
    scratch: &Path,
) -> eyre::Result<(u64, u64)> {
    use outbe_compressed_entities::{
        CeAuditLimits, CeAuditWork, CeDomain, IdPageRequest, MAX_ID_PAGE_LIMIT,
    };
    use outbe_nod::NodRepositoryReader;
    use outbe_offchain_storage::{RocksDbReader, StorageReaderHandle};
    use outbe_tribute::{RetainedTributeReader, TributeRepositoryReader};
    let location = layout
        .projection
        .as_ref()
        .ok_or_else(|| Incomplete("missing selected OCOMP projection configuration".into()))?;
    if !existing_file(&location.root.join("CURRENT"))? {
        return Err(Incomplete("missing selected OCOMP projection CURRENT".into()).into());
    }
    let secondary = tempfile::Builder::new()
        .prefix("ocomp-present-bodies-")
        .tempdir_in(scratch)?;
    let reader: StorageReaderHandle =
        std::sync::Arc::new(RocksDbReader::open(&location.root, secondary.path())?);
    let work = CeAuditWork::create(
        secondary.path().join("audit-work"),
        CeAuditLimits::default(),
    )?;
    let tribute = TributeRepositoryReader::new(reader.clone());
    let nod = NodRepositoryReader::new(reader.clone());
    tribute.audit_indexes(&work)?;
    nod.audit_indexes(&work)?;
    let mut live = 0_u64;
    for domain in [CeDomain::Tribute, CeDomain::NodItem, CeDomain::NodBucket] {
        let mut after = None;
        loop {
            let request = IdPageRequest {
                after,
                limit: MAX_ID_PAGE_LIMIT,
            };
            let page = match domain {
                CeDomain::Tribute => tribute.scan_stored_bodies(request)?,
                CeDomain::NodItem => nod.scan_stored_items(request)?,
                CeDomain::NodBucket => nod.scan_stored_buckets(request)?,
            };
            increment_present(&mut live, u64::try_from(page.entries.len())?)?;
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
    }
    let mut retained = PresentRetainedCount(0);
    RetainedTributeReader::new(reader.clone()).audit_retained(&work, &mut retained)?;
    // Reader-owned repositories and scratch work drop before secondary cleanup.
    Ok((live, retained.0))
}
