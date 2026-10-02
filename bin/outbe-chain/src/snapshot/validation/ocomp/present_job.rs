//! present job obligations for the offline OCOMP audit.
use super::*;

pub(super) fn verify_present_job(
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

pub(super) fn verify_complete_present_results<'a>(
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
