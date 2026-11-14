use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, StorageKey, StorageValue, B256, U256};
use outbe_evm::{
    config::{OutbeEvmConfig, OutbeNextBlockEnvAttributes},
    signer::OutbeEvmSigner,
};
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::block::BlockContext;
use outbe_primitives::consensus::ConsensusExecutionBridge;
use outbe_primitives::storage::{
    direct::DirectStorageProvider, hashmap::HashMapStorageProvider, StorageHandle,
};
use outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;
use outbe_primitives::OutbeHeader;
use reth_ethereum::chainspec::ChainSpec;
use reth_ethereum::{
    chainspec::MAINNET,
    primitives::{Account, Bytecode, Header, SealedHeader},
    trie::{
        updates::TrieUpdates, AccountProof, HashedPostState, HashedStorage, KeccakKeyHasher,
        MultiProof, MultiProofTargets, StorageMultiProof, StorageProof, TrieInput,
    },
};
use reth_evm::{execute::ProviderError, NextBlockEnvAttributes};
use reth_provider::{
    AccountReader, BlockHashReader, BytecodeReader, HashedPostStateProvider, ProviderResult,
    StateProofProvider, StateProvider, StateRootProvider, StorageRootProvider,
};
use reth_trie::test_utils::{state_root_prehashed, storage_root_prehashed};
use revm::{
    database::CacheDB,
    database_interface::EmptyDBTyped,
    state::{AccountInfo, Bytecode as RevmBytecode},
};

pub(super) const GENESIS_OWNER: Address = address!("0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf");
pub(super) const TEST_BLOCK_ONE_TIMESTAMP: u64 = 1_700_000_001;

#[derive(Debug, Default, Clone)]
pub(crate) struct DeterministicEmptyStateProvider;

impl AccountReader for DeterministicEmptyStateProvider {
    fn basic_account(&self, _address: &Address) -> ProviderResult<Option<Account>> {
        Ok(None)
    }
}

impl BlockHashReader for DeterministicEmptyStateProvider {
    fn block_hash(&self, _number: u64) -> ProviderResult<Option<B256>> {
        Ok(None)
    }

    fn canonical_hashes_range(&self, _start: u64, _end: u64) -> ProviderResult<Vec<B256>> {
        Ok(Vec::new())
    }
}

impl StateRootProvider for DeterministicEmptyStateProvider {
    fn state_root(&self, hashed_state: HashedPostState) -> ProviderResult<B256> {
        Ok(compute_state_root(hashed_state))
    }

    fn state_root_from_nodes(&self, _input: TrieInput) -> ProviderResult<B256> {
        Ok(B256::ZERO)
    }

