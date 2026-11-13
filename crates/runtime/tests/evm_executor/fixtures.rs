use std::sync::Arc;

pub use super::fixtures_execution::*;
use super::*;

pub const CHAIN_ID: u64 = GRAMINE_DIRECT_DEV_CHAIN_ID;

pub(super) const TEST_BLOCK_TIMESTAMP_BASE: u64 = 1_700_000_000;

pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

pub(super) fn seed_cycle_genesis(storage: StorageHandle<'_>) {
    // Established-parent fixtures completed every day before the most recent closed day.
    let completed = outbe_system_common::time::timestamp_to_date_key(
        TEST_BLOCK_TIMESTAMP_BASE - 2 * outbe_system_common::time::SECONDS_PER_DAY,
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

/// Establish the UTC boundary baseline for cycle-tick tests.
pub(crate) fn seed_cycle_tick_genesis(
    storage: StorageHandle<'_>,
    genesis_ts: u64,
    _proposer: Address,
) -> outbe_primitives::error::Result<()> {
    // These tests supply their own genesis, not the default established-parent history.
    outbe_nod::NodContract::new(storage.clone())
        .call_latest_day
        .write(0)?;
    outbe_gem::GemContract::new(storage.clone())
        .call_latest_day
        .write(0)?;
    outbe_intex::IntexContract::new(storage.clone())
        .call_latest_day
        .write(0)?;
    outbe_credis::CredisContract::new(storage.clone())
        .call_latest_day
        .write(0)?;
    let cycle = outbe_cycle::schema::Cycle::new(storage);
    let trigger = outbe_cycle::triggers::TriggerId::ProcessEmissionDay.as_u32();
    cycle.last_executed_at.write(&trigger, genesis_ts + 60)?;
    Ok(())
}

/// Publishes `rate` as the COEN/840 VWAP of the UTC day before `block_ts`.
pub(crate) fn seed_previous_day_vwap(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    block_ts: u64,
    rate: U256,
) {
    let (_, index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
    let day = outbe_system_common::time::previous_date_key(
        outbe_system_common::time::timestamp_to_date_key(block_ts),
    )
    .expect("date step");
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, rate)
        .unwrap();
}

pub(super) fn numbered_test_address(prefix: u8, n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = prefix;
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

pub fn dummy_pubkey(seed: u8) -> [u8; 48] {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    pk
}

pub fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    test_chain_spec_with_genesis_timestamp(
        TEST_BLOCK_TIMESTAMP_BASE - 2 * outbe_system_common::time::SECONDS_PER_DAY,
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

pub(crate) fn cache_db_from_storage(
    seed_storage: HashMapStorageProvider,
) -> CacheDB<EmptyDBTyped<ProviderError>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut addresses: Vec<Address> = seed_storage
        .storage
        .keys()
        .map(|(address, _)| *address)
        .chain(std::iter::once(
            outbe_system_common::addresses::STAKING_ADDRESS,
        ))
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    for address in addresses {
        let balance = seed_storage.get_balance(address);
        db.insert_account_info(
            address,
            AccountInfo {
                balance,
                ..Default::default()
            },
        );
    }
    for ((address, slot), value) in seed_storage.storage {
        db.insert_account_storage(address, slot, value)
            .expect("seed storage insert should succeed");
    }
    db
}

/// Independent trie-root projection of the executed test bundle.
pub fn post_state_root(state: &revm::database::BundleState) -> B256 {
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    let sorted = HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
    let storages = sorted.storages;
    let accounts = sorted
        .accounts
        .into_iter()
        .filter_map(|(address, account)| {
            account.map(|account| {
                let storage = storages
                    .get(&address)
                    .map(|storage| storage.storage_slots.clone())
                    .unwrap_or_default();
                (address, (account, storage))
            })
        });
    state_root_prehashed(accounts)
}

pub(crate) fn signer_balance(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    addr: Address,
) -> U256 {
    state
        .basic(addr)
        .expect("signer account read should succeed")
        .map(|a| a.balance)
        .unwrap_or_default()
}

pub(crate) fn test_committee_snapshot(
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
        .increase_balance(outbe_system_common::addresses::STAKING_ADDRESS, stake)
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

pub(crate) fn seed_registered_active_validator(
    storage: StorageHandle,
    validator: Address,
    pk: &[u8; 48],
) {
    let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    vs.config_owner.write(OWNER).unwrap();
    vs.set_config_max_validators(128).unwrap();
    vs.config_epoch_length_blocks.write(60).unwrap();
    vs.config_is_initialized.write(true).unwrap();
    let minimum = U256::from(1_000u64);
    vs.test_seed_authoritative_min_stake(minimum).unwrap();
    vs.register_validator(OWNER, validator, pk).unwrap();
    vs.test_seed_authoritative_bonded(validator, minimum)
        .unwrap();
    let staking = outbe_staking::contract::Staking::new(storage.clone());
    let total = staking.total_staked.read().unwrap();
    staking.total_staked.write(total + minimum).unwrap();
    storage
        .increase_balance(outbe_system_common::addresses::STAKING_ADDRESS, minimum)
        .unwrap();
    vs.activate_validator_via_boundary_for_test(validator)
        .unwrap();
    seed_test_committee_snapshot(storage.clone(), &[(validator, *pk)]);
    // Seed COEN/840 pair + 1.0 rate so the begin-block NOD/GEM/INTEX call scans
    // read a registered pair instead of reverting "pair not registered".
    // 840 also goes on the reference currency list, matching genesis: the
    // scans read their ISO from there.
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        outbe_oracle::api::RateObservation {
            rate: U256::from(1_000_000u64),
            block_number: 0,
            timestamp: 0,
        },
    )
    .unwrap();
}

pub(crate) fn register_and_activate_native(
    validators: &mut outbe_validatorset::contract::ValidatorSet<'_>,
    validator: Address,
    consensus_key: &[u8; 48],
) {
    let minimum = U256::from(1_000u64);
    validators
        .test_seed_authoritative_min_stake(minimum)
        .unwrap();
    validators
        .register_validator(OWNER, validator, consensus_key)
        .unwrap();
    validators
        .test_seed_authoritative_bonded(validator, minimum)
        .unwrap();
    let staking = outbe_staking::contract::Staking::new(validators.storage.clone());
    let total = staking.total_staked.read().unwrap();
    staking.total_staked.write(total + minimum).unwrap();
    validators
        .storage
        .increase_balance(outbe_system_common::addresses::STAKING_ADDRESS, minimum)
        .unwrap();
    validators.mark_pending(validator).unwrap();
    validators.confirm_validator_ready(validator).unwrap();
    validators
        .activate_validator_via_boundary_for_test(validator)
        .unwrap();
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
    for address in [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        outbe_system_common::addresses::ORACLE_ADDRESS,
        outbe_system_common::addresses::METADOSIS_ADDRESS,
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        CYCLE_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code.clone()),
                ..Default::default()
            },
        );
    }
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
    for address in [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        outbe_system_common::addresses::ORACLE_ADDRESS,
        outbe_system_common::addresses::METADOSIS_ADDRESS,
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code.clone()),
                ..Default::default()
            },
        );
    }
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
        seed_storage.enable_scoped_mutation_frames(
            outbe_primitives::storage::MutationPurposeTag::CycleLifecycle,
            cycle_frames,
        );
    }
    StorageHandle::enter(&mut seed_storage, seed_extra);

    let marker_addresses = [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        outbe_system_common::addresses::ORACLE_ADDRESS,
        VOTE_ADDRESS,
        UPDATE_ADDRESS,
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        CYCLE_ADDRESS,
        outbe_system_common::addresses::DESIS_ADDRESS,
        outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        outbe_system_common::addresses::STAKING_ADDRESS,
        outbe_primitives::addresses::REWARDS_ADDRESS,
        outbe_system_common::addresses::AGENT_REWARD_ADDRESS,
        outbe_system_common::addresses::CCA_REGISTRY_ADDRESS,
        outbe_system_common::addresses::METADOSIS_ADDRESS,
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        outbe_system_common::addresses::HYPERLANE_CONTROLLER_ADDRESS,
        outbe_system_common::addresses::TRIBUTE_ADDRESS,
        NOD_ADDRESS,
        outbe_system_common::addresses::GEM_ADDRESS,
        outbe_system_common::addresses::GEM_FACTORY_ADDRESS,
        outbe_system_common::addresses::INTEX_ADDRESS,
        outbe_system_common::addresses::CREDIS_ADDRESS,
        // marker allowlist: in production, `0xef` bytecode preserves the
        // accounting-progress marker account across EIP-161. So its seeded
        // slot survives as live state here too. Otherwise an empty account's
        // storage reads back as zero.
        outbe_system_common::addresses::ACCOUNTING_PROGRESS_ADDRESS,
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
