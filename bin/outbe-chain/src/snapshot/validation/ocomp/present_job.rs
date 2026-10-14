//! present job obligations for the offline OCOMP audit.
use super::*;
use outbe_ocomp::{
    export_binding::ExportedManifestBindingReader, export_receipt::ExportReceiptReader,
};
use outbe_ocomp_protocol::input::InputManifestV1;
use std::path::PathBuf;

struct PresentJobReaders {
    receipt_reader: Option<ExportReceiptReader>,
    binding_reader: Option<ExportedManifestBindingReader>,
    receipt_present: bool,
    binding_present: bool,
    input_present: bool,
    input_path: PathBuf,
    admission_path: PathBuf,
}

impl PresentJobReaders {
    fn open(
        root: &Path,
        id: B256,
        (flags, active, has_ack): (u8, bool, bool),
        errors: &mut PresentJoinErrors,
    ) -> eyre::Result<Self> {
        let name = hex::encode(id);
        let receipt_path = root.join("exporter-v1/receipts").join(&name);
        let binding_path = root.join("supervisor-v1/export-bindings").join(&name);
        let input_path = root.join("exporter-v1/input-refs").join(&name);
        let admission_path = root
            .join("supervisor-v1/jobs")
            .join(&name)
            .join("admissions");
        let schema = poc_schema_limits();
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
                ExportedManifestBindingReader::open_existing(&binding_path, schema)
                    .map_err(Into::into),
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
        observe_incomplete_reader_flags(
            id,
            &receipt_path,
            (flags, active, has_ack),
            (receipt_present, input_present),
            errors,
        )?;
        Ok(Self {
            receipt_reader,
            binding_reader,
            receipt_present,
            binding_present,
            input_present,
            input_path,
            admission_path,
        })
    }

    fn needs_cas(&self, flags: u8) -> bool {
        self.receipt_present
            || self.binding_present
            || self.input_present
            || flags & (PRESENT_ADMISSIONS | PRESENT_REFERENCES) != 0
    }
}

fn observe_incomplete_reader_flags(
    id: B256,
    receipt_path: &Path,
    (flags, active, has_ack): (u8, bool, bool),
    (receipt_present, input_present): (bool, bool),
    errors: &mut PresentJoinErrors,
) -> eyre::Result<()> {
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
    if active && has_ack && !receipt_present {
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: active acknowledged export lacks its required complete receipt"
        ))
        .into()));
    }
    Ok(())
}

fn verify_ack_matches_receipt(
    ack: &outbe_ocomp::discovery_spool::StoredDiscoveryAckV1,
    receipt: &outbe_ocomp::export_receipt::VerifiedExportReceipt,
) -> eyre::Result<()> {
    compare_surviving_ack_export(
        ack,
        outbe_node::ocomp::retention::ExportAuthorityV1 {
            source_generation: receipt.source_pin_generation(),
            lease_generation: receipt.lease_generation(),
            manifest_hash: receipt.manifest_hash(),
        },
    )?;
    if ack.reference.export_receipt_digest != receipt.receipt_ref().transport_digest
        || ack.committed != receipt.committed()
    {
        return Err(SnapshotOcompError::SurvivingAckReceiptCommitmentMismatch.into());
    }
    Ok(())
}

fn verify_present_receipt_step(
    context: &PresentJobContext<'_, '_>,
    id: B256,
    (readers, cas, ack): (
        &PresentJobReaders,
        &FilesystemCasReader,
        Option<&outbe_ocomp::discovery_spool::StoredDiscoveryAckV1>,
    ),
    job: &mut Option<OcompJobRecordV1>,
    errors: &mut PresentJoinErrors,
) -> eyre::Result<Option<InputManifestV1>> {
    if !readers.receipt_present {
        return Ok(None);
    }
    let Some(reader) = &readers.receipt_reader else {
        return Ok(None);
    };
    let Some(receipt) = errors.observe(reader.load_exact(cas).map_err(Into::into)) else {
        return Ok(None);
    };
    if job.is_none() {
        *job = errors.observe(locate_request_job(
            context.state,
            context.view,
            (
                receipt.checkpoint().finalized_block_number,
                id,
                WorldwideDay::new(receipt.manifest().wwd),
            ),
            None,
        ));
    }
    let Some(authority) = job.as_ref() else {
        return Ok(None);
    };
    let Some(receipt) = errors.observe(verify_present_receipt(
        &context.layout.ocomp_root,
        authority,
        context.work.export(id)?,
        context.cas_limits,
    )) else {
        return Ok(None);
    };
    if let Some(ack) = ack {
        errors.observe(verify_ack_matches_receipt(ack, &receipt));
    }
    Ok(Some(receipt.manifest().clone()))
}

