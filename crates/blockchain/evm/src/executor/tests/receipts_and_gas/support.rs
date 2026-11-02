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
        .write(&prev_day, U256::from(validator_count))?;
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

pub(super) fn assert_dense_agent_accrual_pending(
    storage: StorageHandle<'_>,
    prev_day: u32,
    address_count: u64,
) -> Result<(), outbe_primitives::error::PrecompileError> {
    let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
    assert_eq!(
        agent.get_all_waa_counts(prev_day.into())?.len(),
        address_count as usize,
        "GAS-05: pending WAA inputs must survive the validator-only slice"
    );
    assert_eq!(
        agent.get_all_sra_counts(prev_day.into())?.len(),
        address_count as usize,
        "GAS-05: pending SRA inputs must survive the validator-only slice"
    );

    for n in 0..address_count {
        let waa = numbered_test_address(0x10, n);
        let sra = numbered_test_address(0x11, n);
        let waa_claimable = agent.get_claimable_reward(waa)?;
        let sra_claimable = agent.get_claimable_reward(sra)?;
        assert_eq!(
            waa_claimable,
            U256::ZERO,
            "GAS-05: WAA must not receive a second accrual budget in the same block"
        );
        assert_eq!(
            sra_claimable,
            U256::ZERO,
            "GAS-05: SRA must not receive a second accrual budget in the same block"
        );
    }
    assert_eq!(
        storage.balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)?,
        U256::ZERO,
        "GAS-05: AgentReward holds no native COEN"
    );
    assert_eq!(
        agent.accrual_head.read()?,
        1,
        "GAS-05: only the validator job is terminal"
    );
    assert_eq!(
        agent.accrual_tail.read()?,
        4,
        "GAS-05: all four pool descriptors must be queued"
    );
    assert_eq!(agent.pending_jobs_by_day.read(&prev_day)?, 3);
    let waa_job = agent
        .accrual_jobs
        .get(1)?
        .expect("GAS-05: WAA job must remain queued");
    assert_eq!(
        waa_job.pool,
        outbe_agentreward::schema::RewardPool::Waa as u8
    );
    assert_eq!(u64::from(waa_job.recipient_count), address_count);
    assert_eq!(waa_job.total_weight, U256::from(address_count));
    assert_eq!(waa_job.next_index, 0);
    assert_eq!(waa_job.credited_minor, U256::ZERO);
    assert!(!waa_job.pool_minor.is_zero());
    let emission_day = outbe_emission::api::get_emission_day_record(storage.clone(), prev_day)?
        .expect("GAS-05: previous-day emission allocation must exist");
    assert!(
        !emission_day.metadosis_limit_minor().is_zero(),
        "GAS-05: previous-day emission allocation must be nonzero"
    );
    assert_eq!(
        emission_day.state,
        outbe_emission::schema::emission_day_state::PROCESSING,
        "GAS-05: monetary settlement must wait for the queued agent pools"
    );
    Ok(())
}

/// Every dense voter holds a nonzero validator claimable and no Gem.
pub(super) fn assert_dense_validator_settlement(
    storage: StorageHandle<'_>,
    validator_count: u32,
) -> outbe_primitives::error::Result<()> {
    let agent_reward = outbe_agentreward::AgentRewardContract::new(storage);
    for index in 0..validator_count {
        let voter = numbered_test_address(0x12, u64::from(index));
        let claimable = agent_reward
            .get_pool_claimable_reward(outbe_agentreward::schema::RewardPool::Validator, voter)?;
        assert!(
            !claimable.is_zero(),
            "GAS-05: dense validator {voter} received zero claimable reward"
        );
    }
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
        vote.proposal_bond(U256::from(1u64)).unwrap().state,
        BondState::Refunded
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
