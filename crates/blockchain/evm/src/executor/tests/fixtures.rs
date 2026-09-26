use super::*;

pub(super) const CHAIN_ID: u64 = GRAMINE_DIRECT_DEV_CHAIN_ID;

pub(super) const TEST_BLOCK_TIMESTAMP_BASE: u64 = 1_700_000_000;

pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

pub(super) fn seed_compressed_entities_genesis(storage: StorageHandle<'_>) {
    let root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    storage
        .sstore(
            outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
            U256::ZERO,
            U256::from(4),
        )
        .unwrap();
    storage
        .sstore(
            outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(root.as_slice()),
        )
        .unwrap();
}

fn seed_cycle_genesis(storage: StorageHandle<'_>) {
    let cycle = storage.contract::<outbe_cycle::schema::Cycle<'_>>();
    cycle
        .active_utc_day
        .write(outbe_primitives::time::timestamp_to_date_key(
            TEST_BLOCK_TIMESTAMP_BASE,
        ))
        .unwrap();
}

pub(super) fn persistent_test_tree(
    genesis_hash: B256,
) -> (tempfile::TempDir, Arc<CompressedTreeService>) {
    persistent_test_tree_with_marker(
        genesis_hash,
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
        },
    )
}

pub(super) fn persistent_test_tree_with_marker(
    genesis_hash: B256,
    marker: FinalizedMarker,
) -> (tempfile::TempDir, Arc<CompressedTreeService>) {
    let directory = tempfile::tempdir().expect("CE test directory must be created");
    let genesis_marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
    };
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: CHAIN_ID,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
        genesis_marker,
    )
    .expect("CE test MDBX must open");
    if marker != genesis_marker {
        db.test_seed_finalized_marker(marker)
            .expect("CE test finalized marker must seed");
    }
    let service = CompressedTreeService::new(
        db,
        CandidateCacheLimits {
            max_candidates: 4,
            max_encoded_bytes: 1_000_000,
        },
    )
    .expect("CE test tree service must open");
    (directory, Arc::new(service))
}

pub(super) fn numbered_test_address(prefix: u8, n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = prefix;
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

pub(super) fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_policy_v1, tee_attestation_v1_extra_field,
    };

    let mut spec = MAINNET.as_ref().clone();
    spec.chain = CHAIN_ID.into();
    spec.genesis.config.chain_id = CHAIN_ID;
    let policy = gramine_direct_policy_v1(spec.chain().id(), spec.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_extra_field(&policy).expect("test TEE activation manifest is canonical"),
    );
    let spec: Arc<ChainSpec<OutbeHeader>> = spec.map_header(OutbeHeader::new).into();
    spec
}

pub(super) fn test_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
    founders: &[(Address, [u8; 48])],
) -> Arc<outbe_metadosis::config::OcompForkInstallV1> {
    Arc::new(
        outbe_metadosis::test_support::ForkInstallScenario::measurement_at(
            1,
            chain_spec.chain().id(),
            chain_spec.genesis_hash(),
        )
        .unwrap()
        .with_founder_validators(founders)
        .unwrap()
        .into_install(),
    )
}

pub(super) fn seed_test_ocomp_profile(
    provider: &mut HashMapStorageProvider,
    restore_block_number: u64,
    install: &outbe_metadosis::config::OcompForkInstallV1,
) {
    provider.set_block_number(1);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::ForkProfile,
    );
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, 1_700_000_001, CHAIN_ID),
            storage,
        );
        outbe_metadosis::commands::install_fork_profile(&ctx, install).unwrap();
    });
    provider.set_block_number(restore_block_number);
}

pub(super) fn test_evm_signer() -> Arc<OutbeEvmSigner> {
    Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).unwrap())
}

pub(super) fn test_evm_env(block_number: u64, beneficiary: Address) -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(block_number),
            gas_limit: outbe_primitives::system_tx::protocol_block_gas_limit(block_number),
            basefee: MIN_PROTOCOL_BASE_FEE,
            beneficiary,
            timestamp: U256::from(TEST_BLOCK_TIMESTAMP_BASE.saturating_add(block_number)),
            ..Default::default()
        },
    }
}

pub(super) fn state_with_active_proposer(
    proposer: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_fixture(proposer, true)
}

pub(super) fn state_with_active_proposer_without_ocomp(
    proposer: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_fixture(proposer, false)
}

