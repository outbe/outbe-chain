//! Shared fixtures for the receipts and gas tests.

use super::*;

pub(super) fn test_priority_fee_tx() -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: (MIN_PROTOCOL_BASE_FEE * 2) as u128,
        max_priority_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        to: TxKind::Call(Address::ZERO),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[derive(Clone, Copy)]
pub(super) struct CapacityVictim<'a> {
    pub(super) proposer: Address,
    pub(super) victim: WorldwideDay,
    pub(super) day_limit: U256,
    pub(super) scope: &'a ExecutionScope,
    pub(super) parent: &'a TributeRepositoryReader,
}
pub(super) fn seed_expiring_tee_nodes(
    storage: StorageHandle<'_>,
    addresses: &[Address],
    deadline: u64,
) -> eyre::Result<()> {
    let registry = outbe_teeregistry::TeeRegistry::new(storage);
    for (index, validator) in addresses.iter().enumerate() {
        let node_hash = keccak256((index as u64).to_be_bytes());
        registry
            .validator_v1_node_hash
            .write(validator, node_hash)?;
        registry
            .v1_node_enclave_id
            .write(&node_hash, B256::with_last_byte(0x11))?;
        registry
            .v1_node_binding_id
            .write(&node_hash, B256::with_last_byte(0x12))?;
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::with_last_byte(0x13))?;
        registry.v1_node_valid_until.write(&node_hash, deadline)?;
    }

    Ok(())
}

pub(super) fn seed_retained_capacity_days(
    storage: StorageHandle<'_>,
    proposer: Address,
    victim: WorldwideDay,
) -> eyre::Result<()> {
    let genesis_ctx = BlockRuntimeContext::new(
        BlockContext::new(0, 1_704_067_200, CHAIN_ID, proposer, vec![proposer]),
        storage.clone(),
    );
    outbe_rewards::runtime::ensure_genesis_anchor(&genesis_ctx)?;
    let mut tribute = TributeContract::new(storage.clone());
    tribute.initialize_fresh_ocomp_profile()?;
    let retained = (0..outbe_metadosis::constants::MAX_RETAINED_WWDS)
        .map(|offset| -> eyre::Result<_> {
            let days_before = outbe_metadosis::constants::MAX_RETAINED_WWDS - offset;
            Ok(WorldwideDay::from_timestamp(
                victim.start_timestamp() - u64::try_from(days_before)? * 86_400,
            ))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(
        storage.clone(),
        &retained,
    )?;
    for day in &retained {
        tribute.seal_day(*day)?;
    }

    Ok(())
}

pub(super) fn seed_waiting_capacity_victim(
    storage: StorageHandle<'_>,
    fixture: &CapacityVictim,
) -> eyre::Result<u64> {
    let CapacityVictim {
        proposer,
        victim,
        day_limit,
        scope: seed_scope,
        parent: tribute_parent,
    } = *fixture;
    let mut tribute = TributeContract::new(storage.clone());
    let victim_ctx = BlockRuntimeContext::new(
        BlockContext::new(
            1,
            victim.start_timestamp() + 2 * 3_600,
            CHAIN_ID,
            proposer,
            vec![proposer],
        ),
        storage.clone(),
    );
    outbe_metadosis::commands::apply_cycle_day_limit(&victim_ctx, day_limit)?;
    let victim_projection = outbe_metadosis::api::worldwide_day(storage.clone(), victim)?
        .ok_or_else(|| eyre::eyre!("missing fixture value"))?;
    tribute.unseal_day(victim)?;
    tribute.issue(
        seed_scope,
        tribute_parent,
        &TributeData {
            tribute_id: outbe_compressed_entities::derive_poseidon_entity_id(proposer, victim)?,
            owner: proposer,
            worldwide_day: victim,
            issuance_amount_minor: U256::from(1),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(1),
            reference_currency: 840,
            tribute_price_minor: U256::from(1),
            exclude_from_intex_issuance: false,
        },
    )?;
    for boundary in [
        victim_projection.forming_end,
        victim_projection.lookback_end,
        victim_projection.offering_end,
    ] {
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(1, boundary, CHAIN_ID, proposer, vec![proposer]),
            storage.clone(),
        );
        outbe_metadosis::commands::advance_active_worldwide_days(&ctx, seed_scope)?;
    }
    assert_eq!(
        outbe_metadosis::api::worldwide_day(storage.clone(), victim)
            .unwrap()
            .unwrap()
            .status,
        outbe_metadosis::api::WorldwideDayStatus::Waiting
    );
    Ok(victim_projection.scheduled_process_time)
}

pub(super) fn arm_capacity_cycle(
    storage: StorageHandle<'_>,
    fire_at: u64,
    protocol_cycle_period: u64,
) -> eyre::Result<()> {
    let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
    cycle
        .active_utc_day
        .write(outbe_primitives::time::timestamp_to_date_key(fire_at))?;
    for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
        cycle.last_executed_at.write(
            &spec.id,
            if spec.id == outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32() {
                fire_at - protocol_cycle_period
            } else {
                fire_at
            },
        )?;
    }

    Ok(())
}

