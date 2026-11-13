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
    rewards.day_voter_count.write(&prev_day, validator_count)?;
    rewards
        .day_total_votes
        .write(&prev_day, U256::from(validator_count))?;
    for index in 0..validator_count {
        let voter = numbered_test_address(0x12, u64::from(index));
        rewards
            .day_voter_at
            .get_nested(&prev_day)
            .write(&index, voter)?;
        rewards.day_votes.get_nested(&prev_day).write(&voter, 1)?;
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
        storage.balance(outbe_system_common::addresses::AGENT_REWARD_ADDRESS)?,
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

pub(crate) use outbe_system_common::time::{EmissionDate, SECONDS_PER_DAY};

pub(super) const GENESIS: u64 = 1_704_067_200;
pub(super) const POPULATION: usize = 256;
pub(super) const RELAY: Address = Address::repeat_byte(0xc7);
pub(super) type Calibration = (Vec<(String, u64)>, Vec<(u32, u64)>, Vec<Log>, Option<B256>);
pub(crate) type CalibrationState = State<CacheDB<EmptyDBTyped<ProviderError>>>;

pub(super) fn committee(proposer: Address) -> Vec<(Address, [u8; 48])> {
    (0..POPULATION)
        .map(|index| {
            let mut key = [0; 48];
            key[..2].copy_from_slice(&u16::try_from(index + 1).unwrap().to_be_bytes());
            let address = if index == 0 {
                proposer
            } else {
                numbered_test_address(0x91, index as u64)
            };
            (address, key)
        })
        .collect()
}

type PrefixScenario<'a> = (&'a str, &'a [(Address, [u8; 48])], (u64, u64));

pub(super) fn run_prefix(
    scenario: PrefixScenario<'_>,
    seed: impl FnOnce(StorageHandle<'_>),
    contracts: Vec<(Address, Bytecode)>,
    boundary: Option<outbe_primitives::consensus::DkgBoundaryArtifact>,
    verify_deferred: bool,
) -> Calibration {
    let (label, validators, clock) = scenario;
    run_prefix_checked(
        (label, validators, clock),
        seed,
        contracts,
        boundary,
        (verify_deferred, |_, _| {}),
    )
}

pub(super) fn run_prefix_checked(
    scenario: PrefixScenario<'_>,
    seed: impl FnOnce(StorageHandle<'_>),
    contracts: Vec<(Address, Bytecode)>,
    boundary: Option<outbe_primitives::consensus::DkgBoundaryArtifact>,
    verification: (bool, impl FnOnce(&mut CalibrationState, &[Log])),
) -> Calibration {
    let (label, validators, clock) = scenario;
    let (verify_deferred, verify_state) = verification;
    let (block, timestamp) = clock;
    outbe_cycle::runtime::take_trigger_gas_samples();
    outbe_cycle::runtime::take_cycle_gas_samples();
    let signer = test_evm_signer();
    let proposer = signer.address();
    let user_tx = test_regular_tx().try_into_recovered().unwrap();
    let mut state =
        state_with_active_validators_seeded_at_block(validators, block - 1, |storage| {
            if block > 1 {
                storage
                    .contract::<outbe_cycle::schema::Accounting<'_>>()
                    .last_accounted_block_number
                    .write(block - 2)
                    .unwrap();
                seed_expiring_tee_nodes(
                    storage.clone(),
                    &validators.iter().map(|v| v.0).collect::<Vec<_>>(),
                    u64::MAX,
                )
                .unwrap();
            }
            seed(storage);
        });
    let mut accounts = revm::state::EvmState::default();
    if verify_deferred {
        let mut funded: revm::state::Account = AccountInfo {
            balance: U256::from(10).pow(U256::from(24)),
            ..Default::default()
        }
        .into();
        funded.mark_touch();
        accounts.insert(user_tx.signer(), funded);
    }
    for (address, code) in contracts {
        let mut account: revm::state::Account = AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        }
        .into();
        account.mark_touch();
        accounts.insert(address, account);
    }
    revm::DatabaseCommit::commit(&mut state, accounts);
    let config = OutbeEvmConfig::new(
        test_chain_spec_with_genesis_timestamp(GENESIS),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let mut env = test_evm_env(block, REWARDS_ADDRESS);
    env.block_env.timestamp = U256::from(timestamp);
    let parent_hash = B256::repeat_byte(0x91);
    let metadata = (block > 1).then(|| {
        let mut metadata = metadata_with(
            validators.iter().map(|v| v.0).collect(),
            vec![1; validators.len()],
            Vec::new(),
        );
        metadata.finalized_block_number = block - 1;
        metadata.finalized_block_hash = parent_hash;
        metadata
    });
    let mut execution = if let Some(metadata) = metadata.clone() {
        fixtures::parent_accounting_context(fixtures::ParentAccountingFixture {
            parent_hash,
            metadata,
            artifact: AccountedParentArtifact {
                summary: ExecutionSummaryArtifact {
                    validator_fee_sum: U256::ZERO,
                },
                timestamp: timestamp - 1,
                state_root: Some(B256::repeat_byte(0x92)),
            },
            proposer,
        })
    } else {
        execution_ctx_with_tee_bootstrap(
            Some(0),
            Bytes::new(),
            sample_tee_bootstrap_payload_at(1, timestamp),
        )
    };
    let extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: boundary.map(ConsensusHeaderArtifact::BoundaryOutcome),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .unwrap();
    let extra = if block == 1 {
        fixtures::block_one_artifacts(extra)
    } else {
        extra
    };
    execution.inner.extra_data = extra.clone();
    let evm = config.evm_with_env(&mut state, env);
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("calibration pre-execution")
    });
    let txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: block,
            parent_hash: if block == 1 { B256::ZERO } else { parent_hash },
            extra_data: &extra,
            parent_consensus_metadata: metadata,
            proposer,
            bootstrap: if block == 1 {
                BootstrapFixture::explicit(Some(sample_tee_bootstrap_payload_at(1, timestamp)))
            } else {
                BootstrapFixture::StandardForBlock
            },
        },
    );
    let mut phases = Vec::new();
    for tx in txs {
        let phase = SystemTx::decode(tx.tx().input().as_ref()).unwrap();
        let before = executor.system_tx_gross_gas;
        executor
            .execute_transaction(tx)
            .expect("calibration begin phase");
        assert!(
            executor.receipts().last().unwrap().success,
            "{label}: {}",
            outbe_runtime::BLOCK_ORDER.step_name_or_stage(&phase)
        );
        phases.push((
            outbe_runtime::BLOCK_ORDER
                .step_name_or_stage(&phase)
                .to_owned(),
            executor.system_tx_gross_gas - before,
        ));
    }
    let samples = outbe_cycle::runtime::take_trigger_gas_samples();
    let body = outbe_cycle::runtime::take_cycle_gas_samples();
    for (gross, handlers) in &body {
        assert!(gross >= handlers);
    }
    eprintln!(
        "CALIBRATION {label}: phases={phases:?}, triggers={samples:?}, body={body:?}, aggregate={}",
        executor.system_tx_gross_gas
    );
    assert!(
        executor.system_tx_gross_gas < outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
    );
    let logs: Vec<Log> = executor
        .receipts()
        .iter()
        .flat_map(|receipt| receipt.logs.clone())
        .collect();
    if verify_deferred {
        assert_deferred_trigger_logs(&logs, &samples, block, executor.system_tx_gross_gas);
        let receipts_before = executor.receipts().len();
        let gross_before = executor.system_tx_gross_gas;
        executor
            .execute_transaction(user_tx)
            .expect("user transaction after complete begin prefix");
        assert_eq!(executor.receipts().len(), receipts_before + 1);
        assert!(executor.receipts().last().unwrap().success);
        assert_eq!(executor.system_tx_gross_gas, gross_before);
    }
    drop(executor);
    verify_state(&mut state, &logs);
    let root = if verify_deferred {
        let read_ctx = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: block,
            timestamp,
            chain_id: CHAIN_ID,
            genesis_hash: B256::ZERO,
            genesis_timestamp: GENESIS,
            proposer,
            validators: vec![proposer],
        };
        Some(assert_deferred_state(&mut state, &logs, read_ctx))
    } else {
        None
    };
    (phases, samples, logs, root)
}

