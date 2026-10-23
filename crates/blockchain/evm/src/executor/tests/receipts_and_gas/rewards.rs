//! Rewards logs must use the executing system receipt, not HookEvents.

use super::*;
use outbe_rewards::precompile::IRewards;

#[test]
fn real_cpa_escrow_and_late_settlement_receipts_do_not_duplicate_body_witness_logs() {
    assert_real_cpa_and_late_settlement(false);
}

#[test]
fn invalid_slash_percent_rejects_mandatory_late_window_without_committing_penalty() {
    assert_real_cpa_and_late_settlement(true);
}

fn assert_real_cpa_and_late_settlement(invalid_slash_percent: bool) {
    use commonware_codec::Encode;
    use commonware_consensus::{simplex::types::Proposal, types::View};
    use commonware_cryptography::{bls12381::primitives::variant::MinSig, sha256::Digest};
    use outbe_consensus::{
        proof::{finalize_namespace, verify_v2_proof, Finalization},
        test_harness::{
            finalize_messages, test_fully_signed_metadata, vrf_test_committee, CertificateMessages,
            TestFinalizedParent,
        },
    };
    use outbe_primitives::consensus_metadata::ParentParticipationProof;
    use outbe_rewards::late_settlement::{escrow_block_fee, FinalizedBlockBinding};

    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID).unwrap();
    let signer = test_evm_signer();
    let proposer = signer.address();
    let block_number = outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K + 1;
    let parent_number = block_number - 1;
    let parent_hash = B256::repeat_byte(0x91);
    let settled_hash = B256::repeat_byte(0x92);
    let committee = vrf_test_committee(4);
    let snapshot = committee.snapshot(1);
    let (round, vote, seed) = finalize_messages(0, 9, 8, parent_hash);
    let namespace = finalize_namespace(&committee.committee_set());
    let certificate = committee.certificate(
        &[0, 1, 2, 3],
        &CertificateMessages {
            vote_namespace: &namespace,
            vote: &vote,
            seed: &seed,
        },
        &committee.vrf_threshold_private,
        1,
    );
    let finalization: Finalization<outbe_consensus::hybrid::HybridScheme<MinSig>, Digest> =
        Finalization {
            proposal: Proposal::new(round, View::new(8), Digest(parent_hash.0)),
            certificate,
        };
    let metadata = test_fully_signed_metadata(
        &TestFinalizedParent {
            block_number: parent_number,
            block_hash: parent_hash,
            epoch: 0,
            view: 9,
            parent_view: 8,
            vrf_material_version: 1,
            proof_kind: ParentParticipationProof::Finalization,
        },
        &snapshot,
        &finalization.encode(),
    );
    let verified = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash).unwrap();
    let mut voters = vec![snapshot.committee[1].address, snapshot.committee[0].address];
    if invalid_slash_percent {
        voters.push(snapshot.committee[2].address);
    }
    let absent = snapshot.committee[3].address;
    let unit = outbe_primitives::units::NATIVE_UNITS_PER_PROTOCOL_UNIT;
    let matured_fee = U256::from(8_000) * unit;
    let parent_fee = U256::from(123);
    let mut validators = vec![(proposer, dummy_pubkey(0xa2))];
    validators.extend(
        snapshot
            .committee
            .iter()
            .map(|entry| (entry.address, entry.consensus_pubkey)),
    );
    let mut state = state_with_active_validators_seeded(&validators, |storage| {
        outbe_validatorset::write_committee_snapshot(storage.clone(), 0, &snapshot).unwrap();
        if invalid_slash_percent {
            let si = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
            si.config_voter_felony_threshold.write(1).unwrap();
            si.config_slash_amount_percent.write(101).unwrap();
            let staking = outbe_staking::contract::Staking::new(storage.clone());
            for (validator, _) in &validators {
                staking
                    .stake_amount
                    .write(validator, U256::from(1_000))
                    .unwrap();
            }
            let total = U256::from(validators.len() * 1_000);
            staking.total_staked.write(total).unwrap();
            storage.set_balance(STAKING_ADDRESS, total).unwrap();
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(1, TEST_BLOCK_TIMESTAMP_BASE, CHAIN_ID, proposer, vec![]),
            storage.clone(),
        );
        escrow_block_fee(
            &ctx,
            &FinalizedBlockBinding {
                number: 1,
                hash: settled_hash,
                committee_size: 4,
                epoch: 0,
                view: 1,
                parent_view: 0,
                committee_set_hash: metadata.committee_set_hash,
            },
            matured_fee,
            &voters,
        )
        .unwrap();
        outbe_rewards::schema::Rewards::new(storage.clone())
            .pending_reward_day
            .write(
                &settled_hash,
                outbe_primitives::time::timestamp_to_date_key(TEST_BLOCK_TIMESTAMP_BASE),
            )
            .unwrap();
        storage
            .set_balance(REWARDS_ADDRESS, matured_fee + parent_fee)
            .unwrap();
        ctx.storage
            .contract::<outbe_cycle::schema::Accounting<'_>>()
            .last_accounted_block_number
            .write(parent_number - 1)
            .unwrap();
    });
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let mut env = test_evm_env(block_number, REWARDS_ADDRESS);
    env.block_env.prevrandao = Some(verified.prev_randao);
    let evm = config.evm_with_env(&mut state, env);
    let ctx = fixtures::parent_accounting_context(fixtures::ParentAccountingFixture {
        parent_hash,
        metadata: metadata.clone(),
        artifact: AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: parent_fee,
            },
            timestamp: TEST_BLOCK_TIMESTAMP_BASE + parent_number,
            state_root: Some(B256::repeat_byte(0x93)),
        },
        proposer,
    });
    let mut executor = config.create_executor(evm, ctx);
    executor
        .apply_pre_execution_changes()
        .expect("real CPA proof must verify and execute");
    assert_eq!(
        executor.receipts().len(),
        1,
        "CPA executes before the body loop"
    );
    let cpa = executor.receipts()[0].clone();
    assert!(cpa.success);
    let escrow_logs: Vec<_> = cpa
        .logs
        .iter()
        .filter(|log| log.address == REWARDS_ADDRESS)
        .collect();
    assert_eq!(escrow_logs.len(), 1);
    let escrow = IRewards::BlockFeeEscrowed::decode_log(escrow_logs[0]).unwrap();
    assert_eq!(escrow.fbHash, parent_hash);
    assert_eq!(escrow.fbNumber, parent_number);
    assert_eq!(escrow.feeSumNative, parent_fee);
    assert_rewards_bloom(&cpa, escrow_logs[0]);

    let mut system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    )
    .into_iter();
    executor
        .execute_transaction(system_txs.next().unwrap())
        .expect("CPA body witness");
    assert_eq!(
        executor.receipts(),
        &[cpa],
        "body witness must not execute CPA twice"
    );
    let late_tx = system_txs.next().unwrap();
    assert!(matches!(
        SystemTxInputV2::decode(late_tx.tx().input()).unwrap(),
        SystemTxInputV2::LateFinalizeCredits { .. }
    ));
    if invalid_slash_percent {
        use alloy_sol_types::SolError;
        use outbe_slashindicator::precompile::ISlashIndicator;

        let before_late = executor.receipts().to_vec();
        let error = executor
            .execute_transaction(late_tx)
            .expect_err("invalid configuration must reject mandatory LateFinalizeCredits");
        assert!(matches!(
            &error,
            alloy_evm::block::BlockExecutionError::Validation(_)
        ));
        let message = error.to_string();
        assert!(
            message.contains("critical system tx LateFinalizeCredits did not succeed"),
            "{message}"
        );
        assert!(message.contains("Revert"), "{message}");
        let expected = Bytes::from(
            ISlashIndicator::SlashConfigPercentAbove100 {
                field: "slash_amount_percent".into(),
                value: 101,
            }
            .abi_encode(),
        );
        assert!(message.contains(&format!("{expected:?}")), "{message}");
        assert_eq!(executor.receipts(), before_late.as_slice());
        drop(executor);

        let read = BlockContext::new(
            block_number,
            TEST_BLOCK_TIMESTAMP_BASE + block_number,
            CHAIN_ID,
            proposer,
            vec![],
        );
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read);
        StorageHandle::enter(&mut provider, |storage| {
            let si = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
            assert_eq!(si.voter_miss_count.read(&absent)?, 0);
            assert_eq!(si.felony_count.read(&absent)?, 0);
            assert!(!si.voter_window_slashed.read(&settled_hash)?);
            let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let record = vs
                .get_validator(absent)?
                .expect("absentee remains registered");
            assert_eq!(record.status, outbe_validatorset::logic::status::ACTIVE);
            assert_eq!(record.slash_count, 0);
            let staking = outbe_staking::contract::Staking::new(storage.clone());
            let total = U256::from(validators.len() * 1_000);
            assert_eq!(staking.get_stake(absent)?, U256::from(1_000));
            assert_eq!(staking.total_staked.read()?, total);
            assert_eq!(storage.balance(STAKING_ADDRESS)?, total);
            let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
            assert_eq!(rewards.pending_fees.read(&settled_hash)?, matured_fee);
            assert!(!rewards.fee_settled.read(&settled_hash)?);
            assert_eq!(rewards.late_voter_count.read(&settled_hash)?, 3);
            assert_eq!(rewards.pending_fb_hash_at.read(&1)?, settled_hash);
            assert_eq!(storage.balance(REWARDS_ADDRESS)?, matured_fee + parent_fee);
            Ok::<_, outbe_primitives::error::PrecompileError>(())
        })
        .expect("failed mandatory phase leaves prior escrow and penalties unchanged");
        return;
    }
    executor
        .execute_transaction(late_tx)
        .expect("matured fee settlement");
    let late = &executor.receipts()[1];
    assert!(late.success);
    let logs: Vec<_> = late
        .logs
        .iter()
        .filter(|log| log.address == REWARDS_ADDRESS)
        .collect();
    assert_eq!(
        logs.len(),
        3,
        "two positive payments followed by one summary"
    );
    for (log, voter) in logs[..2].iter().zip(voters.iter().copied()) {
        let payment = IRewards::VoterFeePaid::decode_log(log).unwrap();
        assert_eq!(payment.fbHash, settled_hash);
        assert_eq!(
            payment.voter, voter,
            "preserve escrow voter iteration order"
        );
        assert_eq!(payment.amountNative, U256::from(2_000) * unit);
        assert_rewards_bloom(late, log);
    }
    let summary = IRewards::LateWindowSettled::decode_log(logs[2]).unwrap();
    assert_eq!(summary.fbHash, settled_hash);
    assert_eq!(summary.distributedNative, U256::from(4_000) * unit);
    assert_eq!(summary.residueNative, U256::from(4_000) * unit);
    assert_eq!(summary.burnedNative, summary.residueNative);
    assert_eq!(summary.dustAfterNative, U256::ZERO);
    assert_rewards_bloom(late, logs[2]);
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("remaining begin prefix");
    }
    assert!(
        executor.receipts()[2..].iter().all(|receipt| receipt
            .logs
            .iter()
            .all(|log| log.address != REWARDS_ADDRESS)),
        "Rewards logs must not leak into CycleTick or HookEvents"
    );
    drop(executor);
    let mut provider = super::super::DirectStorageProvider::new(
        &mut state,
        BlockContext::new(
            block_number,
            TEST_BLOCK_TIMESTAMP_BASE + block_number,
            CHAIN_ID,
            proposer,
            vec![],
        ),
    );
    let storage = StorageHandle::new(&mut provider);
    for voter in voters {
        assert_eq!(storage.balance(voter).unwrap(), U256::from(2_000) * unit);
    }
    assert_eq!(storage.balance(REWARDS_ADDRESS).unwrap(), parent_fee);
    assert!(outbe_rewards::schema::Rewards::new(storage)
        .fee_settled
        .read(&settled_hash)
        .unwrap());
}