pub(super) fn seed_dense_voter_participation(
    storage: StorageHandle<'_>,
    prev_day: u32,
    validator_count: u32,
) -> eyre::Result<()> {
    let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
    rewards
        .daily_voter_count
        .write(&prev_day, validator_count)?;
    rewards
        .daily_total_participation
        .write(&prev_day, u64::from(validator_count))?;
    for index in 0..validator_count {
        let voter = numbered_test_address(0x12, u64::from(index));
        rewards
            .daily_voter_at
            .get_nested(&prev_day)
            .write(&index, voter)?;
        rewards
            .daily_participation
            .get_nested(&prev_day)
            .write(&voter, 1)?;
    }

    Ok(())
}

pub(super) fn seed_dense_agent_recipients(
    storage: StorageHandle<'_>,
    prev_day: u32,
    address_count: u64,
) -> eyre::Result<()> {
    let mut agent = outbe_agentreward::AgentRewardContract::new(storage);
    for n in 0..address_count {
        let waa = numbered_test_address(0x10, n);
        let sra = numbered_test_address(0x11, n);
        agent.increment_waa_tribute(prev_day.into(), waa)?;
        agent.increment_sra_tribute(prev_day.into(), sra)?;
    }
    assert_eq!(
        agent.get_all_waa_counts(prev_day.into()).unwrap().len(),
        address_count as usize,
        "GAS-05 fixture must seed all dense WAA recipients"
    );
    assert_eq!(
        agent.get_all_sra_counts(prev_day.into()).unwrap().len(),
        address_count as usize,
        "GAS-05 fixture must seed all dense SRA recipients"
    );

    Ok(())
}

pub(super) fn assert_dense_agent_settlement(
    storage: StorageHandle<'_>,
    prev_day: u32,
    address_count: u64,
) -> Result<(), outbe_primitives::error::PrecompileError> {
    let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
    assert!(
        agent.get_all_waa_counts(prev_day.into())?.is_empty(),
        "GAS-05: dense WAA day index must be cleared after CycleTick settlement"
    );
    assert!(
        agent.get_all_sra_counts(prev_day.into())?.is_empty(),
        "GAS-05: dense SRA day index must be cleared after CycleTick settlement"
    );

    let mut claimable_total = U256::ZERO;
    for n in 0..address_count {
        let waa = numbered_test_address(0x10, n);
        let sra = numbered_test_address(0x11, n);
        let waa_claimable = agent.get_claimable_reward(waa)?;
        let sra_claimable = agent.get_claimable_reward(sra)?;
        assert!(
            !waa_claimable.is_zero(),
            "GAS-05: dense WAA recipient {waa} received zero claimable reward"
        );
        assert!(
            !sra_claimable.is_zero(),
            "GAS-05: dense SRA recipient {sra} received zero claimable reward"
        );
        claimable_total += waa_claimable + sra_claimable;
    }
    assert!(
        !claimable_total.is_zero(),
        "GAS-05: dense CycleTick must credit claimable AgentReward balances"
    );
    assert_eq!(
        storage.balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)?,
        claimable_total,
        "GAS-05: AgentReward backing balance must match dense claimable total"
    );
    Ok(())
}