/// Models bridge handling: calldata hashing, a persistent per-message counter,
/// four state reads, a calldata log and a nested relay with its own state/log.
/// This is a supported synthetic contract profile, not production router bytecode.
pub(super) fn bridge_work(forward: bool) -> Vec<u8> {
    let mut code = vec![
        0x36, 0x60, 0, 0x60, 0, 0x37, // copy calldata
        0x36, 0x60, 0, 0x20, // hash the message
        0x80, 0x54, 0x60, 1, 0x01, 0x90, 0x55, // counter[hash] += 1
    ];
    for slot in 1..=4 {
        code.extend([0x60, slot, 0x54, 0x50]);
    }
    code.extend([0x36, 0x60, 0, 0xa0]); // log the full message
    if forward {
        code.extend([0x60, 0, 0x60, 0, 0x36, 0x60, 0, 0x60, 0, 0x73]);
        code.extend(RELAY.as_slice());
        code.extend([0x5a, 0xf1, 0x50]);
    }
    code.extend([0x60, 0x20, 0x60, 0, 0xf3]);
    code
}

/// ABI views are pure; state-changing selectors execute the bridge profile above.
pub(super) fn returning_contract(routes: Vec<([u8; 4], Vec<u8>)>) -> Bytecode {
    let bridge = bridge_work(true);
    let header = 6 + 11 * routes.len() + bridge.len();
    let mut code = vec![0x60, 0, 0x35, 0x60, 0xe0, 0x1c];
    for (index, (selector, _)) in routes.iter().enumerate() {
        code.extend([0x80, 0x63]);
        code.extend(selector);
        code.push(0x14);
        code.push(0x61);
        code.extend(u16::try_from(header + 16 * index).unwrap().to_be_bytes());
        code.push(0x57);
    }
    code.extend(bridge);
    let mut data_offset = header + 16 * routes.len();
    for (_, data) in &routes {
        let len = u16::try_from(data.len()).unwrap();
        code.extend([0x5b, 0x61]);
        code.extend(len.to_be_bytes());
        code.push(0x61);
        code.extend(u16::try_from(data_offset).unwrap().to_be_bytes());
        code.extend([0x60, 0, 0x39, 0x61]);
        code.extend(len.to_be_bytes());
        code.extend([0x60, 0, 0xf3]);
        data_offset += data.len();
    }
    for (_, data) in routes {
        code.extend(data);
    }
    Bytecode::new_legacy(code.into())
}

