use super::*;

pub(super) struct VerifiedInputSummary {
    pub references: Vec<InputChunkRefV1>,
    pub exact_encoded_bytes: u64,
    pub exact_record_count: usize,
    pub tribute_count: usize,
    pub tribute_nominal_total: U256,
    pub tribute_owners: BTreeSet<Address>,
    pub tribute_isos: BTreeSet<u16>,
    pub fidelity_openings: Vec<AuthenticatedOpeningV1>,
    pub oracle_openings: Vec<AuthenticatedOpeningV1>,
}

pub(super) fn summarize_verified_chunks(
    manifest: &InputManifestV1,
    chunk_objects: &[VerifiedCasObject],
    bundle: &ProtocolBundleV1,
    limits: &SchemaLimits,
) -> Result<VerifiedInputSummary, InputArtifactError> {
    let mut references = Vec::new();
    references
        .try_reserve_exact(chunk_objects.len())
        .map_err(|_| InputArtifactError::Invariant("input chunk reference allocation"))?;
    let mut exact_encoded_bytes = 0_u64;
    let mut exact_record_count = 0_usize;
    let mut tribute_count = 0_usize;
    let mut tribute_nominal_total = U256::ZERO;
    let mut tribute_owners = BTreeSet::new();
    let mut tribute_isos = BTreeSet::from([840_u16]);
    let mut fidelity_openings = Vec::new();
    let mut oracle_openings = Vec::new();
    let mut previous_kind = None;

    for (index, object) in chunk_objects.iter().enumerate() {
        let derived = derive_input_chunk(object, bundle, limits)?;
        let expected_ordinal =
            u32::try_from(index).map_err(|_| InputArtifactError::CountOverflow)?;
        require(
            derived.ordinal == expected_ordinal,
            "input chunk ordinal sequence",
        )?;
        require(
            derived.protocol_bundle_hash == manifest.protocol_bundle_hash,
            "input chunk protocol bundle hash",
        )?;
        require(derived.job_id == manifest.job_id, "input chunk job id")?;
        if let Some(kind) = previous_kind {
            require(kind <= derived.kind, "input chunk kind order")?;
        }
        previous_kind = Some(derived.kind);

        exact_encoded_bytes = exact_encoded_bytes
            .checked_add(derived.public.reference.encoded_bytes)
            .ok_or(InputArtifactError::ByteCountOverflow)?;
        exact_record_count = exact_record_count
            .checked_add(derived.record_count)
            .ok_or(InputArtifactError::CountOverflow)?;
        tribute_count = tribute_count
            .checked_add(derived.tribute_count)
            .ok_or(InputArtifactError::CountOverflow)?;
        tribute_nominal_total = tribute_nominal_total
            .checked_add(derived.tribute_nominal_total)
            .ok_or(InputArtifactError::NominalTotalOverflow)?;
        tribute_owners.extend(derived.tribute_owners);
        tribute_isos.extend(derived.tribute_isos);
        fidelity_openings.extend(derived.fidelity_openings);
        oracle_openings.extend(derived.oracle_openings);
        references.push(derived.public.reference);
    }

    Ok(VerifiedInputSummary {
        references,
        exact_encoded_bytes,
        exact_record_count,
        tribute_count,
        tribute_nominal_total,
        tribute_owners,
        tribute_isos,
        fidelity_openings,
        oracle_openings,
    })
}

pub(super) fn verify_input_openings(
    manifest: &InputManifestV1,
    summary: VerifiedInputSummary,
    bundle: &ProtocolBundleV1,
    limits: &SchemaLimits,
    list_limits: OrderedListLimits,
) -> Result<(), InputArtifactError> {
    let VerifiedInputSummary {
        tribute_owners,
        tribute_isos,
        fidelity_openings,
        oracle_openings,
        ..
    } = summary;
    require(!fidelity_openings.is_empty(), "Fidelity opening set")?;
    let mut opened_owners = Vec::new();
    for opening in &fidelity_openings {
        opening.validate_against_bundle(bundle, limits)?;
        let _ = opening
            .decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
        opened_owners.extend(decode_fidelity_subject_key(
            &opening.canonical_subject_key.0,
        )?);
    }
    require(
        opened_owners == tribute_owners.into_iter().collect::<Vec<_>>(),
        "Fidelity subjects cover the exact Tribute owner set",
    )?;
    require(
        authenticated_opening_root(
            OpeningSourceKind::Fidelity,
            &fidelity_openings,
            bundle,
            limits,
            list_limits,
        )? == manifest.fidelity_opening_root,
        "Fidelity opening root",
    )?;

    require(oracle_openings.len() == 1, "exactly one Oracle opening")?;
    let oracle = &oracle_openings[0];
    oracle.validate_against_bundle(bundle, limits)?;
    let _ =
        oracle.decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
    let (oracle_wwd, oracle_isos) = decode_oracle_subject_key(&oracle.canonical_subject_key.0)?;
    require(oracle_wwd == manifest.wwd, "Oracle subject WWD")?;
    require(
        oracle_isos == tribute_isos.into_iter().collect::<Vec<_>>(),
        "Oracle subject covers the exact Tribute currency set",
    )?;
    require(
        authenticated_opening_root(
            OpeningSourceKind::Oracle,
            &oracle_openings,
            bundle,
            limits,
            list_limits,
        )? == manifest.oracle_opening_root,
        "Oracle opening root",
    )?;

    Ok(())
}