pub(super) struct ApprovedFactoryExpected {
    pub(super) issuer: Address,
    pub(super) forced_surplus: U256,
    pub(super) token_id: B256,
    pub(super) token: Address,
}
pub(super) fn assert_approved_factory_state(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    block_context: BlockContext,
    expected: ApprovedFactoryExpected,
) -> eyre::Result<()> {
    let ApprovedFactoryExpected {
        issuer,
        forced_surplus,
        token_id: expected_token_id,
        token: expected_token,
    } = expected;
    let mut provider = super::DirectStorageProvider::new(state, block_context.clone());
    let storage = StorageHandle::new(&mut provider);
    let vote = Vote::new(storage.clone());
    let factory = StablecoinFactoryContract::new(storage.clone());
    assert_eq!(
        vote.proposals
            .get(U256::from(1u64))
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Approved
    );
    assert_eq!(
        vote.proposal_bond(U256::from(1u64)).unwrap().settlement,
        BondSettlement::Refunded
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
    assert_eq!(storage.balance(issuer).unwrap(), STABLECOIN_CREATE_BOND);
    assert_eq!(factory.token_count().unwrap(), U256::from(1u64));
    assert_eq!(
        factory.registered_token_id(expected_token).unwrap(),
        Some(expected_token_id)
    );
    assert_eq!(
        factory.token_id_of(expected_token).unwrap(),
        expected_token_id
    );
    assert!(!factory.reservations.exists(U256::from(1u64)).unwrap());

    Ok(())
}

pub(super) struct CapacityParent {
    pub(super) state: State<CacheDB<EmptyDBTyped<ProviderError>>>,
    pub(super) tree_directory: tempfile::TempDir,
    pub(super) tree_service: Arc<CompressedTreeService>,
    pub(super) seed_hash: B256,
    pub(super) seed_root: B256,
    pub(super) body_reader: StorageReaderHandle,
    pub(super) fire_at: u64,
}
pub(super) fn prepare_capacity_parent(proposer: Address) -> eyre::Result<CapacityParent> {
    let victim = WorldwideDay::new(2023_1101);
    let day_limit = U256::from(100);
    let (tree_directory, tree_service) = persistent_test_tree(B256::ZERO);
    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO)?;
    let parent_tree = tree_service.open_parent(ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 0,
        block_hash: B256::ZERO,
        root: empty_root,
    })?;
    let seed_scope =
        ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
    let body_storage = Arc::new(MemoryStorage::new());
    let body_reader: StorageReaderHandle = body_storage;
    let tribute_parent = TributeRepositoryReader::new(body_reader.clone());
    let mut seeded = None;
    let state = state_with_active_validators_seeded_at_block_with_cycle_frames(
        &[(proposer, dummy_pubkey(0xA3))],
        1,
        4,
        |storage| {
            seeded = Some(seed_capacity_storage(
                storage,
                &CapacityVictim {
                    proposer,
                    victim,
                    day_limit,
                    scope: &seed_scope,
                    parent: &tribute_parent,
                },
            ));
        },
    );
    let (fire_at, staged_tree_batch) =
        seeded.ok_or_else(|| eyre::eyre!("capacity storage must be seeded"))??;
    let seed_hash = B256::repeat_byte(0xA5);
    let seed_root = staged_tree_batch.new_root();
    tree_service.publish_candidate(seed_hash, staged_tree_batch)?;
    tree_service.apply_finalized(1, seed_hash, seed_root)?;

    Ok(CapacityParent {
        state,
        tree_directory,
        tree_service,
        seed_hash,
        seed_root,
        body_reader,
        fire_at,
    })
}
pub(super) fn seed_capacity_storage(
    storage: StorageHandle<'_>,
    fixture: &CapacityVictim<'_>,
) -> eyre::Result<(u64, outbe_compressed_entities::ProvisionalTreeBatch)> {
    let CapacityVictim {
        proposer,
        victim,
        day_limit,
        scope: seed_scope,
        parent: tribute_parent,
    } = *fixture;
    outbe_compressed_entities::begin_block(storage.clone(), seed_scope)?;
    seed_retained_capacity_days(storage.clone(), proposer, victim)?;
    let scheduled = seed_waiting_capacity_victim(
        storage.clone(),
        &CapacityVictim {
            proposer,
            victim,
            day_limit,
            scope: seed_scope,
            parent: tribute_parent,
        },
    )?;
    let tribute = TributeContract::new(storage.clone());
    tribute.day_totals.update(&outbe_tribute::DayTotals {
        worldwide_day: victim,
        initialized: true,
        tribute_count: u32::MAX,
        tribute_nominal_total_minor: U256::MAX,
        is_sealed: true,
    })?;
    tribute.total_supply.write(u64::from(u32::MAX))?;
    let protocol_cycle_period = 3_600;
    let fire_at = scheduled.div_ceil(protocol_cycle_period) * protocol_cycle_period;
    arm_capacity_cycle(storage.clone(), fire_at, protocol_cycle_period)?;
    let staged_tree_batch =
        outbe_compressed_entities::end_block(storage, seed_scope)?.staged_tree_batch;
    Ok((fire_at, staged_tree_batch))
}
