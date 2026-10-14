//! Present admissions, input catalog, reference membership, and result obligations for the offline OCOMP audit.
use super::*;
use outbe_ocomp_protocol::input::InputManifestV1;

pub(super) struct PresentAdmissionsVerify<'a, 'b, 'c> {
    pub(super) context: &'a PresentJobContext<'b, 'c>,
    pub(super) id: B256,
    pub(super) admission_path: &'a Path,
    pub(super) cas: &'a FilesystemCasReader,
    pub(super) job: &'a OcompJobRecordV1,
    pub(super) bundle: &'a PinnedProtocolBundle,
    pub(super) inputs: Option<&'a VerifiedInputChunkRefCatalog>,
    pub(super) result_binding: Option<(B256, B256)>,
    pub(super) membership: &'a ReferenceMembership,
    pub(super) require_certified: bool,
}

impl PresentAdmissionsVerify<'_, '_, '_> {
    pub(super) fn run(
        &self,
        manifest: &mut Option<InputManifestV1>,
        counts: &mut PresentArtifactCounts,
    ) -> eyre::Result<bool> {
        let schema = poc_schema_limits();
        let inputs = self.inputs.ok_or_else(|| {
            Incomplete(format!(
                "job {}: admissions comparison lacks input catalog",
                self.id
            ))
        })?;
        let admissions =
            AdmissionCatalogReader::open_existing(self.admission_path, self.cas, schema)?;
        let audit = outbe_ocomp::lysis_plan_audit::open_read_only_local_plan_integrity_audit(
            &admissions,
            inputs,
            self.cas,
            self.bundle,
            &schema,
        )?;
        validate_present_manifest(audit.manifest(), self.job, self.bundle)?;
        if let Some(result) = self.result_binding {
            compare_surviving_result_binding(
                result,
                audit.manifest(),
                Some(audit.plan().plan_hash(&schema)?),
            )?;
        }
        if let Some(expected) = manifest.as_ref() {
            ensure!(
                audit.manifest() == expected,
                "present admission/receipt manifest mismatch"
            );
        }
        *manifest = Some(audit.manifest().clone());
        let present = verify_present_admissions(
            &self.context.layout.ocomp_root,
            (
                audit.manifest(),
                self.job.intent.frozen_metadosis_values.lysis_limit_minor,
                self.job.intent.logical_evaluation_time,
            ),
            self.context.cas_limits,
            None,
            &mut |reference| self.membership.insert(self.id, reference),
        )?;
        increment_present(&mut counts.admissions, u64::from(present.present))?;
        if present.present == present.expected {
            let chunks = verify_complete_present_results(
                self.context,
                (self.job, self.bundle),
                &audit,
                self.membership,
                self.require_certified,
            )?;
            increment_present(&mut counts.results, chunks)?;
            return Ok(true);
        }
        Ok(false)
    }
}

pub(super) fn verify_present_input_catalog(
    id: B256,
    (cas, bundle): (&FilesystemCasReader, &PinnedProtocolBundle),
    inputs: &VerifiedInputChunkRefCatalog,
    (job, manifest): (&OcompJobRecordV1, Option<&InputManifestV1>),
    membership: &ReferenceMembership,
) -> eyre::Result<u64> {
    let schema = poc_schema_limits();
    let mut cursor = inputs.exact_verified_integrity_cursor(cas, bundle.bundle())?;
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
    let expected = manifest.ok_or_else(|| {
        Incomplete(format!(
            "job {id}: sealed catalog internally valid; full manifest authority comparison unavailable"
        ))
    })?;
    validate_present_manifest(expected, job, bundle)?;
    ensure!(
        (
            expected.input_chunk_count,
            expected.input_chunk_list_root,
            expected.exact_encoded_bytes,
            u64::from(expected.exact_record_count),
            u64::from(expected.tribute_count),
        ) == (expected_count, list_root, bytes, records, tribute),
        "present input catalog differs from authenticated manifest"
    );
    record_tribute_input_membership(id, (cas, bundle), inputs, job, membership)?;
    Ok(u64::from(expected_count))
}

fn record_tribute_input_membership(
    id: B256,
    (cas, bundle): (&FilesystemCasReader, &PinnedProtocolBundle),
    inputs: &VerifiedInputChunkRefCatalog,
    job: &OcompJobRecordV1,
    membership: &ReferenceMembership,
) -> eyre::Result<()> {
    for entry in inputs.exact_verified_integrity_cursor(cas, bundle.bundle())? {
        let entry = entry?;
        if entry.reference.kind == outbe_ocomp_protocol::input::InputChunkKind::Tribute {
            ensure!(
                entry.chunk.job_id == id
                    && entry.chunk.protocol_bundle_hash == job.intent.protocol_bundle_hash,
                "present input chunk differs from canonical job"
            );
            membership.insert(
                id,
                &outbe_ocomp_protocol::CasObjectRefV1 {
                    transport_digest: entry.reference.transport_digest,
                    encoded_bytes: entry.reference.encoded_bytes,
                    expected_ocb1_kind: Some(
                        outbe_ocomp_protocol::ObjectKind::AuthenticatedInputChunkV1.tag(),
                    ),
                },
            )?;
        }
    }
    Ok(())
}

