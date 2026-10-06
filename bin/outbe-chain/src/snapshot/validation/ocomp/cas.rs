//! cas obligations for the offline OCOMP audit.
use super::*;

#[derive(Debug, Default)]
pub(crate) struct CasAudit {
    pub objects: u64,
    pub bytes: u64,
}

/// Check transport integrity of every published native CAS object, including
/// unreferenced historical objects. Typed artifact/job relationships are checked
/// separately through their native references. This does not infer an OCB1 kind.
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