pub(super) mod router_abi {
    alloy_sol_types::sol!("../../contracts/intex/src/origin/interfaces/IOriginRouter.sol");
}

pub(super) fn accept_auction_batches(
    storage: StorageHandle<'_>,
    day: EmissionDate,
    chain: u32,
    batches: u16,
) {
    for batch in 0..batches {
        let indices = (0..64u32).map(|offset| u32::from(batch) * 64 + offset);
        let call = outbe_desis::precompile::IDesis::processBidsBatchCall {
            emissionDay: day.value(),
            srcChainId: chain,
            batchIndex: batch,
            totalBatches: batches,
            bidderAddresses: indices
                .clone()
                .map(|index| {
                    numbered_test_address(0xa3, u64::from(chain) * 16384 + u64::from(index + 1))
                })
                .collect(),
            packedBids: indices
                .map(|index| {
                    U256::from_limbs([
                        840 | (840 << 16) | (u64::from(index) << 32),
                        2_000_000 | (1 << 32),
                        0,
                        0,
                    ])
                })
                .collect(),
        };
        outbe_desis::precompile::dispatch(
            storage.clone(),
            &call.abi_encode(),
            outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
        )
        .unwrap();
    }
    let done = outbe_desis::precompile::IDesis::processBidsDoneCall {
        emissionDay: day.value(),
        srcChainId: chain,
        totalBatches: batches,
        totalBids: u32::from(batches) * 64,
    };
    outbe_desis::precompile::dispatch(
        storage.clone(),
        &done.abi_encode(),
        outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
    )
    .unwrap();
    let auction = outbe_desis::DesisContract::new(storage);
    let key = outbe_desis::DesisContract::chain_key(day, chain);
    assert_eq!(
        auction.chain_bid_count.read(&key).unwrap(),
        u32::from(batches) * 64
    );
    assert_eq!(auction.chain_done.read(&key).unwrap(), 1);
}

pub(crate) fn queue_accepted_cleanup(storage: StorageHandle<'_>, chains: u32) {
    let day = EmissionDate::new(20240101);
    let auction = outbe_desis::DesisContract::new(storage.clone());
    auction
        .auction_stage
        .write(&day, outbe_desis::AuctionStage::CollectingBids as u8)
        .unwrap();
    for chain in 1..=chains {
        accept_auction_batches(
            storage.clone(),
            day,
            chain,
            if chains == 1 { 17 } else { 1 },
        );
    }
    auction
        .auction_stage
        .write(&day, outbe_desis::AuctionStage::Completed as u8)
        .unwrap();
    auction.queue_intake_cleanup(day).unwrap();
}

