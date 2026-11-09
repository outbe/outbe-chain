use std::sync::Arc;

use super::*;

pub const CHAIN_ID: u64 = GRAMINE_DIRECT_DEV_CHAIN_ID;

pub(super) const TEST_BLOCK_TIMESTAMP_BASE: u64 = 1_700_000_000;

pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

pub(super) fn seed_cycle_genesis(storage: StorageHandle<'_>) {
    // Established-parent fixtures completed every day before the most recent closed day.
    let completed = outbe_primitives::time::timestamp_to_date_key(
        TEST_BLOCK_TIMESTAMP_BASE - 2 * outbe_primitives::time::SECONDS_PER_DAY,
    );
    outbe_nod::NodContract::new(storage.clone())
        .call_latest_day
        .write(completed)
        .unwrap();
    outbe_gem::GemContract::new(storage.clone())
        .call_latest_day
        .write(completed)
        .unwrap();
    outbe_intex::IntexContract::new(storage.clone())
        .call_latest_day
        .write(completed)
        .unwrap();
    outbe_credis::CredisContract::new(storage)
        .call_latest_day
        .write(completed)
        .unwrap();
}

pub(super) fn numbered_test_address(prefix: u8, n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = prefix;
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

pub fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    test_chain_spec_with_genesis_timestamp(
        TEST_BLOCK_TIMESTAMP_BASE - 2 * outbe_primitives::time::SECONDS_PER_DAY,
    )
}

pub fn test_chain_spec_with_genesis_timestamp(
    genesis_timestamp: u64,
) -> Arc<ChainSpec<OutbeHeader>> {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_policy_v1, tee_attestation_v1_extra_field,
    };

    let mut spec = MAINNET.as_ref().clone();
    spec.chain = CHAIN_ID.into();
    spec.genesis.config.chain_id = CHAIN_ID;
    spec.genesis.timestamp = genesis_timestamp;
    let policy = gramine_direct_policy_v1(spec.chain().id(), spec.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_extra_field(&policy).expect("test TEE activation manifest is canonical"),
    );
    let spec: Arc<ChainSpec<OutbeHeader>> = spec.map_header(OutbeHeader::new).into();
    spec
}

pub fn test_evm_signer() -> Arc<OutbeEvmSigner> {
    Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).unwrap())
}

pub fn test_evm_env(block_number: u64, beneficiary: Address) -> EvmEnv {
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
    state_with_active_proposer_fixture(proposer)
}

fn state_with_active_proposer_fixture(
    proposer: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let proposer_key = dummy_pubkey(0xA2);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_cycle_genesis(storage.clone());
        seed_registered_active_validator(storage, proposer, &proposer_key);
    });

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
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
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

pub fn state_with_active_proposer_and_funded_account(
    proposer: Address,
    funded: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_and_funded_account_fixture(proposer, funded)
}

pub(super) fn state_with_active_proposer_and_funded_account_fixture(
    proposer: Address,
    funded: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let chain_spec = test_chain_spec();
    let mut seed_storage =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, chain_spec.genesis_hash());
    let proposer_key = dummy_pubkey(0xA2);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_cycle_genesis(storage.clone());
        seed_registered_active_validator(storage.clone(), proposer, &proposer_key);
        seed_valid_tee_nodes(storage, &[proposer]);
    });

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
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code.clone()),
            ..Default::default()
        },
    );
    db.insert_account_info(
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
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

pub fn state_with_active_validators_seeded(
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
    seed_storage.set_block_number(block_number);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_cycle_genesis(storage.clone());
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(outbe_validatorset::runtime::CONSENSUS_VALIDATOR_BOUND)
            .unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        for (validator, pk) in validators {
            register_and_activate_native(&mut vs, *validator, pk);
        }
        seed_test_committee_snapshot(storage.clone(), validators);
        // Seed the COEN/840 oracle pair + a 1.0 rate so the begin-block
        // NOD/GEM/INTEX call scans resolve a live rate instead of soft-skipping.
        // Also push 840 onto the reference currency list to match genesis. The
        // scans read their ISO from there, not from a hard-coded constant.
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            outbe_oracle::api::RateObservation {
                rate: U256::from(1_000_000u64),
                block_number: 0,
                timestamp: 0,
            },
        )
        .unwrap();
    });
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
        outbe_primitives::addresses::DESIS_ADDRESS,
        outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        outbe_primitives::addresses::STAKING_ADDRESS,
        outbe_primitives::addresses::REWARDS_ADDRESS,
        outbe_primitives::addresses::AGENT_REWARD_ADDRESS,
        outbe_primitives::addresses::CCA_REGISTRY_ADDRESS,
        outbe_primitives::addresses::METADOSIS_ADDRESS,
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS,
        outbe_primitives::addresses::TRIBUTE_ADDRESS,
        NOD_ADDRESS,
        outbe_primitives::addresses::GEM_ADDRESS,
        outbe_primitives::addresses::GEM_FACTORY_ADDRESS,
        outbe_primitives::addresses::INTEX_ADDRESS,
        outbe_primitives::addresses::CREDIS_ADDRESS,
        // marker allowlist: in production, `0xef` bytecode preserves the
        // accounting-progress marker account across EIP-161. So its seeded
        // slot survives as live state here too. Otherwise an empty account's
        // storage reads back as zero.
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