fn assert_rewards_bloom(receipt: &Receipt, log: &Log) {
    use alloy_primitives::BloomInput;
    let bloom = logs_bloom(receipt.logs.iter());
    assert!(bloom.contains_input(BloomInput::Raw(log.address.as_slice())));
    for topic in log.data.topics() {
        assert!(bloom.contains_input(BloomInput::Raw(topic.as_slice())));
    }
}

/// Shared inputs for one retention scenario. Expected fees, queue bounds, and
/// participation values stay at the assertion sites.
struct RetentionSlice {
    day: u32,
    other_day: u32,
    timestamp: u64,
    voters: u32,
    failure: Option<&'static str>,
}

#[test]
fn cycle_tick_rewards_retention_slice_fits_budget_and_rolls_back_on_failure() {
    use outbe_primitives::time::SECONDS_PER_DAY;

    const VOTERS: u32 = 257;
    let timestamp = TEST_BLOCK_TIMESTAMP_BASE;
    let day = outbe_primitives::time::timestamp_to_date_key(timestamp - 8 * SECONDS_PER_DAY);
    let other_day = outbe_primitives::time::previous_date_key(day).expect("date step");
    // The successful case measures real storage work. The other cases fail after a
    // tail mutation, or after execution at the aggregate system-work admission guard.
    for failure in [None, Some("mid-slice"), Some("work-budget")] {
        let slice = RetentionSlice {
            day,
            other_day,
            timestamp,
            voters: VOTERS,
            failure,
        };
        let (mut state, signer, proposer) = prepare_retention_slice(&slice);
        assert_retention_cycle_tick_outcome(&mut state, signer, proposer, &slice);
        assert_retention_slice_storage(&mut state, proposer, &slice);
    }
}

