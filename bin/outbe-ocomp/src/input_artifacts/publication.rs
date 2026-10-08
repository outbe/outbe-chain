use super::*;

pub(super) struct InputPublication {
    pub(super) protocol_bundle_hash: B256,
    pub(super) chunk_objects: Vec<VerifiedCasObject>,
    pub(super) chunk_references: Vec<InputChunkRefV1>,
    pub(super) ordinal: u32,
    pub(super) tribute_count: u32,
    pub(super) tribute_nominal_total: U256,
}

pub(super) fn publish_tribute_chunks(
    context: InputArtifactContext<'_>,
    identity: &InputArtifactIdentity,
    source: TributeInputStream<impl FnMut() -> Result<Option<Vec<u8>>, InputArtifactError>>,
    publication: &mut InputPublication,
) -> Result<(), InputArtifactError> {
    let TributeInputStream {
        expected_count: expected_tribute_count,
        next: mut next_tribute,
    } = source;
    let tribute_chunk_items =
        usize::try_from(PRIMARY_WORK_SHARD_SIZE).map_err(|_| InputArtifactError::CountOverflow)?;

    let mut tribute_chunk = Vec::with_capacity(tribute_chunk_items);
    while let Some(canonical) = next_tribute()? {
        let tribute = outbe_tribute::record::decode_canonical(&canonical)?.calculation_view()?;
        require(
            tribute.worldwide_day.value() == identity.wwd,
            "Tribute WWD matches input identity",
        )?;
        publication.tribute_count = publication
            .tribute_count
            .checked_add(1)
            .ok_or(InputArtifactError::CountOverflow)?;
        if publication.tribute_count > expected_tribute_count {
            return Err(InputArtifactError::TributeCountMismatch {
                expected: expected_tribute_count,
                actual: publication.tribute_count,
            });
        }
        publication.tribute_nominal_total = publication
            .tribute_nominal_total
            .checked_add(tribute.nominal_amount_minor)
            .ok_or(InputArtifactError::NominalTotalOverflow)?;
        tribute_chunk.push(outbe_ocomp_protocol::common::BoundedBytes(canonical));
        if tribute_chunk.len() == tribute_chunk_items {
            publish_chunk(
                context,
                AuthenticatedInputChunkV1 {
                    protocol_bundle_hash: publication.protocol_bundle_hash,
                    job_id: identity.job_id,
                    kind: InputChunkKind::Tribute,
                    ordinal: publication.ordinal,
                    canonical_records_or_openings: core::mem::take(&mut tribute_chunk),
                },
                publication,
            )?;
            publication.ordinal = publication
                .ordinal
                .checked_add(1)
                .ok_or(InputArtifactError::CountOverflow)?;
        }
    }
    if publication.tribute_count != expected_tribute_count {
        return Err(InputArtifactError::TributeCountMismatch {
            expected: expected_tribute_count,
            actual: publication.tribute_count,
        });
    }
    if !tribute_chunk.is_empty() {
        publish_chunk(
            context,
            AuthenticatedInputChunkV1 {
                protocol_bundle_hash: publication.protocol_bundle_hash,
                job_id: identity.job_id,
                kind: InputChunkKind::Tribute,
                ordinal: publication.ordinal,
                canonical_records_or_openings: tribute_chunk,
            },
            publication,
        )?;
        publication.ordinal = publication
            .ordinal
            .checked_add(1)
            .ok_or(InputArtifactError::CountOverflow)?;
    }
    Ok(())
}

pub(super) fn publish_opening_chunks(
    context: InputArtifactContext<'_>,
    identity: &InputArtifactIdentity,
    openings: &InputArtifactOpenings,
    publication: &mut InputPublication,
) -> Result<(), InputArtifactError> {
    let limits = &context.limits;
    let fidelity_openings = &openings.fidelity;
    let oracle_opening = &openings.oracle;
    for opening in fidelity_openings {
        require(
            opening.source_kind == OpeningSourceKind::Fidelity,
            "Fidelity opening source",
        )?;
        publish_chunk(
            context,
            AuthenticatedInputChunkV1 {
                protocol_bundle_hash: publication.protocol_bundle_hash,
                job_id: identity.job_id,
                kind: InputChunkKind::Fidelity,
                ordinal: publication.ordinal,
                canonical_records_or_openings: vec![outbe_ocomp_protocol::common::BoundedBytes(
                    opening.encode_canonical_record(limits)?,
                )],
            },
            publication,
        )?;
        publication.ordinal = publication
            .ordinal
            .checked_add(1)
            .ok_or(InputArtifactError::CountOverflow)?;
    }
    publish_chunk(
        context,
        AuthenticatedInputChunkV1 {
            protocol_bundle_hash: publication.protocol_bundle_hash,
            job_id: identity.job_id,
            kind: InputChunkKind::Oracle,
            ordinal: publication.ordinal,
            canonical_records_or_openings: vec![outbe_ocomp_protocol::common::BoundedBytes(
                oracle_opening.encode_canonical_record(limits)?,
            )],
        },
        publication,
    )?;

    Ok(())
}

