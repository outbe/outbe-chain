use super::*;

pub(super) struct ReplayAuthority<'a> {
    pub catalog: &'a VerifiedInputChunkRefCatalog,
    pub reader: &'a FilesystemCasReader,
    pub work_root: &'a std::path::Path,
    pub finalized: &'a VerifiedFinalizedIntentV1,
    pub expected: &'a ExpectedInputAuthorityV1,
    pub bundle: &'a ProtocolBundleV1,
    pub limits: &'a SchemaLimits,
}

pub(super) fn verify_replayed_finalized_inputs(
    authority: ReplayAuthority<'_>,
    on_progress: &impl Fn(),
) -> Result<(), RpcInputExporterErrorV1> {
    let inventory = replay_inventory(&authority, on_progress)?;
    let (oracle, reference_isos) = replay_oracle(&authority, &inventory, on_progress)?;
    replay_fidelity(
        &authority,
        &inventory,
        &oracle,
        &reference_isos,
        on_progress,
    )
}

fn replay_inventory(
    authority: &ReplayAuthority<'_>,
    on_progress: &impl Fn(),
) -> Result<SealedTributeInventory, RpcInputExporterErrorV1> {
    let ReplayAuthority {
        catalog,
        reader,
        work_root,
        expected,
        bundle,
        ..
    } = *authority;
    let subject = TributeInventorySubjectV1 {
        protocol_bundle_hash: expected.protocol_bundle_hash,
        job_id: expected.job_id,
        attempt: expected.attempt,
        checkpoint: expected.checkpoint.clone(),
        worldwide_day: WorldwideDay::new(expected.wwd),
        sealed_tribute_collection_root: expected.sealed_tribute_collection_root,
        expected_tribute_count: expected.tribute_count,
        expected_nominal_total: expected.tribute_nominal_total,
    };
    let inventory_root = work_root.join("inventory");
    let inventory = match crate::input_inventory::open_sealed_inventory_observing(
        &inventory_root,
        subject.clone(),
        on_progress,
    ) {
        Ok(inventory) => inventory,
        Err(TributeInventoryError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            let mut builder = TributeInventoryBuilder::create(
                &inventory_root,
                subject,
                TributeInventoryWorkConfig::default(),
            )
            .map_err(|error| stage("create replay Tribute inventory", error))?;
            for verified in catalog
                .exact_verified_cursor_observing(reader, bundle, on_progress)
                .map_err(|error| stage("open replay input catalog", error))?
            {
                let verified =
                    verified.map_err(|error| stage("read replay input catalog", error))?;
                on_progress();
                if verified.reference.kind != InputChunkKind::Tribute {
                    continue;
                }
                for canonical in verified.chunk.canonical_records_or_openings {
                    builder
                        .push(replay_inventory_record(canonical.0)?)
                        .map_err(|error| stage("spool replay Tribute inventory", error))?;
                    on_progress();
                }
            }
            builder
                .finish_observing(on_progress)
                .map_err(|error| stage("seal replay Tribute inventory", error))?
        }
        Err(error) => return Err(stage("reopen replay Tribute inventory", error)),
    };

    Ok(inventory)
}

fn replay_inventory_record(
    canonical: Vec<u8>,
) -> Result<TributeInventoryRecordV1, RpcInputExporterErrorV1> {
    let body = outbe_tribute::record::decode_canonical(&canonical)
        .map_err(|error| stage("decode replay Tribute", error))?;
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        body.stored_body()
            .map_err(|error| stage("encode replay Tribute", error))?
            .schema_version(),
        body.tribute_id,
        &canonical,
    )
    .map_err(|error| stage("commit replay Tribute", error))?;
    let amounts = body
        .calculation_view()
        .map_err(|error| stage("read replay Tribute amounts", error))?;
    Ok(TributeInventoryRecordV1 {
        tribute_id: body.tribute_id,
        commitment,
        owner: body.owner,
        reference_iso: body.reference_currency,
        nominal_amount_minor: amounts.nominal_amount_minor,
        canonical_body: canonical,
    })
}