fn prepare_retention_slice(
    slice: &RetentionSlice,
) -> (
    State<CacheDB<EmptyDBTyped<ProviderError>>>,
    std::sync::Arc<OutbeEvmSigner>,
    Address,
) {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let state = state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xa2))], |storage| {
        seed_retention_days(storage, proposer, slice);
    });
    (state, signer, proposer)
}

fn seed_retention_days(storage: StorageHandle, proposer: Address, slice: &RetentionSlice) {
    seed_dense_voter_participation(storage.clone(), slice.day, slice.voters).unwrap();
    let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
    rewards
        .daily_fee_sum_raw
        .write(&slice.day, U256::from(987))
        .unwrap();
    rewards
        .daily_participation_closed
        .write(&slice.day, true)
        .unwrap();
    rewards
        .daily_participation_closed
        .write(&slice.other_day, true)
        .unwrap();
    rewards
        .daily_fee_sum_raw
        .write(&slice.other_day, U256::from(654))
        .unwrap();
    let ctx = BlockRuntimeContext::new(
        BlockContext::new(1, slice.timestamp, CHAIN_ID, proposer, vec![proposer]),
        storage,
    );
    outbe_rewards::api::mark_day_settled(&ctx, slice.day).unwrap();
    outbe_rewards::api::mark_day_settled(&ctx, slice.other_day).unwrap();
    if slice.failure == Some("mid-slice") {
        rewards
            .daily_participation
            .get_nested(&slice.day)
            .write(&numbered_test_address(0x12, 255), 0)
            .unwrap();
    }
}

