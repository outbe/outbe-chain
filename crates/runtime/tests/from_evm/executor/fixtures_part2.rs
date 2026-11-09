#[allow(dead_code)] // retained for follow-up tests
pub(super) fn cache_db_from_storage(
    seed_storage: HashMapStorageProvider,
) -> CacheDB<EmptyDBTyped<ProviderError>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut addresses: Vec<Address> = seed_storage
        .storage
        .keys()
        .map(|(address, _)| *address)
        .chain(std::iter::once(
            outbe_primitives::addresses::STAKING_ADDRESS,
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
    let minimum = U256::from(1_000u64);
    vs.test_seed_authoritative_min_stake(minimum).unwrap();
    vs.register_validator(OWNER, validator, pk).unwrap();
    vs.test_seed_authoritative_bonded(validator, minimum)
        .unwrap();
    let staking = outbe_staking::contract::Staking::new(storage.clone());
    let total = staking.total_staked.read().unwrap();
    staking.total_staked.write(total + minimum).unwrap();
    storage
        .increase_balance(outbe_primitives::addresses::STAKING_ADDRESS, minimum)
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

pub(super) fn register_and_activate_native(
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
        .increase_balance(outbe_primitives::addresses::STAKING_ADDRESS, minimum)
        .unwrap();
    validators.mark_pending(validator).unwrap();
    validators.confirm_validator_ready(validator).unwrap();
    validators
        .activate_validator_via_boundary_for_test(validator)
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
        // `Vec<MissedProposerEvent>`. The view defaults to 0. The V2 contract
        // is an empty list. This fixture exercises the validation path only.
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
    )
    .expect("date step");
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(day, index, rate)
        .unwrap();
}

pub(super) fn empty_executor_inputs(parent_hash: B256) -> BlockExecutorInputs {
    BlockExecutorInputs {
        identity: BlockExecutionIdentity {
            block_extra_data: Bytes::new(),
            validate_execution_summary: false,
            block_hash: None,
            block_state_root: None,
            parent_hash,
        },
        system_plan: BlockSystemPlan {
            expected_begin_system_txs: Vec::new(),
            system_layout_error: None,
            proposer_evm_address: None,
            execute_outbe_block_hooks: true,
            prebuilt_phase1_tx: None,
            pending_tee_bootstrap: None,
        },
        parent_accounting: ParentAccountingInputs {
            accounted_parent_artifact_provider: None,
            parent_consensus_metadata: None,
            parent_artifact_hint: None,
        },
        dependencies: BlockExecutionDependencies {
            bridge: None,
            evm_signer: None,
            runtime_host: Arc::new(outbe_runtime::OutbeRuntime::new()),
        },
    }
}

pub(super) fn executor_inputs_from_ctx(
    ctx: &OutbeBlockExecutionCtx<'_>,
    evm_signer: Option<SharedOutbeEvmSigner>,
    validate_execution_summary: bool,
) -> BlockExecutorInputs {
    let expected_begin_system_txs = ctx.expected_begin_system_txs.clone();
    let system_layout_error = ctx.system_layout_error.clone();
    let parent_consensus_metadata = ctx.parent_consensus_metadata.clone();
    let proposer_evm_address = ctx.proposer_evm_address;
    let execute_outbe_block_hooks = ctx.execute_outbe_block_hooks;
    let prebuilt_phase1_tx = ctx.prebuilt_phase1_tx.clone();
    let parent_artifact_hint = ctx.parent_artifact_hint;
    BlockExecutorInputs {
        identity: BlockExecutionIdentity {
            block_extra_data: Bytes::new(),
            validate_execution_summary,
            block_hash: None,
            block_state_root: None,
            parent_hash: ctx.inner.parent_hash,
        },
        system_plan: BlockSystemPlan {
            expected_begin_system_txs,
            system_layout_error,
            proposer_evm_address,
            execute_outbe_block_hooks,
            prebuilt_phase1_tx,
            pending_tee_bootstrap: None,
        },
        parent_accounting: ParentAccountingInputs {
            accounted_parent_artifact_provider: None,
            parent_consensus_metadata,
            parent_artifact_hint,
        },
        dependencies: BlockExecutionDependencies {
            bridge: None,
            evm_signer,
            runtime_host: Arc::new(outbe_runtime::OutbeRuntime::new()),
        },
    }
}

pub(super) fn executor_inputs_with_bootstrap(
    ctx: &OutbeBlockExecutionCtx<'_>,
    evm_signer: Option<SharedOutbeEvmSigner>,
    validate_execution_summary: bool,
) -> BlockExecutorInputs {
    let mut inputs = executor_inputs_from_ctx(ctx, evm_signer, validate_execution_summary);
    inputs.system_plan.pending_tee_bootstrap = ctx.pending_tee_bootstrap.clone();
    inputs
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

/// Establish the UTC boundary baseline for cycle-tick tests.
pub(super) fn seed_cycle_tick_genesis(
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
    let trigger = outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32();
    cycle.last_executed_at.write(&trigger, genesis_ts + 60)?;
    Ok(())
}

/// Observations from the executed prefix, without deriving any expected result.
pub(super) struct CycleObservation {
    pub(super) visible_gas: u64,
    pub(super) internal_gas: u64,
    pub(super) receipt_index: usize,
}

pub(super) fn observe_cycle_tick(
    system_txs: Vec<Recovered<TransactionSigned>>,
    decode_message: &str,
    mut execute: impl FnMut(Recovered<TransactionSigned>) -> CycleObservation,
) -> Option<CycleObservation> {
    for tx in system_txs {
        let input = SystemTx::decode(tx.tx().input().as_ref()).expect(decode_message);
        let observation = execute(tx);
        if matches!(input, SystemTx::CycleTick) {
            return Some(observation);
        }
    }
    None
}

pub(super) struct ParentAccountingFixture {
    pub(super) parent_hash: B256,
    pub(super) metadata: CertifiedParentAccountingMetadata,
    pub(super) artifact: AccountedParentArtifact,
    pub(super) proposer: Address,
}

pub(super) fn parent_accounting_context(
    input: ParentAccountingFixture,
) -> OutbeBlockExecutionCtx<'static> {
    let mut execution = execution_ctx(Some(0), Bytes::new());
    execution.inner.parent_hash = input.parent_hash;
    execution.parent_consensus_metadata = Some(input.metadata);
    execution.parent_artifact_hint = Some(input.artifact);
    execution.proposer_evm_address = Some(input.proposer);
    execution
}

pub(super) struct BeginPrefixCheck<'a> {
    pub(super) execution_message: &'a str,
    pub(super) cumulative_receipts: bool,
}

pub(super) fn assert_begin_prefix_gas<E>(
    executor: &mut E,
    system_txs: Vec<Recovered<TransactionSigned>>,
    check: BeginPrefixCheck<'_>,
) -> u64
where
    E: alloy_evm::block::BlockExecutor<Transaction = TransactionSigned, Receipt = Receipt>,
    E::Evm: reth_ethereum::evm::primitives::Evm<Tx = revm::context::TxEnv>,
{
    let mut visible_system_gas_used = 0u64;
    for tx in system_txs {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_output = executor
            .execute_transaction(tx)
            .expect(check.execution_message);
        assert!(gas_output.tx_gas_used() <= signed_gas_limit);
        visible_system_gas_used += gas_output.tx_gas_used();
        if check.cumulative_receipts {
            assert_eq!(
                executor
                    .receipts()
                    .last()
                    .expect("system receipt should be present")
                    .cumulative_gas_used,
                visible_system_gas_used,
            );
        }
    }
    visible_system_gas_used
}