fn replay_oracle(
    authority: &ReplayAuthority<'_>,
    inventory: &SealedTributeInventory,
    on_progress: &impl Fn(),
) -> Result<(AuthenticatedOpeningV1, Vec<u16>), RpcInputExporterErrorV1> {
    let ReplayAuthority {
        catalog,
        reader,
        expected,
        bundle,
        limits,
        ..
    } = *authority;
    let reference_isos = inventory.reference_isos();
    let mut oracle = None;
    for verified in catalog
        .exact_verified_cursor_observing(reader, bundle, on_progress)
        .map_err(|error| stage("open replay Oracle catalog", error))?
    {
        let verified = verified.map_err(|error| stage("read replay Oracle catalog", error))?;
        on_progress();
        if verified.reference.kind != InputChunkKind::Oracle {
            continue;
        }
        let canonical = verified
            .chunk
            .canonical_records_or_openings
            .into_iter()
            .next()
            .ok_or(RpcInputExporterErrorV1::Authority(
                "replayed Oracle opening",
            ))?;
        if oracle.is_some() {
            return Err(RpcInputExporterErrorV1::Authority(
                "replayed Oracle cardinality",
            ));
        }
        oracle = Some(
            AuthenticatedOpeningV1::decode_canonical_record(&canonical.0, limits)
                .map_err(|error| stage("decode replay Oracle opening", error))?,
        );
    }
    let oracle = oracle.ok_or(RpcInputExporterErrorV1::Authority(
        "replayed Oracle opening",
    ))?;
    let (oracle_wwd, oracle_isos) = decode_oracle_subject_key(&oracle.canonical_subject_key.0)
        .map_err(|error| stage("decode replay Oracle subject", error))?;
    if oracle_wwd != expected.wwd || oracle_isos != reference_isos {
        return Err(RpcInputExporterErrorV1::Authority(
            "replayed Oracle subject",
        ));
    }

    Ok((oracle, reference_isos))
}

fn replay_fidelity(
    authority: &ReplayAuthority<'_>,
    inventory: &SealedTributeInventory,
    oracle: &AuthenticatedOpeningV1,
    reference_isos: &[u16],
    on_progress: &impl Fn(),
) -> Result<(), RpcInputExporterErrorV1> {
    let ReplayAuthority {
        catalog,
        reader,
        finalized,
        bundle,
        limits,
        ..
    } = *authority;
    let mut owner_reader = inventory
        .owner_batches()
        .map_err(|error| stage("open replay owner inventory", error))?;
    let mut expected_owners = Vec::new();
    let mut expected_owner_index = 0_usize;
    for verified in catalog
        .exact_verified_cursor_observing(reader, bundle, on_progress)
        .map_err(|error| stage("open replay Fidelity catalog", error))?
    {
        let verified = verified.map_err(|error| stage("read replay Fidelity catalog", error))?;
        on_progress();
        if verified.reference.kind != InputChunkKind::Fidelity {
            continue;
        }
        let canonical = verified
            .chunk
            .canonical_records_or_openings
            .into_iter()
            .next()
            .ok_or(RpcInputExporterErrorV1::Authority(
                "replayed Fidelity opening",
            ))?;
        let fidelity = AuthenticatedOpeningV1::decode_canonical_record(&canonical.0, limits)
            .map_err(|error| stage("decode replay Fidelity opening", error))?;
        let owners = decode_fidelity_subject_key(&fidelity.canonical_subject_key.0)
            .map_err(|error| stage("decode replay Fidelity subject", error))?;
        for owner in &owners {
            if expected_owner_index == expected_owners.len() {
                expected_owners = owner_reader
                    .next_batch(outbe_ocomp_protocol::opening::MAX_FIDELITY_OWNERS_PER_OPENING)
                    .map_err(|error| stage("read replay owner inventory", error))?
                    .ok_or(RpcInputExporterErrorV1::Authority(
                        "replayed Fidelity owner overflow",
                    ))?;
                expected_owner_index = 0;
            }
            if expected_owners.get(expected_owner_index) != Some(owner) {
                return Err(RpcInputExporterErrorV1::Authority(
                    "replayed Fidelity owner coverage",
                ));
            }
            expected_owner_index += 1;
            on_progress();
        }
        verify_durable_lysis_openings(
            &fidelity,
            oracle,
            finalized,
            &OpeningSubjectsV1 {
                owners,
                reference_isos: reference_isos.to_vec(),
            },
            (bundle, limits),
        )
        .map_err(|error| stage("verify replayed finalized openings", error))?;
    }
    if expected_owner_index != expected_owners.len()
        || owner_reader
            .next_batch(outbe_ocomp_protocol::opening::MAX_FIDELITY_OWNERS_PER_OPENING)
            .map_err(|error| stage("close replay owner inventory", error))?
            .is_some()
    {
        return Err(RpcInputExporterErrorV1::Authority(
            "replayed Fidelity owner coverage",
        ));
    }
    Ok(())
}