pub(super) fn verify_present_reference_membership(
    id: B256,
    work: &PresentJobUnion,
    cas: &FilesystemCasReader,
    (membership, membership_valid, membership_complete): (&ReferenceMembership, bool, bool),
    counts: &mut PresentArtifactCounts,
) -> eyre::Result<()> {
    let mut count = 0_u64;
    work.visit_refs(id, &mut |reference| {
        if !membership_valid {
            cas.read_verified(&reference)?;
            return Err(Incomplete(format!(
                "job {id}: materialization reference lacks available plan membership evidence"
            ))
            .into());
        }
        membership.verify(id, &reference, cas, membership_complete)?;
        increment_present(&mut count, 1)
    })?;
    increment_present(&mut counts.references, count)?;
    if !membership_complete {
        return Err(Incomplete(format!(
            "job {id}: retained reference membership is partial; certified output-root comparison unavailable"
        ))
        .into());
    }
    Ok(())
}

pub(super) fn verify_complete_present_results<'a>(
    context: &PresentJobContext<'_, '_>,
    (job, bundle): (&OcompJobRecordV1, &PinnedProtocolBundle),
    audit: &'a LocalLysisPlanAuditV1<'a>,
    membership: &ReferenceMembership,
    require_certified: bool,
) -> eyre::Result<u64> {
    use outbe_ocomp::lysis_result_catalog::{
        ExactLysisResultCatalogCursorV1, LysisResultCatalogStepV1,
    };
    use outbe_ocomp_protocol::{ListKind, StreamingOrderedListRoot};
    let limits = poc_schema_limits();
    let id = job
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("result catalog without finalized job"))?
        .job_id;
    let certified = context
        .state
        .nod_certified_generation(WorldwideDay::new(job.intent.wwd))?;
    if require_certified {
        let expected = certified.as_ref().ok_or_else(|| {
            Incomplete(format!(
                "job {id}: retained materialization reference lacks certified generation comparison"
            ))
        })?;
        ensure!(
            expected.job_id == id,
            "materialization reference job differs from canonical certified generation"
        );
    }
    let sorted = PresentJobUnion::create(context.scratch, context.protected)?;
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
                allocation = allocation
                    .checked_add(chunk.summary().lysis_allocation_minor)
                    .ok_or_else(|| eyre::eyre!("result allocation overflow"))?;
                cost = cost
                    .checked_add(chunk.summary().nod_cost_total)
                    .ok_or_else(|| eyre::eyre!("result NOD cost overflow"))?;
                record_result_chunk_actions(
                    &sorted,
                    &mut nod,
                    &chunk.chunk().ordered_nod_actions,
                    &limits,
                )?;
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
    let roots = (nod.finish()?, bucket.finish()?, output.finish()?);
    if let Some(expected) = certified.filter(|expected| expected.job_id == id) {
        verify_certified_result_outputs(&expected, (job, bundle), audit, roots, (allocation, cost))?;
    }
    Ok(chunks)
}

fn record_result_chunk_actions(
    sorted: &PresentJobUnion,
    nod: &mut outbe_ocomp_protocol::StreamingOrderedListRoot,
    actions: &[outbe_ocomp_protocol::result::NodActionV1],
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> eyre::Result<()> {
    use outbe_lysis::program_v1::phases::NodBucketKeyV1;
    use outbe_ocomp_protocol::shuffle::ShuffleBucketRecordV1;
    let tx = sorted.db.tx_mut()?;
    for action in actions {
        nod.push(
            &action.encode_canonical_record(limits)?,
            limits.max_bounded_bytes,
        )?;
        // Same native record projection as Lysis finalizer::stream_result_chunks.
        // Scratch sorting replaces its external globally ordered record input.
        let record = ShuffleBucketRecordV1 {
            bucket_key: action.bucket_key(),
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
        tx.put::<InventoryRows>(key, record.encode_canonical_record(limits)?)?;
    }
    tx.commit()?;
    Ok(())
}

fn verify_certified_result_outputs(
    expected: &NodCertifiedGenerationProjection,
    (job, bundle): (&OcompJobRecordV1, &PinnedProtocolBundle),
    audit: &LocalLysisPlanAuditV1<'_>,
    (nod_root, bucket_root, output_root): (B256, B256, B256),
    (allocation, cost): (U256, U256),
) -> eyre::Result<()> {
    let tribute_count = audit.plan().tribute_count;
    ensure!(
        (
            expected.protocol_bundle_hash,
            expected.program_semantics_hash,
            expected.tribute_count,
            expected.nod_count,
            expected.bucket_count,
            expected.nod_root,
            expected.bucket_root,
            expected.output_manifest_root,
            expected.lysis_allocation_minor,
            expected.nod_amount_total,
        ) == (
            job.intent.protocol_bundle_hash,
            bundle.bundle().lysis_program_semantics_hash,
            tribute_count,
            tribute_count,
            tribute_count,
            nod_root,
            bucket_root,
            output_root,
            allocation,
            cost,
        ),
        "complete present result catalog differs from canonical certified outputs"
    );
    Ok(())
}