    fn state_root_with_updates(
        &self,
        hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((compute_state_root(hashed_state), TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        _input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        Ok((B256::ZERO, TrieUpdates::default()))
    }
}

impl StorageRootProvider for DeterministicEmptyStateProvider {
    fn storage_root(
        &self,
        _address: Address,
        hashed_storage: HashedStorage,
    ) -> ProviderResult<B256> {
        let slots = hashed_storage.storage.into_iter().collect::<Vec<_>>();
        Ok(storage_root_prehashed(slots))
    }

    fn storage_proof(
        &self,
        _address: Address,
        slot: B256,
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Ok(StorageProof::new(slot))
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Ok(StorageMultiProof::empty())
    }
}

impl StateProofProvider for DeterministicEmptyStateProvider {
    fn proof(
        &self,
        _input: TrieInput,
        address: Address,
        _slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        Ok(AccountProof::new(address))
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        _targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        Ok(MultiProof::default())
    }

    fn multiproof_v2(
        &self,
        _targets: reth_trie::MultiProofTargetsV2,
        _requires_masks: bool,
    ) -> ProviderResult<reth_trie::DecodedMultiProofV2> {
        Ok(reth_trie::DecodedMultiProofV2::default())
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
        _mode: reth_trie::ExecutionWitnessMode,
    ) -> ProviderResult<Vec<Bytes>> {
        Ok(Vec::new())
    }
}

impl HashedPostStateProvider for DeterministicEmptyStateProvider {
    fn hashed_post_state(
        &self,
        bundle_state: &revm::database::BundleState,
    ) -> ProviderResult<HashedPostState> {
        Ok(HashedPostState::from_bundle_state::<KeccakKeyHasher>(
            bundle_state.state(),
        ))
    }
}

impl StateProvider for DeterministicEmptyStateProvider {
    fn storage(
        &self,
        _account: Address,
        _storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        Ok(None)
    }
}

impl BytecodeReader for DeterministicEmptyStateProvider {
    fn bytecode_by_hash(&self, _code_hash: &B256) -> ProviderResult<Option<Bytecode>> {
        Ok(None)
    }
}

pub(super) fn compute_state_root(hashed_state: HashedPostState) -> B256 {
    let sorted = hashed_state.into_sorted();
    let storages = sorted.storages;
    let accounts = sorted
        .accounts
        .into_iter()
        .filter_map(|(hashed_address, maybe_account)| {
            maybe_account.map(|account| {
                let storage = storages
                    .get(&hashed_address)
                    .map(|hashed_storage| hashed_storage.storage_slots.clone())
                    .unwrap_or_default();
                (hashed_address, (account, storage))
            })
        });
    state_root_prehashed(accounts)
}

pub(super) fn test_bridge() -> ConsensusExecutionBridge {
    ConsensusExecutionBridge::new()
}

pub(super) fn test_evm_signer() -> Arc<OutbeEvmSigner> {
    Arc::new(
        OutbeEvmSigner::from_hex(
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        )
        .expect("test EVM signer must be valid"),
    )
}

pub(super) fn genesis_dev_tee_bootstrap(
    committee_snapshot_hash: B256,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_bootstrap_v2, gramine_direct_policy_v1, DevValidatorV1,
    };

    let mut secret = [0_u8; 32];
    secret[31] = 1;
    let mut consensus_public = [0_u8; 48];
    consensus_public[0] = 1;
    let policy = gramine_direct_policy_v1(
        GRAMINE_DIRECT_DEV_CHAIN_ID,
        test_chain_spec().genesis_hash(),
    )
    .expect("test GramineDirectDev policy is canonical");
    gramine_direct_bootstrap_v2(
        policy,
        committee_snapshot_hash,
        1,
        TEST_BLOCK_ONE_TIMESTAMP + 3_600,
        &[DevValidatorV1 {
            evm_secret: secret,
            bls_minpk_public: consensus_public,
        }],
    )
    .expect("test GramineDirectDev OST3 payload is canonical")
}

pub(super) fn test_config(bridge: ConsensusExecutionBridge) -> OutbeEvmConfig {
    OutbeEvmConfig::new_with_bridge(
        test_chain_spec(),
        bridge,
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(test_evm_signer())
}

pub(super) fn test_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_policy_v1, tee_attestation_v1_extra_field,
    };

    // Outbe starts after the Merge; PREVRANDAO must be present at block 1.
    let mut spec = reth_chainspec::ChainSpecBuilder::from(&*MAINNET)
        .paris_activated()
        .build();
    spec.chain = GRAMINE_DIRECT_DEV_CHAIN_ID.into();
    spec.genesis.config.chain_id = GRAMINE_DIRECT_DEV_CHAIN_ID;
    spec.genesis.timestamp = TEST_BLOCK_ONE_TIMESTAMP - 1;
    let policy = gramine_direct_policy_v1(spec.chain().id(), spec.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_extra_field(&policy).expect("test TEE activation manifest is canonical"),
    );
    spec.map_header(OutbeHeader::new).into()
}

pub(super) fn test_parent() -> SealedHeader<OutbeHeader> {
    SealedHeader::seal_slow(OutbeHeader::new(Header::default()))
}

pub(super) fn test_parent_at(number: u64) -> SealedHeader<OutbeHeader> {
    SealedHeader::seal_slow(OutbeHeader::new(Header {
        number,
        ..Default::default()
    }))
}

