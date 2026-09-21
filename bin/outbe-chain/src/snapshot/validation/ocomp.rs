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
    pub receipt: outbe_ocomp::export_receipt::VerifiedExportReceipt,
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
pub(crate) fn verify_local_result(
    ocomp_root: &Path,
    job: &OcompJobRecordV1,
) -> eyre::Result<Option<LocalResultAudit>> {
    use outbe_node::ocomp::local_result::LocalLysisResultReader;
    use outbe_ocomp_protocol::result::LysisResultV1;

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
    let result = LysisResultV1::decode_canonical(&loaded.canonical_result, &limits)?;
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
    Ok(Some(LocalResultAudit {
        result,
        terminal_digest_checked: completed.is_some(),
    }))
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
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
        input::CheckpointIdentityV1,
    };
    let check = || -> eyre::Result<ExportInputsAudit> {
        let limits = poc_schema_limits();
        let finalized = job.finalized.as_ref().ok_or_else(|| {
            eyre::eyre!("complete export lacks canonical finalized job authority")
        })?;
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
            receipt,
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

impl<'a, 'b> CanonicalInventory<'a, 'b> {
    pub(crate) fn scan(
        state: &'a CanonicalState<'b>,
        scratch_parent: &Path,
        protected: &ProtectedPaths,
        maximum_records: Option<u64>,
    ) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[scratch_parent.to_path_buf()])?;
        // The owner bounds its native aggregate before allocation and validates
        // live scheduler/FSM/job equivalence. No local directory seeds this list.
        let active_jobs = state.live_ocomp_jobs()?;
        let active_intents = u64::try_from(active_jobs.len())?;
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
        }

        let total_series = state.intex_total_series()?;
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
            let day_key = inventory_key(b'w', &day.value().to_be_bytes());
            if tx.get::<InventoryRows>(day_key.clone())?.is_some() {
                continue;
            }
            tx.put::<InventoryRows>(day_key, Vec::new())?;
            bounds.days += 1;
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
            let bitmap = verify_paid_bitmap(
                certified.contributor_count,
                round.paid_leaf_count,
                maximum_records,
                |word| state.intex_paid_leaves_word(day.value(), word),
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