pub fn block_one_execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
) -> OutbeBlockExecutionCtx<'a> {
    execution_ctx_with_tee_bootstrap(tx_count_hint, extra_data, sample_tee_bootstrap_payload(1))
}

pub fn block_two_execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
    proposer: Address,
    parent_hash: B256,
) -> (
    OutbeBlockExecutionCtx<'a>,
    CertifiedParentAccountingMetadata,
) {
    let mut metadata = metadata_with(vec![proposer], vec![1], Vec::new());
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    let mut ctx = execution_ctx(tx_count_hint, extra_data);
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    ctx.parent_artifact_hint = Some(AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        timestamp: 0,
        state_root: Some(B256::repeat_byte(0x91)),
    });
    ctx.proposer_evm_address = Some(proposer);
    (ctx, metadata)
}

pub fn seed_valid_tee_nodes(storage: StorageHandle<'_>, addresses: &[Address]) {
    let registry = outbe_teeregistry::TeeRegistry::new(storage);
    for (index, validator) in addresses.iter().enumerate() {
        let node_hash = keccak256((index as u64).to_be_bytes());
        registry
            .validator_v1_node_hash
            .write(validator, node_hash)
            .unwrap();
        registry
            .v1_node_enclave_id
            .write(&node_hash, B256::with_last_byte(0x11))
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, B256::with_last_byte(0x12))
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::with_last_byte(0x13))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, u64::MAX)
            .unwrap();
    }
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

pub enum BootstrapFixture {
    StandardForBlock,
    Payload(Box<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>),
    Absent,
}

impl BootstrapFixture {
    pub(super) fn explicit(
        payload: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Self {
        match payload {
            Some(payload) => Self::Payload(Box::new(payload)),
            None => Self::Absent,
        }
    }
}

pub struct BeginBlockFixture<'a> {
    pub block_number: u64,
    pub parent_hash: B256,
    pub extra_data: &'a Bytes,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub proposer: Address,
    pub bootstrap: BootstrapFixture,
}

pub fn begin_system_txs_for_test(
    config: &OutbeEvmConfig,
    input: BeginBlockFixture<'_>,
) -> Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>> {
    let BeginBlockFixture {
        block_number,
        parent_hash,
        extra_data,
        parent_consensus_metadata,
        proposer,
        bootstrap,
    } = input;
    let pending_tee_bootstrap = match bootstrap {
        BootstrapFixture::StandardForBlock => {
            (block_number == 1).then(|| sample_tee_bootstrap_payload(block_number))
        }
        BootstrapFixture::Payload(payload) => Some(*payload),
        BootstrapFixture::Absent => None,
    };
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
// field. Wire format v0x04 dropped that field from
// `ExecutionSummaryArtifact`. The broader
// `outbe_rewards::finalized_metadata_hook::record_votes_and_fees` hook and the
// metadata-fingerprint guard in
// `outbe_rewards::runtime::check_and_record_metadata_fingerprint`
// verify the remaining `validator_fee_sum` field.

pub fn dummy_pubkey(seed: u8) -> [u8; 48] {
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
    vs.test_seed_authoritative_min_stake(minimum).unwrap();
    vs.test_set_stake_projection(
        validator,
        outbe_validatorset::StakeProjection::new(stake, None),
    )
    .unwrap();
    let staking = outbe_staking::contract::Staking::new(vs.storage.clone());
    let total = staking.total_staked.read().unwrap();
    staking.total_staked.write(total + stake).unwrap();
    vs.storage
        .increase_balance(outbe_primitives::addresses::STAKING_ADDRESS, stake)
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

