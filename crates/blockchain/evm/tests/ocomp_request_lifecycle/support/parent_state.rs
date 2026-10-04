use super::super::*;

pub(in crate::lifecycle) struct PreparedParent {
    pub(in crate::lifecycle) _tree_directory: tempfile::TempDir,
    pub(in crate::lifecycle) tree_service: Arc<CompressedTreeService>,
    pub(in crate::lifecycle) parent: Arc<SealedHeader<OutbeHeader>>,
    pub(in crate::lifecycle) parent_storage: HashMap<(Address, U256), U256>,
    pub(in crate::lifecycle) wwd: WorldwideDay,
    pub(in crate::lifecycle) nominal: U256,
    pub(in crate::lifecycle) request_time: u64,
}

pub(in crate::lifecycle) fn prepare_parent(
    snapshot: &StoredCommitteeSnapshot,
    genesis_hash: B256,
    fork_install: &outbe_metadosis::config::OcompForkInstallV1,
) -> PreparedParent {
    let directory = tempfile::tempdir().unwrap();
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: CHAIN_ID,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    let tree_service = Arc::new(
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap(),
    );
    let marker = tree_service.finalized_marker().unwrap();
    let parent_tree = tree_service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: marker.commitment_scheme_version,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        })
        .unwrap();
    let scope = ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
    let mut seed = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, genesis_hash);
    seed.set_block_number(PARENT_HEIGHT);
    seed.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::ForkProfile);
    let wwd = WorldwideDay::new(2026_0710);
    let parent_time = wwd.start_timestamp();
    let request_time = parent_time
        + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
        + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
        + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
        + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let nominal = U256::from(1_000);
    let owner = address!("7300000000000000000000000000000000000073");
    let seal = StorageHandle::enter(&mut seed, |storage| {
        seed_ce_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let nod = NodContract::new(storage.clone());
        nod.ocomp_materialization_head_sequence.write(1).unwrap();
        nod.ocomp_materialization_tail_sequence.write(1).unwrap();

        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(VALIDATOR_OWNER).unwrap();
        validators.set_config_max_validators(128).unwrap();
        validators.config_epoch_length_blocks.write(60).unwrap();
        validators.config_is_initialized.write(true).unwrap();
        for (entry, registration) in snapshot
            .committee
            .iter()
            .zip(&fork_install.founder_registrations)
        {
            validators
                .register_validator(VALIDATOR_OWNER, entry.address, &entry.consensus_pubkey)
                .unwrap();
            validators.mark_pending(entry.address).unwrap();
            validators
                .confirm_validator_ready(
                    entry.address,
                    &registration.encode_canonical(&poc_schema_limits()).unwrap(),
                )
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(entry.address)
                .unwrap();
        }
        write_committee_snapshot(storage.clone(), FINALIZED_EPOCH, snapshot).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        let mut oracle_genesis = outbe_oracle::genesis::OracleGenesisConfig::default_config();
        oracle_genesis.initial_rates.push((
            outbe_oracle::api::COEN_ASSET,
            outbe_oracle::api::currency_address(840),
            U256::from(2_000_000_000_000_000_000u128),
        ));
        outbe_oracle::genesis::init_from_genesis(&mut oracle, &oracle_genesis).unwrap();

        let activation_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(PARENT_HEIGHT, parent_time, CHAIN_ID),
            storage.clone(),
        );
        outbe_metadosis::commands::install_fork_profile(&activation_ctx, fork_install).unwrap();

        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let lookback_end = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
        let offering_end = lookback_end + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
        FreshDevnetGenesisBuilder::new()
            .seed_active_worldwide_day(GenesisWorldwideDay {
                worldwide_day: wwd,
                status: outbe_metadosis::WwdStatus::Ready,
                day_type: WwdDayType::Green,
                forming_start,
                forming_end,
                lookback_end,
                offering_end,
                scheduled_process_time: request_time,
                metadosis_limit_minor: U256::from(100),
                previous_vwap: U256::ZERO,
                current_vwap: U256::from(2),
            })
            .apply(storage.clone())
            .unwrap();
        let mut tribute = TributeContract::new(storage.clone());
        tribute.unseal_day(wwd).unwrap();
        tribute
            .issue(
                &scope,
                &EmptyParent,
                &TributeData {
                    tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                    owner,
                    worldwide_day: wwd,
                    issuance_amount_minor: nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::from(2),
                },
            )
            .unwrap();
        tribute.seal_day(wwd).unwrap();

        let parent_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(PARENT_HEIGHT, parent_time, CHAIN_ID),
            storage.clone(),
        );
        outbe_rewards::runtime::ensure_genesis_anchor(&parent_ctx).unwrap();
        Cycle::new(storage.clone())
            .active_utc_day
            .write(outbe_primitives::time::timestamp_to_date_key(parent_time))
            .unwrap();
        outbe_cycle::runtime::dispatch_triggers(&parent_ctx, &scope, &EmptyParent).unwrap();
        // This focused fixture jumps directly from the seeded WWD start to its
        // processing time. Model the intervening canonical daily advances so
        // the request block exercises a contiguous settlement, not SkipMissed.
        Cycle::new(storage.clone())
            .active_utc_day
            .write(outbe_primitives::time::previous_date_key(
                outbe_primitives::time::timestamp_to_date_key(request_time),
            ))
            .unwrap();
        end_block(storage, &scope).unwrap()
    });

    let parent_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            r_sealed: seal.new_root,
        }),
    })
    .unwrap();
    let parent_state_root = state_root_prehashed(hashed_marker_state(&seed.storage));
    let parent = Arc::new(SealedHeader::seal_slow(OutbeHeader::new(Header {
        parent_hash: genesis_hash,
        beneficiary: REWARDS_ADDRESS,
        state_root: parent_state_root,
        number: PARENT_HEIGHT,
        gas_limit: BLOCK_GAS_LIMIT,
        timestamp: parent_time,
        base_fee_per_gas: Some(1_000_000_000),
        extra_data: parent_extra_data,
        ..Default::default()
    })));
    tree_service
        .publish_candidate(parent.hash(), seal.staged_tree_batch)
        .unwrap();
    tree_service
        .apply_finalized(PARENT_HEIGHT, parent.hash(), seal.new_root)
        .unwrap();

    PreparedParent {
        _tree_directory: directory,
        tree_service,
        parent,
        parent_storage: seed.storage,
        wwd,
        nominal,
        request_time,
    }
}