fn state_with_active_proposer_fixture(
    proposer: Address,
    seed_ocomp: bool,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let proposer_key = dummy_pubkey(0xA2);
    let install = test_ocomp_fork_install(&chain_spec, &[(proposer, proposer_key)]);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_compressed_entities_genesis(storage.clone());
        seed_cycle_genesis(storage.clone());
        seed_registered_active_validator_with_registration(
            storage.clone(),
            proposer,
            &proposer_key,
            &install.founder_registrations[0],
        );
    });
    if seed_ocomp {
        seed_test_ocomp_profile(&mut seed_storage, 0, &install);
    }

    let mut db = cache_db_from_storage(seed_storage);
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        CYCLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

pub(super) fn state_with_active_proposer_and_funded_account(
    proposer: Address,
    funded: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_and_funded_account_fixture(proposer, funded, true)
}

pub(super) fn state_with_active_proposer_and_funded_account_fixture(
    proposer: Address,
    funded: Address,
    seed_ocomp: bool,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let proposer_key = dummy_pubkey(0xA2);
    let install = test_ocomp_fork_install(&chain_spec, &[(proposer, proposer_key)]);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_compressed_entities_genesis(storage.clone());
        seed_cycle_genesis(storage.clone());
        seed_registered_active_validator_with_registration(
            storage.clone(),
            proposer,
            &proposer_key,
            &install.founder_registrations[0],
        );
    });
    if seed_ocomp {
        seed_test_ocomp_profile(&mut seed_storage, 0, &install);
    }

    let mut db = cache_db_from_storage(seed_storage);
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    db.insert_account_info(
        funded,
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );

    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

pub(super) fn state_with_active_validators_seeded(
    validators: &[(Address, [u8; 48])],
    seed_extra: impl FnOnce(StorageHandle),
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_validators_seeded_at_block(validators, 0, seed_extra)
}

pub(super) fn state_with_active_validators_seeded_at_block(
    validators: &[(Address, [u8; 48])],
    block_number: u64,
    seed_extra: impl FnOnce(StorageHandle),
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_validators_seeded_at_block_with_cycle_frames(
        validators,
        block_number,
        0,
        seed_extra,
    )
}

pub(super) fn state_with_active_validators_seeded_at_block_with_cycle_frames(
    validators: &[(Address, [u8; 48])],
    block_number: u64,
    cycle_frames: u8,
    seed_extra: impl FnOnce(StorageHandle),
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let install = test_ocomp_fork_install(&chain_spec, validators);
    seed_storage.set_block_number(block_number);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_compressed_entities_genesis(storage.clone());
        seed_cycle_genesis(storage.clone());
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        for ((validator, pk), registration) in validators.iter().zip(&install.founder_registrations)
        {
            register_and_activate_with_ocomp_registration(&mut vs, *validator, pk, registration);
        }
        seed_test_committee_snapshot(storage.clone(), validators);
        // Seed the COEN/840 oracle pair + a 1.0 rate so begin-block
        // NOD/GEM/INTEX floor-price promotion resolves a live rate instead
        // of soft-skipping the scan. 840 is also pushed onto the reference
        // currency list, matching genesis: the Nod qualifier reads its ISO
        // from there, not from a hard-coded constant.
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(outbe_oracle::api::DAY_TYPE_ISO)
            .unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            U256::from(1_000_000u64),
            0,
            0,
        )
        .unwrap();
    });
    seed_test_ocomp_profile(&mut seed_storage, block_number, &install);
    if cycle_frames != 0 {
        seed_storage.enable_metadosis_mutation_frames(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
            cycle_frames,
        );
    }
    StorageHandle::enter(&mut seed_storage, seed_extra);

    let marker_addresses = [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        outbe_primitives::addresses::ORACLE_ADDRESS,
        VOTE_ADDRESS,
        UPDATE_ADDRESS,
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        CYCLE_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        outbe_primitives::addresses::STAKING_ADDRESS,
        outbe_primitives::addresses::REWARDS_ADDRESS,
        outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        outbe_primitives::addresses::OCOMP_REGISTRY_ADDRESS,
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        outbe_primitives::addresses::TRIBUTE_ADDRESS,
        NOD_ADDRESS,
        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
        // marker allowlist: the accounting-progress marker account
        // is preserved across EIP-161 by `0xef` bytecode in production, so its
        // seeded slot survives as live state here too (otherwise an empty
        // account's storage reads back as zero).
        outbe_primitives::addresses::ACCOUNTING_PROGRESS_ADDRESS,
    ];
    // `cache_db_from_storage` carries storage slots but not balances, and the
    // marker-info insert below overwrites `AccountInfo`. Capture any balance a
    // seed closure funded on a marker address first, then re-apply it so the
    // marker code AND the seeded balance both survive.
    let seeded_balances: Vec<U256> = marker_addresses
        .iter()
        .map(|address| seed_storage.get_balance(*address))
        .collect();
    let mut db = cache_db_from_storage(seed_storage);
    let marker_code = Bytecode::new_legacy([0xef].into());
    for (address, balance) in marker_addresses.into_iter().zip(seeded_balances) {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code.clone()),
                balance,
                ..Default::default()
            },
        );
    }

    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