pub(super) fn next_block_attrs(extra_data: Bytes) -> OutbeNextBlockEnvAttributes {
    OutbeNextBlockEnvAttributes {
        inner: NextBlockEnvAttributes {
            timestamp: TEST_BLOCK_ONE_TIMESTAMP,
            suggested_fee_recipient: REWARDS_ADDRESS,
            prev_randao: B256::ZERO,
            gas_limit: 30_000_000,
            parent_beacon_block_root: None,
            withdrawals: None,
            extra_data,
            slot_number: None,
        },
        timestamp_millis_part: 0,
        parent_consensus_metadata: None,
        proposer_evm_address: Some(GENESIS_OWNER),
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    }
}

pub(super) type TestDb = CacheDB<EmptyDBTyped<ProviderError>>;

pub(super) fn seed_active_validators(db: &mut TestDb, validators: &[Address]) {
    let chain_spec = test_chain_spec();
    let founders = validators
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, validator)| {
            let mut consensus_key = [0u8; 48];
            consensus_key[0] = idx as u8 + 1;
            (validator, consensus_key)
        })
        .collect::<Vec<_>>();
    let mut metadosis_genesis = HashMapStorageProvider::new_with_chain_identity(
        chain_spec.chain().id(),
        chain_spec.genesis_hash(),
    );
    metadosis_genesis.set_block_number(1);
    metadosis_genesis.enter(|storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(Address::ZERO).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        vs.config_is_initialized.write(true).unwrap();

        let minimum = U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        for (validator, consensus_key) in &founders {
            vs.register_validator(Address::ZERO, *validator, consensus_key)
                .unwrap();
            vs.test_seed_authoritative_bonded(*validator, minimum)
                .unwrap();
            let total = staking.total_staked.read().unwrap();
            staking.total_staked.write(total + minimum).unwrap();
            storage
                .increase_balance(outbe_system_common::addresses::STAKING_ADDRESS, minimum)
                .unwrap();
            vs.mark_pending(*validator).unwrap();
            vs.confirm_validator_ready(*validator).unwrap();
            vs.activate_validator_via_boundary_for_test(*validator)
                .unwrap();
        }
        // Seed the COEN/840 pair and a 1.0 rate so products priced in 840 resolve a live rate.
        outbe_oracle::test_support::publish_day_type_rate(&storage, U256::from(1_000_000u64), 0, 0)
            .unwrap();
    });
    db.insert_account_info(
        outbe_system_common::addresses::STAKING_ADDRESS,
        AccountInfo {
            balance: metadosis_genesis.get_balance(outbe_system_common::addresses::STAKING_ADDRESS),
            ..Default::default()
        },
    );
    for ((address, slot), value) in metadosis_genesis.storage {
        db.insert_account_storage(address, slot, value)
            .expect("production-route genesis seed must install");
    }

    let marker_code = RevmBytecode::new_legacy([0xef].into());
    for address in [
        outbe_primitives::addresses::VALIDATOR_SET_ADDRESS,
        outbe_system_common::addresses::ORACLE_ADDRESS,
        outbe_system_common::addresses::METADOSIS_ADDRESS,
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
    outbe_evm::test_support::install_call_scan_markers(
        db,
        &[
            outbe_system_common::addresses::NOD_ADDRESS,
            outbe_system_common::addresses::GEM_ADDRESS,
            outbe_system_common::addresses::GEM_FACTORY_ADDRESS,
            outbe_system_common::addresses::INTEX_ADDRESS,
            outbe_system_common::addresses::CREDIS_ADDRESS,
        ],
        &marker_code,
    );
}

pub(super) fn assert_seeded_validators(db: &mut TestDb, validators: &[Address]) {
    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 0,
        timestamp: 0,
        chain_id: MAINNET.chain().id(),
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer: GENESIS_OWNER,
        validators: validators.to_vec(),
    };
    let mut provider = DirectStorageProvider::new(db, ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        for validator in validators {
            let record = vs
                .get_validator(*validator)
                .unwrap()
                .expect("seeded validator must exist");
            assert_eq!(record.status, outbe_validatorset::logic::status::ACTIVE);
            assert!(record.has_bls_share);
        }
    });
}