fn assert_deferred_trigger_logs(
    logs: &[Log],
    samples: &[(u32, u64)],
    block: u64,
    system_gross_gas: u64,
) {
    let over: Vec<_> = logs
        .iter()
        .filter_map(|log| outbe_cycle::ICycle::CycleTriggerOverBudget::decode_log(log).ok())
        .collect();
    assert_eq!(over.len(), 1);
    let over = &over[0].data;
    assert_eq!(over.id, 4);
    assert_eq!(over.blockNumber, block);
    let budget = outbe_cycle::triggers::ACTIVE_TRIGGERS
        .iter()
        .find(|spec| spec.id == 4)
        .unwrap()
        .work_budget_gas();
    assert_eq!(over.budgetGas, budget);
    assert_eq!(
        samples.iter().find(|(id, _)| *id == 4).unwrap().1,
        over.consumedGas
    );
    assert!(over.consumedGas > budget / 2 && over.consumedGas <= budget);
    assert!(system_gross_gas > over.consumedGas);
    assert!(!logs.iter().any(
        |log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log)
            .is_ok_and(|event| event.data.id == 4)
            || outbe_cycle::ICycle::CycleTriggerFailed::decode_log(log)
                .is_ok_and(|event| event.data.id == 4)
    ));
    for id in [5, 6] {
        assert!(logs.iter().any(
            |log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log)
                .is_ok_and(|event| event.data.id == id)
        ));
    }
}

fn assert_deferred_state(
    state: &mut CalibrationState,
    logs: &[Log],
    read_ctx: BlockContext,
) -> B256 {
    let block = read_ctx.block_number;
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
        assert_eq!(cycle.last_executed_at.read(&4).unwrap(), GENESIS + 60);
        assert_eq!(cycle.last_executed_block_number.read(&4).unwrap(), 0);
        for id in [5, 6] {
            let scheduled = logs
                .iter()
                .filter_map(|log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log).ok())
                .find(|event| event.data.id == id)
                .unwrap()
                .data
                .scheduledAt;
            assert_eq!(cycle.last_executed_at.read(&id).unwrap(), scheduled);
            assert_eq!(cycle.last_executed_block_number.read(&id).unwrap(), block);
        }
        let first_result = router_abi::IOriginRouter::sendAuctionResultCall {
            dstChainId: 1,
            emissionDay: 20240101,
            issuedUnits: 0,
            auctionClearingRate: 0,
            wonBidsCount: 0,
        }
        .abi_encode();
        let counter = U256::from_be_bytes(keccak256(first_result).0);
        for address in [outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS, RELAY] {
            assert_eq!(
                storage.sload(address, counter).unwrap(),
                U256::ZERO,
                "rolled-back child economic state must not survive"
            );
        }
        let auction = outbe_desis::DesisContract::new(storage);
        let day = EmissionDate::new(20240101);
        assert_eq!(
            auction.auction_stage.read(&day).unwrap(),
            outbe_desis::AuctionStage::CollectingBids as u8
        );
        assert_eq!(auction.day_bid_count.read(&day).unwrap(), 256);
        assert_eq!(auction.gate_active_count.read().unwrap(), 1);
        for chain in 1..=128 {
            assert_eq!(
                auction
                    .chain_bid_count
                    .read(&outbe_desis::DesisContract::chain_key(day, chain))
                    .unwrap(),
                2
            );
            for index in 0..2 {
                let key = outbe_desis::DesisContract::bid_key(day, chain, index);
                assert_eq!(
                    auction.bid_bidder.read(&key).unwrap(),
                    numbered_test_address(0xa1, u64::from(chain * 2 + index))
                );
                let packed = U256::from(1_000_000u64)
                    | (U256::from((840u64 << 48) | (1 << 32)) << 64)
                    | (U256::from(840u64) << 128);
                assert_eq!(auction.bid_packed.read(&key).unwrap(), packed);
            }
        }
    });
    state.merge_transitions(BundleRetention::Reverts);
    fixtures::post_state_root(&state.bundle_state)
}
