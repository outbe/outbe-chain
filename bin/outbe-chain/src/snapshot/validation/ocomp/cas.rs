//! cas obligations for the offline OCOMP audit.
use super::*;

#[derive(Debug, Default)]
pub(crate) struct CasAudit {
    pub objects: u64,
    pub bytes: u64,
}

fn verify_cas_object_entry(
    cas: &FilesystemCasReader,
    prefix: &str,
    entry: std::fs::DirEntry,
    (limits, maximum_objects): (CasLimits, Option<u64>),
    result: &mut CasAudit,
) -> eyre::Result<()> {
    if maximum_objects.is_some_and(|maximum| result.objects >= maximum) {
        return Err(
            Incomplete(format!("CAS scan stopped after {} objects", result.objects)).into(),
        );
    }
    if !entry.file_type()?.is_file() {
        return Err(SnapshotOcompError::CasObjectNotRegularFile.into());
    }
    let suffix = entry.file_name();
    let suffix = suffix
        .to_str()
        .ok_or(SnapshotOcompError::InvalidCasObjectLocator)?;
    let encoded = format!("{prefix}{suffix}");
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(&encoded, &mut digest)?;
    if hex::encode(digest) != encoded {
        return Err(SnapshotOcompError::NoncanonicalCasDigestLocator.into());
    }
    let length = entry.metadata()?.len();
    let total = result
        .bytes
        .checked_add(length)
        .ok_or(SnapshotOcompError::CasTotalByteCountOverflow)?;
    if total > limits.max_total_bytes {
        return Err(SnapshotIncompleteError::CasScanByteBudgetExceeded {
            max_total_bytes: limits.max_total_bytes,
            objects: result.objects,
            bytes: result.bytes,
        }
        .into_report());
    }
    let reference = outbe_ocomp_protocol::CasObjectRefV1 {
        transport_digest: B256::from(digest),
        encoded_bytes: length,
        expected_ocb1_kind: None,
    };
    cas.read_verified(&reference)?;
    result.objects = result
        .objects
        .checked_add(1)
        .ok_or(SnapshotOcompError::CasObjectCountOverflow)?;
    result.bytes = total;
    Ok(())
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
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(SnapshotOcompError::CasRootNotDirectory.into());
            }
        }
    }
    let scan = || -> eyre::Result<CasAudit> {
        let cas = FilesystemCasReader::open(&root, limits)?;
        let mut result = CasAudit::default();
        for shard in std::fs::read_dir(root.join("objects"))? {
            let shard = shard?;
            let prefix = shard.file_name();
            let prefix = prefix.to_str().ok_or(SnapshotOcompError::InvalidCasShard)?;
            if !shard.file_type()?.is_dir() || prefix.len() != 2 {
                return Err(SnapshotOcompError::InvalidNativeCasShard.into());
            }
            for entry in std::fs::read_dir(shard.path())? {
                verify_cas_object_entry(
                    &cas,
                    prefix,
                    entry?,
                    (limits, maximum_objects),
                    &mut result,
                )?;
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
        let finalized = job
            .finalized
            .as_ref()
            .ok_or(SnapshotOcompError::PresentReceiptLacksFinality)?;
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
        require(
            receipt.job_id() == spec.summary.job_id && manifest.job_id == spec.summary.job_id,
            SnapshotOcompError::ReceiptManifestJobIdMismatch,
        )?;
        require(
            manifest.protocol_bundle_hash == spec.summary.protocol_bundle_hash
                && manifest.protocol_bundle_hash == job.intent.protocol_bundle_hash,
            SnapshotOcompError::ReceiptManifestBundleMismatch,
        )?;
        require(
            manifest.attempt == job.intent.attempt && manifest.wwd == job.intent.wwd,
            SnapshotOcompError::ReceiptManifestAttemptOrWwdMismatch,
        )?;
        require(
            manifest.tribute_count == job.intent.authenticated_day_count
                && manifest.tribute_nominal_total == job.intent.authenticated_day_nominal,
            SnapshotOcompError::ReceiptManifestTributeMismatch,
        )?;
        let checkpoint = CheckpointIdentityV1 {
            finalized_block_number: job.intent_height,
            finalized_block_hash: finalized.finalized_request_block_hash,
            finalized_state_root: finalized.finalized_request_state_root,
        };
        require(
            receipt.checkpoint() == &checkpoint && manifest.checkpoint == checkpoint,
            SnapshotOcompError::ReceiptCheckpointMismatch,
        )?;
        let bundle = read_pinned_bundle(ocomp_root, job.intent.protocol_bundle_hash)?;
        manifest.validate_against_bundle(bundle.bundle(), &limits)?;
        if let Some(expected) = expected_export {
            require(
                expected.source_generation == receipt.source_pin_generation()
                    && expected.lease_generation == receipt.lease_generation()
                    && expected.manifest_hash == receipt.manifest_hash(),
                SnapshotOcompError::SavedPinExportDiffersFromReceipt,
            )?;
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
