// Supported-workload samples, not bounds on arbitrary governance-installed code or state.
use super::*;
use outbe_primitives::time::{EmissionDate, SECONDS_PER_DAY};

const GENESIS: u64 = 1_704_067_200;
const POPULATION: usize = 256;
const RELAY: Address = Address::repeat_byte(0xc7);
type Calibration = (Vec<(String, u64)>, Vec<(u32, u64)>, Vec<Log>, Option<B256>);
type CalibrationState = State<CacheDB<EmptyDBTyped<ProviderError>>>;

fn committee(proposer: Address) -> Vec<(Address, [u8; 48])> {
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

fn run_prefix(
    label: &str,
    validators: &[(Address, [u8; 48])],
    clock: (u64, u64),
    seed: impl FnOnce(StorageHandle<'_>),
    contracts: Vec<(Address, Bytecode)>,
    boundary: Option<outbe_primitives::consensus::DkgBoundaryArtifact>,
    verify_deferred: bool,
) -> Calibration {
    run_prefix_checked(
        label,
        validators,
        clock,
        seed,
        contracts,
        boundary,
        (verify_deferred, |_, _| {}),
    )
}

fn run_prefix_checked(
    label: &str,
    validators: &[(Address, [u8; 48])],
    clock: (u64, u64),
    seed: impl FnOnce(StorageHandle<'_>),
    contracts: Vec<(Address, Bytecode)>,
    boundary: Option<outbe_primitives::consensus::DkgBoundaryArtifact>,
    verification: (bool, impl FnOnce(&mut CalibrationState, &[Log])),
) -> Calibration {
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
    let config = OutbeEvmConfig::new(test_chain_spec_with_genesis_timestamp(GENESIS), std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
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
    execution.inner.extra_data = extra.clone();
    let evm = config.evm_with_env(&mut state, env);
    let mut executor = config.create_executor(evm, execution);
    super::super::with_phase1_verify_disabled(|| {
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
            phase.name()
        );
        phases.push((
            phase.name().to_owned(),
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
        assert!(executor.system_tx_gross_gas > over.consumedGas);
        assert!(!logs.iter().any(
            |log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log)
                .is_ok_and(|event| event.data.id == 4)
                || outbe_cycle::ICycle::CycleTriggerFailed::decode_log(log)
                    .is_ok_and(|event| event.data.id == 4)
        ));
        for id in [5, 6] {
            assert!(logs
                .iter()
                .any(
                    |log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log)
                        .is_ok_and(|event| event.data.id == id)
                ));
        }
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
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let cycle = outbe_cycle::schema::Cycle::new(storage.clone());
            assert_eq!(cycle.last_executed_at.read(&4).unwrap(), GENESIS + 60);
            assert_eq!(cycle.last_executed_block_number.read(&4).unwrap(), 0);
            for id in [5, 6] {
                let scheduled = logs
                    .iter()
                    .filter_map(|log| {
                        outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log).ok()
                    })
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
            for address in [outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS, RELAY] {
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
        Some(fixtures::post_state_root(&state.bundle_state))
    } else {
        None
    };
    (phases, samples, logs, root)
}

#[test]
fn calibration_supported_committee_and_unbonding_envelope() {
    let validators = committee(test_evm_signer().address());
    for full_exit in [false, true] {
        run_prefix(
            if full_exit {
                "256 validators x64 claims Oracle slash"
            } else {
                "256 active x63 claims TEE expiry"
            },
            &validators,
            (150, GENESIS + 120),
            |storage| {
                let mut staking = outbe_staking::contract::Staking::new(storage.clone());
                staking
                    .config_unbonding_period
                    .write(SECONDS_PER_DAY)
                    .unwrap();
                for (address, _) in &validators {
                    staking
                        .topup(*address, *address, U256::from(63_000))
                        .unwrap();
                    storage
                        .increase_balance(STAKING_ADDRESS, U256::from(63_000))
                        .unwrap();
                    for _ in 0..63 {
                        staking.unstake(*address, U256::from(1_000)).unwrap();
                    }
                    if full_exit {
                        staking.unstake(*address, U256::from(1_000)).unwrap();
                    }
                    assert_eq!(
                        staking.per_val_unbonding_len.read(address).unwrap(),
                        if full_exit { 64 } else { 63 }
                    );
                }
                if full_exit {
                    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                    oracle.config_is_initialized.write(true).unwrap();
                    oracle.config_slash_window.write(150).unwrap();
                    oracle
                        .config_min_valid_per_window
                        .write(outbe_oracle::schema::SCALE_1E18)
                        .unwrap();
                    oracle
                        .config_slash_fraction
                        .write(outbe_oracle::schema::SCALE_1E18 / U256::from(20))
                        .unwrap();
                    for (address, _) in &validators {
                        oracle.penalty_miss_count.write(address, 1).unwrap();
                    }
                } else {
                    seed_expiring_tee_nodes(
                        storage,
                        &validators.iter().map(|v| v.0).collect::<Vec<_>>(),
                        GENESIS + 120,
                    )
                    .unwrap();
                }
            },
            Vec::new(),
            None,
            false,
        );
    }
}

#[test]
fn calibration_supported_liveness_overlap() {
    let validators = committee(test_evm_signer().address());
    for active_count in [255, 256] {
        run_prefix(
            &format!("{active_count} active x256 newcomer domains"),
            &validators[..active_count],
            (150, GENESIS + 120),
            |storage| {
                let controller =
                    outbe_hyperlanecontroller::schema::HyperlaneControllerContract::new(storage);
                controller
                    .ica_router
                    .write(Address::repeat_byte(0x82))
                    .unwrap();
                for domain in 1..=256 {
                    controller.domains.push(domain).unwrap();
                }
            },
            Vec::new(),
            None,
            false,
        );
    }
}

/// Models bridge handling: calldata hashing, a persistent per-message counter,
/// four state reads, a calldata log and a nested relay with its own state/log.
/// This is a supported synthetic contract profile, not production router bytecode.
fn bridge_work(forward: bool) -> Vec<u8> {
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
fn returning_contract(routes: Vec<([u8; 4], Vec<u8>)>) -> Bytecode {
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

mod router_abi {
    alloy_sol_types::sol!("../../contracts/intex/src/origin/interfaces/IOriginRouter.sol");
}

#[test]
fn calibration_populated_auction_and_parked_handlers() {
    let (_, samples, logs, _) = populated_auction(16);
    assert_eq!(samples.len(), 8);
    assert!(logs.iter().any(|log| log.data.topics().first()
        == Some(&keccak256("IntexAuctionCompletedEmpty(uint32,uint64)"))));
    let resend = router_abi::IOriginRouter::resendParkedMessageCall::SELECTOR;
    for address in [outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS, RELAY] {
        assert_eq!(
            logs.iter()
                .filter(|log| log.address == address && log.data.data.starts_with(&resend))
                .count(),
            16
        );
    }
    assert!(logs.iter().any(
        |log| outbe_cycle::ICycle::CycleTriggerExecuted::decode_log(log)
            .is_ok_and(|event| event.data.id == 8)
    ));
}

#[test]
fn oversized_clearing_defers_atomically_and_deterministically_without_blocking_user() {
    let first = populated_auction(128);
    let second = populated_auction(128);
    assert_eq!(
        first, second,
        "same prestate must produce identical gross gas, logs and state root"
    );
    assert!(first.3.is_some());
}

#[test]
fn calibrated_canonical_envelope_fits_artifact_limit() {
    let handlers = outbe_cycle::triggers::ACTIVE_TRIGGERS
        .iter()
        .try_fold(0u64, |sum, spec| sum.checked_add(spec.work_budget_gas()))
        .unwrap();
    assert_eq!(outbe_cycle::triggers::ACTIVE_TRIGGERS.len(), 8);
    assert_eq!(handlers, 16_369_000);
    assert!(handlers < outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
}

fn populated_auction(chain_count: u32) -> Calibration {
    use router_abi::IOriginRouter;
    let targets: Vec<u32> = (1..=chain_count).collect();
    let router = returning_contract(vec![
        (
            IOriginRouter::targetsOfCall::SELECTOR,
            IOriginRouter::targetsOfCall::abi_encode_returns(&targets),
        ),
        (
            IOriginRouter::parkedMessageCountCall::SELECTOR,
            IOriginRouter::parkedMessageCountCall::abi_encode_returns(&U256::from(64)),
        ),
        (
            IOriginRouter::parkedProceedsCountCall::SELECTOR,
            IOriginRouter::parkedProceedsCountCall::abi_encode_returns(&U256::from(64)),
        ),
        (
            IOriginRouter::parkedMessageCall::SELECTOR,
            IOriginRouter::parkedMessageCall::abi_encode_returns(&IOriginRouter::ParkedMessage {
                dstChainId: 1,
                gasLimit: 100_000,
                sent: false,
                payload: vec![7; 256].into(),
            }),
        ),
        (
            IOriginRouter::parkedProceedsCall::SELECTOR,
            IOriginRouter::parkedProceedsCall::abi_encode_returns(&IOriginRouter::ParkedProceeds {
                emissionDay: 20240101,
                srcChainId: 1,
                amount: 1_000_000,
                settled: false,
            }),
        ),
    ]);
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    run_prefix(
        &format!(
            "populated auction{} bids/{chain_count} chains + parked64",
            chain_count * 2
        ),
        &[(proposer, dummy_pubkey(0xa2))],
        (if chain_count > 16 { 2 } else { 1 }, timestamp),
        |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            let day = EmissionDate::new(20240101);
            let auction = outbe_desis::DesisContract::new(storage.clone());
            auction
                .auction_stage
                .write(&day, outbe_desis::AuctionStage::CollectingBids as u8)
                .unwrap();
            auction
                .auction_at
                .write(&day, u32::try_from(timestamp).unwrap())
                .unwrap();
            auction.gate_active_count.write(1).unwrap();
            auction.gate_active_at.write(&0, day.value()).unwrap();
            auction.gate_active_slot.write(&day, 1).unwrap();
            auction.clearing_initiated.write(&day, 1).unwrap();
            auction
                .config_promis_load_minor
                .write(&day, U256::from(1_000_000))
                .unwrap();
            auction.config_min_bid_rate.write(&day, 2_000_000).unwrap();
            for chain in 1..=chain_count {
                let key = outbe_desis::DesisContract::chain_key(day, chain);
                auction.chain_done.write(&key, 1).unwrap();
                auction.chain_bid_count.write(&key, 2).unwrap();
                for index in 0..2 {
                    let key = outbe_desis::DesisContract::bid_key(day, chain, index);
                    auction
                        .bid_bidder
                        .write(
                            &key,
                            numbered_test_address(0xa1, u64::from(chain * 2 + index)),
                        )
                        .unwrap();
                    let packed = U256::from(1_000_000u64)
                        | (U256::from((840u64 << 48) | (1 << 32)) << 64)
                        | (U256::from(840u64) << 128);
                    auction.bid_packed.write(&key, packed).unwrap();
                }
            }
            auction.day_bid_count.write(&day, chain_count * 2).unwrap();
            outbe_desis::api::dispatch_auction_brief(
                storage.clone(),
                outbe_desis::api::AuctionBrief {
                    emission_day: EmissionDate::new(20240102),
                    desis_limit_minor: U256::ZERO,
                    has_limit: false,
                },
                outbe_desis::api::BriefOverflowPolicy::ReturnToPromisLimit,
            )
            .unwrap();
            seed_previous_day_vwap(&storage, timestamp, U256::from(1_000_000));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(20240101)
                .unwrap();
            for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
                outbe_cycle::schema::Cycle::new(storage.clone())
                    .last_executed_at
                    .write(&spec.id, GENESIS + 60)
                    .unwrap();
            }
        },
        vec![
            (outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        chain_count > 16,
    )
}

#[test]
fn calibration_bootstrap_and_dense_cleanup_overlap() {
    let proposer = test_evm_signer().address();
    run_prefix(
        "bootstrap +256 accrual +3x256 cleanup +248 bid GC",
        &[(proposer, dummy_pubkey(0xa2))],
        (1, GENESIS + SECONDS_PER_DAY + 60),
        |storage| {
            let day = 20231231;
            let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            let cca = outbe_ccaregistry::schema::CcaContract::new(storage.clone());
            agent
                .cleanup_jobs
                .create(&outbe_agentreward::schema::AccrualJob {
                    seq: 0,
                    day,
                    pool: 0,
                    pool_minor: U256::ZERO,
                    next_index: 0,
                    credited_minor: U256::ZERO,
                    recipient_count: 256,
                    total_weight: U256::ZERO,
                })
                .unwrap();
            agent.cleanup_tail.write(1).unwrap();
            agent.waa_address_count.write(&day.into(), 256).unwrap();
            agent.waa_total_count.write(&day.into(), 256).unwrap();
            rewards.day_cleanup_tail.write(1).unwrap();
            rewards.day_cleanup_day_at.write(&0, day).unwrap();
            rewards.last_sealed_day.write(day).unwrap();
            rewards.day_cleanup_scheduled.write(&day, true).unwrap();
            rewards.day_voter_count.write(&day, 256).unwrap();
            rewards
                .day_total_votes
                .write(&day, U256::from(256))
                .unwrap();
            cca.day_cleanup_tail.write(1).unwrap();
            cca.day_cleanup_queue.write(&0, day).unwrap();
            cca.day_cleanup_scheduled.write(&day, true).unwrap();
            cca.reward_day_count.write(&day, 256).unwrap();
            for index in 0..256u32 {
                let address = numbered_test_address(0x92, u64::from(index));
                let akey =
                    outbe_agentreward::AgentRewardContract::address_index_key(day.into(), index);
                let ckey =
                    outbe_agentreward::AgentRewardContract::tribute_count_key(day.into(), address);
                agent.waa_addresses.write(&akey, address).unwrap();
                agent.waa_tribute_counts.write(&ckey, 1).unwrap();
                agent.waa_member_index.write(&ckey, index + 1).unwrap();
                rewards
                    .day_voter_at
                    .get_nested(&day)
                    .write(&index, address)
                    .unwrap();
                rewards
                    .day_votes
                    .get_nested(&day)
                    .write(&address, 1)
                    .unwrap();
                let index_key = B256::from(U256::from((u64::from(day) << 32) | u64::from(index)));
                let member = outbe_primitives::storage::keys::address_u32_key(address, day);
                cca.reward_day_addresses.write(&index_key, address).unwrap();
                cca.reward_day_members.write(&member, true).unwrap();
                cca.gratis_minor_per_utc_day
                    .write(&member, U256::ONE)
                    .unwrap();
            }
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            queue_accepted_cleanup(storage.clone(), 1);
            seed_previous_day_vwap(
                &storage,
                GENESIS + SECONDS_PER_DAY + 60,
                U256::from(1_000_000),
            );
            seed_dense_voter_participation(storage.clone(), 20240101, 256).unwrap();
            seed_dense_agent_recipients(storage, 20240101, 128).unwrap();
        },
        Vec::new(),
        None,
        false,
    );
}

#[test]
fn calibration_boundary_with_nested_ism_calls() {
    use outbe_hyperlanecontroller::sol_ext::{IInterchainAccountRouter, IStorageMultisigIsm};
    let validators = committee(test_evm_signer().address());
    let validators = &validators[..255];
    let ism = Address::repeat_byte(0xb1);
    let router = Address::repeat_byte(0xb2);
    let local = u32::try_from(CHAIN_ID).unwrap();
    let contracts = vec![
        (
            ism,
            returning_contract(vec![(
                IStorageMultisigIsm::validatorsAndThresholdCall::SELECTOR,
                IStorageMultisigIsm::validatorsAndThresholdCall::abi_encode_returns(
                    &IStorageMultisigIsm::validatorsAndThresholdReturn {
                        _0: Vec::new(),
                        _1: 0,
                    },
                ),
            )]),
        ),
        (
            router,
            returning_contract(vec![(
                IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
                IInterchainAccountRouter::quoteGasPaymentCall::abi_encode_returns(&U256::ZERO),
            )]),
        ),
        (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
    ];
    let boundary = boundary_with_epoch(1, true, validators.to_vec());
    let (phases, _, logs, _) = run_prefix(
        "boundary255 validators x256 domains nested ISM",
        validators,
        (150, GENESIS + 120),
        |storage| {
            let controller =
                outbe_hyperlanecontroller::schema::HyperlaneControllerContract::new(storage);
            controller.ica_router.write(router).unwrap();
            controller.domains.push(local).unwrap();
            controller.ism_by_domain.write(&local, ism).unwrap();
            for domain in 1..=255 {
                assert_ne!(domain, local);
                controller.domains.push(domain).unwrap();
                controller.ism_by_domain.write(&domain, ism).unwrap();
            }
        },
        contracts,
        Some(boundary),
        false,
    );
    assert!(phases.iter().any(|(phase, _)| phase == "BoundaryOutcome"));
    assert!(logs.iter().any(|log| log.data.topics().first()
        == Some(&keccak256("ValidatorsAndThresholdApplied(uint8,uint256)"))));
    assert_eq!(
        logs.iter()
            .filter(|log| log.data.topics().first()
                == Some(&keccak256("RemoteCallDispatched(uint32,bytes32,uint256)")))
            .count(),
        255
    );
}

#[test]
fn calibration_noop_and_calendar_horizon() {
    let proposer = test_evm_signer().address();
    for elapsed_days in [0, 1, 65] {
        let (_, handlers, logs, _) = run_prefix(
            &format!("calendar elapsed_days={elapsed_days}"),
            &[(proposer, dummy_pubkey(0xa2))],
            (1, GENESIS + elapsed_days * SECONDS_PER_DAY + 60),
            |storage| {
                seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
                for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
                    outbe_cycle::schema::Cycle::new(storage.clone())
                        .last_executed_at
                        .write(&spec.id, GENESIS + 1)
                        .unwrap();
                }
                if elapsed_days == 1 {
                    let mut agent = outbe_agentreward::AgentRewardContract::new(storage);
                    agent
                        .increment_waa_tribute(20240101u32.into(), Address::repeat_byte(0x44))
                        .unwrap();
                    agent
                        .pending_total_minor
                        .write(
                            &(outbe_agentreward::schema::RewardPool::Waa as u8),
                            U256::MAX,
                        )
                        .unwrap();
                }
            },
            Vec::new(),
            None,
            false,
        );
        if elapsed_days == 0 {
            assert!(handlers.is_empty());
        } else {
            assert!(handlers.iter().any(|(id, _)| *id == 0));
            if elapsed_days == 1 {
                assert!(logs.iter().any(|log| log.data.topics().first()
                    == Some(
                        &outbe_cycle::ICycle::DayEmissionLimitAllocationFailed::SIGNATURE_HASH
                    )));
            }
        }
    }
}