fn reopen_present_input_catalog(
    readers: &PresentJobReaders,
    cas: &FilesystemCasReader,
    errors: &mut PresentJoinErrors,
) -> Option<VerifiedInputChunkRefCatalog> {
    if !readers.input_present {
        return None;
    }
    let inputs = errors.observe(
        VerifiedInputChunkRefCatalog::reopen(
            &readers.input_path,
            cas,
            poc_schema_limits(),
            poc_input_list_limits(),
        )
        .map_err(Into::into),
    );
    if let Some(inputs) = &inputs {
        errors.observe((|| -> eyre::Result<()> {
            for reference in inputs.exact_cursor()? {
                reference?;
            }
            Ok(())
        })());
    }
    inputs
}

fn verify_present_binding_step(
    context: &PresentJobContext<'_, '_>,
    id: B256,
    (readers, cas, inputs): (
        &PresentJobReaders,
        &FilesystemCasReader,
        Option<&VerifiedInputChunkRefCatalog>,
    ),
    (job, bundle, ack): (
        &OcompJobRecordV1,
        &PinnedProtocolBundle,
        Option<&outbe_ocomp::discovery_spool::StoredDiscoveryAckV1>,
    ),
    (manifest, errors): (&mut Option<InputManifestV1>, &mut PresentJoinErrors),
) -> eyre::Result<bool> {
    if !readers.binding_present {
        return Ok(false);
    }
    let (Some(reader), Some(inputs)) = (&readers.binding_reader, inputs) else {
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: present binding comparison lacks complete input catalog"
        ))
        .into()));
        return Ok(false);
    };
    let loaded = load_and_verify_present_binding(
        context,
        id,
        (readers, reader, cas, inputs),
        (job, bundle, ack),
        manifest.as_ref(),
    );
    if let Some(binding_manifest) = errors.observe(loaded) {
        *manifest = Some(binding_manifest);
        return Ok(true);
    }
    Ok(false)
}