pub(super) fn publication_manifest(
    context: InputArtifactContext<'_>,
    identity: &InputArtifactIdentity,
    openings: &InputArtifactOpenings,
    publication: &InputPublication,
) -> Result<InputManifestV1, InputArtifactError> {
    let InputArtifactContext {
        bundle,
        limits,
        list_limits,
        ..
    } = context;
    let limits = &limits;
    let fidelity_openings = &openings.fidelity;
    let oracle_opening = &openings.oracle;
    let input_chunk_count = u32::try_from(publication.chunk_references.len())
        .map_err(|_| InputArtifactError::CountOverflow)?;
    let input_chunk_list_root = streaming_input_chunk_reference_root(
        input_chunk_count,
        publication.chunk_references.iter().cloned().map(Ok),
        limits,
    )?;
    let fidelity_opening_root = authenticated_opening_root(
        OpeningSourceKind::Fidelity,
        fidelity_openings,
        bundle,
        limits,
        list_limits,
    )?;
    let oracle_opening_root = authenticated_opening_root(
        OpeningSourceKind::Oracle,
        std::slice::from_ref(oracle_opening),
        bundle,
        limits,
        list_limits,
    )?;

    let exact_encoded_bytes =
        publication
            .chunk_references
            .iter()
            .try_fold(0_u64, |total, reference| {
                total
                    .checked_add(reference.encoded_bytes)
                    .ok_or(InputArtifactError::ByteCountOverflow)
            })?;
    let exact_record_count =
        publication
            .chunk_references
            .iter()
            .try_fold(0_u32, |total, reference| {
                total
                    .checked_add(reference.record_count)
                    .ok_or(InputArtifactError::CountOverflow)
            })?;
    Ok(InputManifestV1 {
        protocol_bundle_hash: publication.protocol_bundle_hash,
        job_id: identity.job_id,
        attempt: identity.attempt,
        checkpoint: identity.checkpoint.clone(),
        wwd: identity.wwd,
        sealed_tribute_collection_key: identity.sealed_tribute_collection_key,
        sealed_tribute_collection_root: identity.sealed_tribute_collection_root,
        tribute_count: publication.tribute_count,
        tribute_nominal_total: publication.tribute_nominal_total,
        input_chunk_count,
        input_chunk_list_root,
        fidelity_opening_root,
        oracle_opening_root,
        exact_encoded_bytes,
        exact_record_count,
        body_codec_id: bundle.tribute_body_codec_id,
        opening_codec_registry_hash: bundle.opening_codec_registry_hash()?,
        compression: Compression::None,
    })
}

pub(super) fn seal_input_publication(
    context: InputArtifactContext<'_>,
    input_ref_catalog_root: impl AsRef<Path>,
    manifest: InputManifestV1,
    publication: InputPublication,
) -> Result<PublishedInputArtifacts, InputArtifactError> {
    let InputArtifactContext {
        cas,
        bundle,
        limits,
        list_limits,
    } = context;
    let limits = &limits;
    let manifest_bytes = manifest.encode_canonical(limits)?;
    let mut manifest_ref = cas.publish_bytes(&manifest_bytes)?;
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    let manifest_object = cas.read_verified(&manifest_ref)?;
    let verified = verify_input_artifact_set(
        &manifest_object,
        &publication.chunk_objects,
        bundle,
        limits,
        list_limits,
    )?;
    let mut input_ref_catalog = VerifiedInputChunkRefCatalog::open(
        input_ref_catalog_root,
        cas,
        &manifest_ref,
        *limits,
        list_limits,
    )?;
    for reference in &publication.chunk_references {
        let _ = input_ref_catalog.admit(reference)?;
    }
    let _ = input_ref_catalog.exact_cursor()?;

    Ok(PublishedInputArtifacts {
        manifest_ref,
        ordered_chunk_refs: publication
            .chunk_objects
            .iter()
            .map(VerifiedCasObject::reference)
            .collect(),
        manifest_hash: verified.manifest_hash,
        tribute_count: verified.tribute_count,
        tribute_nominal_total: verified.tribute_nominal_total,
    })
}

fn publish_chunk(
    context: InputArtifactContext<'_>,
    chunk: AuthenticatedInputChunkV1,
    publication: &mut InputPublication,
) -> Result<(), InputArtifactError> {
    let InputArtifactContext {
        cas,
        bundle,
        limits,
        ..
    } = context;
    let limits = &limits;
    let encoded = chunk.encode_canonical(limits)?;
    let mut object_ref = cas.publish_bytes(&encoded)?;
    object_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
    let object = cas.read_verified(&object_ref)?;
    publication
        .chunk_references
        .push(derive_input_chunk_ref(&object, bundle, limits)?.reference);
    publication.chunk_objects.push(object);
    Ok(())
}