fn assert_retention_cycle_tick_outcome(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    signer: std::sync::Arc<OutbeEvmSigner>,
    proposer: Address,
    slice: &RetentionSlice,
) {
    use outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;

    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
    let mut env = test_evm_env(1, REWARDS_ADDRESS);
    env.block_env.timestamp = U256::from(slice.timestamp);
    let evm = config.evm_with_env(state, env);
    let mut executor = config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));
    executor.apply_pre_execution_changes().unwrap();
    let cycle_tx = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    )
    .remove(0);
    if slice.failure == Some("work-budget") {
        executor.system_tx_execution_gas = SYSTEM_TX_ARTIFACT_GAS_LIMIT - 1;
    }
    let internal_before = executor.system_tx_execution_gas;
    let signed_limit = cycle_tx.tx().gas_limit();
    let result = executor.execute_transaction(cycle_tx);
    if let Some(failure) = slice.failure {
        let error = result.expect_err("failed retention slice rejects critical CycleTick");
        if failure == "work-budget" {
            assert!(
                error.to_string().contains("internal system-work budget"),
                "{error}"
            );
        } else {
            assert!(
                error.to_string().contains("critical system tx CycleTick"),
                "{error}"
            );
        }
        assert!(executor.receipts().is_empty());
        assert_eq!(executor.system_tx_execution_gas, internal_before);
        assert_eq!(executor.inner.cumulative_tx_gas_used, 0);
    } else {
        let gas = result.expect("256-voter retention slice must fit CycleTick work budget");
        assert!(gas.tx_gas_used() <= signed_limit);
        assert!(executor.receipts()[0].success);
        let work = executor.system_tx_execution_gas - internal_before;
        assert!(work > 0 && work < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
        eprintln!(
            "Rewards 256-voter CycleTick cleanup: internal={work}, visible={}",
            gas.tx_gas_used()
        );
    }
}

fn assert_retention_slice_storage(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    proposer: Address,
    slice: &RetentionSlice,
) {
    let mut provider = super::super::DirectStorageProvider::new(
        state,
        BlockContext::new(1, slice.timestamp, CHAIN_ID, proposer, vec![proposer]),
    );
    let storage = StorageHandle::new(&mut provider);
    let rewards = outbe_rewards::schema::Rewards::new(storage);
    assert_eq!(
        rewards.daily_voter_count.read(&slice.day).unwrap(),
        if slice.failure.is_none() {
            1
        } else {
            slice.voters
        }
    );
    for index in 0..slice.voters {
        let voter = numbered_test_address(0x12, u64::from(index));
        let removed = slice.failure.is_none() && index > 0;
        assert_eq!(
            rewards
                .daily_voter_at
                .get_nested(&slice.day)
                .read(&index)
                .unwrap(),
            if removed { Address::ZERO } else { voter }
        );
        let expected = if removed || (slice.failure == Some("mid-slice") && index == 255) {
            0
        } else {
            1
        };
        assert_eq!(
            rewards
                .daily_participation
                .get_nested(&slice.day)
                .read(&voter)
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        rewards.daily_fee_sum_raw.read(&slice.day).unwrap(),
        U256::from(987)
    );
    assert!(rewards.daily_settled.read(&slice.day).unwrap());
    assert!(rewards.daily_participation_closed.read(&slice.day).unwrap());
    assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 0);
    assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 2);
    assert_eq!(
        rewards.settled_day_cleanup_day_at.read(&0).unwrap(),
        slice.day
    );
    assert_eq!(
        rewards.settled_day_cleanup_day_at.read(&1).unwrap(),
        slice.other_day
    );
    assert_eq!(
        rewards.daily_fee_sum_raw.read(&slice.other_day).unwrap(),
        U256::from(654),
        "one invocation must not process the next expired queued day"
    );
}