#[derive(Debug)]
pub(in crate::lifecycle) struct EmptyParent;

impl outbe_compressed_entities::ParentBodySource for EmptyParent {
    fn get(
        &self,
        _entity: outbe_compressed_entities::EntityRef,
    ) -> Result<
        Option<outbe_compressed_entities::StoredBody>,
        outbe_compressed_entities::ParentBodySourceError,
    > {
        Ok(None)
    }

    fn list(
        &self,
        _query: outbe_compressed_entities::QueryRef,
        _request: outbe_compressed_entities::IdPageRequest,
    ) -> Result<outbe_compressed_entities::IdPage, outbe_compressed_entities::ParentBodySourceError>
    {
        Ok(outbe_compressed_entities::IdPage {
            ids: Vec::new(),
            next_after: None,
        })
    }
}

pub(in crate::lifecycle) fn assert_provider_snapshot(
    provider: &TestProvider,
    parent_storage: &HashMap<(Address, U256), U256>,
    expected: &StoredCommitteeSnapshot,
    committee_set_hash: B256,
) {
    let mut database = StateProviderDatabase::new(provider);
    let mut mirror = HashMapStorageProvider::new(CHAIN_ID);
    for ((address, slot), expected_value) in parent_storage
        .iter()
        .filter(|((address, _), _)| *address == VALIDATOR_SET_ADDRESS)
    {
        let actual = database.storage(*address, *slot).unwrap();
        assert_eq!(actual, *expected_value);
        mirror.storage.insert((*address, *slot), actual);
    }
    StorageHandle::enter(&mut mirror, |storage| {
        assert_eq!(
            read_committee_snapshot(
                storage,
                committee_snapshot_key(FINALIZED_EPOCH, committee_set_hash),
            )
            .unwrap(),
            Some(expected.clone())
        );
    });

    let context = BlockContext::empty_for_tests(REQUEST_HEIGHT, 0, CHAIN_ID);
    let mut state = reth_revm::db::State::builder()
        .with_database(database)
        .with_bundle_update()
        .build();
    let mut direct = DirectStorageProvider::new(&mut state, context);
    let storage = StorageHandle::new(&mut direct);
    assert_eq!(
        read_committee_snapshot(
            storage,
            committee_snapshot_key(FINALIZED_EPOCH, committee_set_hash),
        )
        .unwrap(),
        Some(expected.clone())
    );
}

pub(in crate::lifecycle) fn assert_provider_activated_ocomp_inputs(
    provider: &TestProvider,
    wwd: WorldwideDay,
    expected_nominal: U256,
) {
    let database = StateProviderDatabase::new(provider);
    let context = BlockContext::empty_for_tests(REQUEST_HEIGHT, 0, CHAIN_ID);
    let mut state = reth_revm::db::State::builder()
        .with_database(database)
        .with_bundle_update()
        .build();
    let mut direct = DirectStorageProvider::new(&mut state, context);
    let storage = StorageHandle::new(&mut direct);
    let update = Update::new(storage.clone());
    assert_eq!(update.get_active_version().unwrap(), ProtocolVersion::ZERO);
    assert_eq!(update.get_active_version_height().unwrap(), 0);

    assert!(outbe_metadosis::api::has_active_ocomp_profile(storage.clone()).unwrap());
    let days = outbe_metadosis::api::worldwide_days(storage.clone()).unwrap();
    assert_eq!(days.len(), 1);
    let projection = outbe_metadosis::api::worldwide_day(storage.clone(), wwd)
        .unwrap()
        .unwrap();
    assert_eq!(projection.status, outbe_metadosis::WwdStatus::Ready);
    assert_eq!(
        projection.membership,
        outbe_metadosis::WwdMembership::Active
    );
    assert_eq!(projection.day_type, WwdDayType::Green);
    assert_eq!(projection.metadosis_limit_minor, U256::from(100));
    let totals = TributeContract::new(storage).get_day_totals(wwd).unwrap();
    assert_eq!(totals.tribute_count, 1);
    assert_eq!(totals.tribute_nominal_total_minor, expected_nominal);
}

pub(in crate::lifecycle) fn seed_ce_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}
