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
    verify_replayed_tributes(&authority, &inventory, on_progress)?;
    let (oracle, reference_isos) = replay_oracle(&authority, &inventory, on_progress)?;
    replay_fidelity(
        &authority,
        &inventory,
        &oracle,
        &reference_isos,
        on_progress,
    )
}

fn verify_replayed_tributes(
    authority: &ReplayAuthority<'_>,
    inventory: &SealedTributeInventory,
    on_progress: &impl Fn(),
) -> Result<(), RpcInputExporterErrorV1> {
    let sources = inventory
        .source_proofs()
        .map_err(|error| stage("open replay Tribute state witnesses", error))?;
    let mut previous = None;
    let mut count = 0_u32;
    for verified in authority
        .catalog
        .exact_verified_cursor_observing(authority.reader, authority.bundle, on_progress)
        .map_err(|error| stage("open replay Tribute catalog", error))?
    {
        let verified = verified.map_err(|error| stage("read replay Tribute catalog", error))?;
        if verified.reference.kind != InputChunkKind::Tribute {
            continue;
        }
        for canonical in verified.chunk.canonical_records_or_openings {
            let record = outbe_tribute::record::decode_canonical(&canonical.0)
                .map_err(|error| stage("decode replay Tribute source", error))?;
            let source = sources
                .record(record.tribute_id)
                .map_err(|error| stage("verify replay Tribute source", error))?;
            if previous.is_some_and(|id| id >= record.tribute_id)
                || record.encrypted() != Some(&source.tribute)
            {
                return Err(RpcInputExporterErrorV1::Authority(
                    "replayed Tribute state coverage",
                ));
            }
            count = count
                .checked_add(1)
                .ok_or(RpcInputExporterErrorV1::Authority(
                    "replayed Tribute count overflow",
                ))?;
            previous = Some(record.tribute_id);
            on_progress();
        }
    }
    if count != authority.expected.tribute_count {
        return Err(RpcInputExporterErrorV1::Authority("replayed Tribute count"));
    }
    Ok(())
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
        chain_id: authority.finalized.intent.chain_id,
        worldwide_day: WorldwideDay::new(expected.wwd),
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
            let sources = crate::input_inventory::StateTributeProofArchive::open(
                inventory_root.join(crate::input_inventory::SOURCE_PROOF_ARCHIVE_DIRECTORY),
                subject.state_source(),
                on_progress,
            )
            .map_err(|error| stage("reopen finalized state sources", error))?;
            let mut builder = TributeInventoryBuilder::create(
                &inventory_root,
                subject,
                TributeInventoryWorkConfig::default(),
                &sources,
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
    Ok(TributeInventoryRecordV1 {
        tribute_id: body.tribute_id,
        owner: body.owner,
        reference_iso: body.reference_currency,
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