pub(super) fn execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
) -> OutbeBlockExecutionCtx<'a> {
    OutbeBlockExecutionCtx {
        inner: EthBlockExecutionCtx {
            parent_hash: B256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data,
            tx_count_hint,
            slot_number: None,
        },
        timestamp_millis_part: 0,
        block_hash: None,
        block_state_root: None,
        expected_begin_system_txs: Vec::new(),
        expected_end_system_txs: Vec::new(),
        system_layout_error: None,
        parent_consensus_metadata: None,
        proposer_evm_address: None,
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    }
}

pub(super) fn block_one_execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
) -> OutbeBlockExecutionCtx<'a> {
    execution_ctx_with_tee_bootstrap(tx_count_hint, extra_data, sample_tee_bootstrap_payload(1))
}

pub(super) fn execution_ctx_with_tee_bootstrap<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
    tee_bootstrap: outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
) -> OutbeBlockExecutionCtx<'a> {
    let mut ctx = execution_ctx(tx_count_hint, extra_data);
    ctx.pending_tee_bootstrap = Some(tee_bootstrap);
    ctx
}

pub(super) fn begin_system_txs_for_test(
    config: &OutbeEvmConfig,
    block_number: u64,
    parent_hash: B256,
    extra_data: &Bytes,
    parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    proposer: Address,
) -> Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>> {
    let pending_tee_bootstrap =
        (block_number == 1).then(|| sample_tee_bootstrap_payload(block_number));
    begin_system_txs_for_test_with_bootstrap(
        config,
        block_number,
        parent_hash,
        extra_data,
        parent_consensus_metadata,
        proposer,
        pending_tee_bootstrap,
    )
}

pub(super) fn begin_system_txs_for_test_with_bootstrap(
    config: &OutbeEvmConfig,
    block_number: u64,
    parent_hash: B256,
    extra_data: &Bytes,
    parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    proposer: Address,
    pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
) -> Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>> {
    config
        .build_begin_system_txs(
            block_number,
            CHAIN_ID,
            outbe_primitives::system_tx::protocol_block_gas_limit(block_number),
            parent_hash,
            extra_data,
            parent_consensus_metadata,
            Some(proposer),
            None,
            pending_tee_bootstrap,
        )
        .expect("begin-zone system txs should build")
}

pub(super) fn sample_tee_bootstrap_payload(
    block_number: u64,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    sample_tee_bootstrap_payload_at(
        block_number,
        TEST_BLOCK_TIMESTAMP_BASE.saturating_add(block_number),
    )
}

pub(super) fn sample_tee_bootstrap_payload_at(
    block_number: u64,
    consensus_timestamp: u64,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::DevValidatorV1;

    let consensus_public = dummy_pubkey(0xA2);
    let snapshot = test_committee_snapshot(&[(test_evm_signer().address(), consensus_public)]);
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    let requested_valid_until = consensus_timestamp
        .checked_add(3_600)
        .expect("test lease timestamp fits u64");
    sample_tee_bootstrap_payload_for(
        block_number,
        committee_snapshot_hash,
        requested_valid_until,
        &[DevValidatorV1 {
            evm_secret: [1; 32],
            bls_minpk_public: consensus_public,
        }],
    )
}

