//! exports obligations for the offline OCOMP audit.
use super::*;

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

pub(super) fn canonical_job_spec(
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
    view: &crate::snapshot::native::RethReadOnlyView,
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