fn load_and_verify_present_binding(
    context: &PresentJobContext<'_, '_>,
    id: B256,
    (readers, reader, cas, inputs): (
        &PresentJobReaders,
        &ExportedManifestBindingReader,
        &FilesystemCasReader,
        &VerifiedInputChunkRefCatalog,
    ),
    (job, bundle, ack): (
        &OcompJobRecordV1,
        &PinnedProtocolBundle,
        Option<&outbe_ocomp::discovery_spool::StoredDiscoveryAckV1>,
    ),
    receipt_manifest: Option<&InputManifestV1>,
) -> eyre::Result<InputManifestV1> {
    let binding = reader.load_integrity(cas, &canonical_job_spec(job)?, bundle.bundle(), inputs)?;
    validate_present_manifest(binding.manifest(), job, bundle)?;
    if let Some(ack) = ack {
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
    if receipt_manifest.is_some_and(|expected| binding.manifest() != expected) {
        return Err(SnapshotOcompError::PresentReceiptBindingManifestMismatch.into());
    }
    if let Some(receipt_reader) = readers
        .receipt_reader
        .as_ref()
        .filter(|_| readers.receipt_present)
    {
        let receipt = receipt_reader.load_exact(cas)?;
        if binding.commit_replay_request() != receipt.commit_replay_request() {
            return Err(SnapshotOcompError::PresentReceiptBindingExportMismatch.into());
        }
        binding.require_exact_node_replay(&receipt.committed())?;
    }
    if let Some(expected) = context.work.export(id)? {
        let request = binding.commit_replay_request();
        if (
            request.pin_generation,
            request.lease_generation,
            request.manifest_hash,
        ) != (
            expected.source_generation,
            expected.lease_generation,
            expected.manifest_hash,
        ) {
            return Err(SnapshotOcompError::PresentBindingSavedPinMismatch.into());
        }
    }
    Ok(binding.manifest().clone())
}

pub(super) fn verify_present_job(
    context: &PresentJobContext<'_, '_>,
    id: B256,
    flags: u8,
    active: bool,
    counts: &mut PresentArtifactCounts,
) -> eyre::Result<()> {
    let root = &context.layout.ocomp_root;
    let mut errors = PresentJoinErrors::default();
    let mut job = context.work.job(id)?;
    let ack = context.work.ack(id)?;
    if let (Some(ack), Some(export)) = (&ack, context.work.export(id)?) {
        errors.observe(compare_surviving_ack_export(ack, export));
    }
    let result_binding = context.work.result_binding(id)?;
    let readers = PresentJobReaders::open(root, id, (flags, active, ack.is_some()), &mut errors)?;
    if !readers.needs_cas(flags) {
        return errors.finish();
    }
    let cas = FilesystemCasReader::open(root.join("cas-v1"), context.cas_limits)?;
    let mut manifest = verify_present_receipt_step(
        context,
        id,
        (&readers, &cas, ack.as_ref()),
        &mut job,
        &mut errors,
    )?;
    if manifest.is_some() {
        increment_present(&mut counts.receipts, 1)?;
    }
    // Reopen/close the independent sealed input catalog even without job authority.
    let inputs = reopen_present_input_catalog(&readers, &cas, &mut errors);
    let Some(job) = job else {
        // A bare JobId does not justify historical state scans or invented B/day.
        errors.observe::<()>(Err(Incomplete(format!(
            "job {id}: canonical comparison lacks retained job locator evidence"
        ))
        .into()));
        return errors.finish();
    };
    let bundle = read_pinned_bundle(root, job.intent.protocol_bundle_hash)?;
    if verify_present_binding_step(
        context,
        id,
        (&readers, &cas, inputs.as_ref()),
        (&job, &bundle, ack.as_ref()),
        (&mut manifest, &mut errors),
    )? {
        increment_present(&mut counts.bindings, 1)?;
    }
    // Per-job membership is discarded if the enclosing admission audit fails.
    let membership = ReferenceMembership::create(context.scratch, context.protected)?;
    let (mut membership_valid, mut membership_complete) = (false, false);
    if flags & PRESENT_ADMISSIONS != 0 {
        let audit_admissions = PresentAdmissionsVerify {
            context,
            id,
            admission_path: &readers.admission_path,
            cas: &cas,
            job: &job,
            bundle: &bundle,
            inputs: inputs.as_ref(),
            result_binding,
            membership: &membership,
            require_certified: flags & PRESENT_REFERENCES != 0,
        }
        .run(&mut manifest, counts);
        if let Some(complete) = errors.observe(audit_admissions) {
            membership_valid = true;
            membership_complete = complete;
        }
    }
    if let (Some(result), Some(manifest)) = (result_binding, manifest.as_ref()) {
        errors.observe(compare_surviving_result_binding(result, manifest, None));
    }
    if let Some(inputs) = &inputs {
        let verify = verify_present_input_catalog(
            id,
            (&cas, &bundle),
            inputs,
            (&job, manifest.as_ref()),
            &membership,
        );
        if let Some(chunks) = errors.observe(verify) {
            increment_present(&mut counts.inputs, chunks)?;
        }
    }
    if flags & PRESENT_REFERENCES != 0 {
        let verified = verify_present_reference_membership(
            id,
            context.work,
            &cas,
            (&membership, membership_valid, membership_complete),
            counts,
        );
        errors.observe(verified);
    }
    errors.finish()
}