pub(super) fn sample_tee_bootstrap_payload_for(
    block_number: u64,
    committee_snapshot_hash: B256,
    requested_valid_until: u64,
    validators: &[outbe_primitives::tee_test_utils::DevValidatorV1],
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::{gramine_direct_bootstrap_v2, gramine_direct_policy_v1};

    let policy = gramine_direct_policy_v1(CHAIN_ID, MAINNET.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    gramine_direct_bootstrap_v2(
        policy,
        committee_snapshot_hash,
        block_number,
        requested_valid_until,
        validators,
    )
    .expect("test GramineDirectDev OST3 payload is canonical")
}

#[allow(dead_code)] // retained for follow-up tests
pub(super) fn test_regular_tx() -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(Address::ZERO),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

pub(super) fn test_oracle_submit_vote_tx() -> reth_ethereum::TransactionSigned {
    test_oracle_submit_vote_tx_with_gas_limit(1_000_000)
}

pub(super) fn test_oracle_submit_vote_tx_with_gas_limit(
    gas_limit: u64,
) -> reth_ethereum::TransactionSigned {
    let input = outbe_oracle::precompile::IOracle::submitVoteCall {
        tuples: vec![outbe_oracle::precompile::IOracle::ExchangeRateTuple {
            base: outbe_oracle::api::COEN_ASSET,
            quote: outbe_oracle::api::currency_address(840),
            exchangeRate: U256::from(1_000_000u64),
            volume: U256::from(10_000_000_000u64),
        }],
    }
    .abi_encode();

    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(ORACLE_ADDRESS),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[allow(dead_code)] // retained for follow-up tests
pub(super) fn test_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata::default()
}

// `finish_rejects_execution_summary_mismatch` was removed.
// The previous test asserted mismatch via the `total_emission_limit`
// field, which has been dropped from `ExecutionSummaryArtifact` in
// wire format v0x04. The remaining `validator_fee_sum` field is
// verified by the broader `outbe_rewards::on_finalized_metadata`
// hook and the metadata-fingerprint guard in
// `outbe_rewards::runtime::check_and_record_metadata_fingerprint`.

pub(super) fn dummy_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

fn test_committee_snapshot(
    validators: &[(Address, [u8; 48])],
) -> outbe_validatorset::CommitteeSnapshot {
    outbe_validatorset::CommitteeSnapshot {
        committee: validators
            .iter()
            .map(
                |(address, consensus_pubkey)| outbe_validatorset::CommitteeEntry {
                    address: *address,
                    consensus_pubkey: *consensus_pubkey,
                },
            )
            .collect(),
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vec![0x42; 96],
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

pub(super) fn seed_test_committee_snapshot(
    storage: StorageHandle,
    validators: &[(Address, [u8; 48])],
) -> B256 {
    let snapshot = test_committee_snapshot(validators);
    outbe_validatorset::write_committee_snapshot(storage, 0, &snapshot)
        .expect("seed epoch-0 test committee snapshot")
        .0
}

pub(super) fn test_register_waiting(
    vs: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    pubkey: &[u8; 48],
) {
    vs.test_register_validator_without_pop(validator, pubkey)
        .unwrap();
}

fn test_register_active_with_stake(
    vs: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    pubkey: &[u8; 48],
    stake: U256,
    minimum: U256,
) {
    test_register_waiting(vs, validator, pubkey);
    assert!(stake >= minimum);
    vs.test_set_stake_projection(
        validator,
        outbe_validatorset::StakeProjection::new(stake, None),
    )
    .unwrap();
    vs.activate_validator_via_boundary_for_test(validator)
        .unwrap();
}

pub(super) fn test_register_active(
    vs: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    pubkey: &[u8; 48],
) {
    test_register_active_with_stake(vs, validator, pubkey, U256::from(1), U256::from(1));
}

#[allow(dead_code)] // retained for follow-up tests
pub(super) fn cache_db_from_storage(
    seed_storage: HashMapStorageProvider,
) -> CacheDB<EmptyDBTyped<ProviderError>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let entries: Vec<_> = seed_storage.storage.into_iter().collect();
    let mut addresses: Vec<Address> = entries.iter().map(|((address, _), _)| *address).collect();
    addresses.sort_unstable();
    addresses.dedup();
    for address in addresses {
        db.insert_account_info(address, AccountInfo::default());
    }
    for ((address, slot), value) in entries {
        db.insert_account_storage(address, slot, value)
            .expect("seed storage insert should succeed");
    }
    db
}

pub(super) fn seed_registered_active_validator(
    storage: StorageHandle,
    validator: Address,
    pk: &[u8; 48],
) {
    let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    vs.config_owner.write(OWNER).unwrap();
    vs.set_config_max_validators(128).unwrap();
    vs.config_epoch_length_blocks.write(60).unwrap();
    vs.config_is_initialized.write(true).unwrap();
    vs.register_validator(OWNER, validator, pk).unwrap();
    vs.activate_validator_via_boundary_for_test(validator)
        .unwrap();
    seed_test_committee_snapshot(storage.clone(), &[(validator, *pk)]);
    // Seed COEN/840 pair + 1.0 rate so begin-block NOD/GEM/INTEX promotion
    // reads a registered pair instead of reverting "pair not registered".
    // 840 also goes on the reference currency list, matching genesis: the
    // Nod qualifier reads its ISO from there.
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .reference_currencies
        .push(outbe_oracle::api::DAY_TYPE_ISO)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        U256::from(1_000_000u64),
        0,
        0,
    )
    .unwrap();
}

pub(super) fn register_and_activate_with_ocomp_registration(
    validators: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    consensus_key: &[u8; 48],
    registration: &outbe_ocomp_protocol::committee::OcompKeyRegistrationV1,
) {
    validators
        .register_validator(OWNER, validator, consensus_key)
        .unwrap();
    validators.mark_pending(validator).unwrap();
    let encoded = registration
        .encode_canonical(&outbe_metadosis::config::poc_schema_limits())
        .unwrap();
    validators
        .confirm_validator_ready(validator, &encoded)
        .unwrap();
    validators
        .activate_validator_via_boundary_for_test(validator)
        .unwrap();
}

fn seed_registered_active_validator_with_registration(
    storage: StorageHandle,
    validator: Address,
    consensus_key: &[u8; 48],
    registration: &outbe_ocomp_protocol::committee::OcompKeyRegistrationV1,
) {
    let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    validators.config_owner.write(OWNER).unwrap();
    validators.set_config_max_validators(128).unwrap();
    validators.config_epoch_length_blocks.write(60).unwrap();
    validators.config_is_initialized.write(true).unwrap();
    register_and_activate_with_ocomp_registration(
        &mut validators,
        validator,
        consensus_key,
        registration,
    );
    seed_test_committee_snapshot(storage.clone(), &[(validator, *consensus_key)]);
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .reference_currencies
        .push(outbe_oracle::api::DAY_TYPE_ISO)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        U256::from(1_000_000u64),
        0,
        0,
    )
    .unwrap();
}

pub(super) fn metadata_with(
    committee: Vec<Address>,
    signer_bitmap: Vec<u8>,
    missed_proposers: Vec<Address>,
) -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        ordered_committee: committee,
        signer_bitmap,
        // convert V1-shape `Vec<Address>` test fixture into V2
        // `Vec<MissedProposerEvent>` (view defaults to 0 - V2 contract is
        // empty list, this fixture exercises the validation path only).
        missed_proposers: missed_proposers
            .into_iter()
            .map(
                |validator| outbe_primitives::consensus_metadata::MissedProposerEvent {
                    view: 0,
                    validator,
                },
            )
            .collect(),
        ..CertifiedParentAccountingMetadata::default()
    }
}

pub(super) fn boundary_with(
    is_validator_set_change: bool,
    committee: Vec<(Address, [u8; 48])>,
) -> outbe_primitives::consensus::DkgBoundaryArtifact {
    boundary_with_epoch(0, is_validator_set_change, committee)
}

pub(super) fn boundary_with_epoch(
    epoch: u64,
    is_validator_set_change: bool,
    committee: Vec<(Address, [u8; 48])>,
) -> outbe_primitives::consensus::DkgBoundaryArtifact {
    let new_active_set: Vec<Address> = committee.iter().map(|(address, _)| *address).collect();
    let vrf_group_public_key_bytes = vec![0x42u8; 96];
    let snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: committee
            .into_iter()
            .map(
                |(address, consensus_pubkey)| outbe_validatorset::CommitteeEntry {
                    address,
                    consensus_pubkey,
                },
            )
            .collect(),
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.clone(),
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    };
    let active_set_hash = super::hash_boundary_active_set(&new_active_set);
    let committee_set_hash = outbe_validatorset::committee_set_hash_v2(epoch, &snapshot);
    let vrf_group_public_key = keccak256(&vrf_group_public_key_bytes);
    outbe_primitives::consensus::DkgBoundaryArtifact {
        epoch,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        target_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::from(vrf_group_public_key_bytes),
        committee_set_hash,
        is_validator_set_change,
        outcome: Bytes::new(),
        is_full_dkg: false,
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: outbe_primitives::consensus::ReshareResult {
            new_active_set,
            active_set_hash,
        },
    }
}

pub(super) fn signer_balance(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    addr: Address,
) -> U256 {
    state
        .basic(addr)
        .expect("signer account read should succeed")
        .map(|a| a.balance)
        .unwrap_or_default()
}

/// Publishes `rate` as the COEN/840 VWAP of the UTC day before `block_ts`.
pub(super) fn seed_previous_day_vwap(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    block_ts: u64,
    rate: U256,
) {
    let (_, index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
    let day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(block_ts),
    );
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, rate)
        .unwrap();
}
