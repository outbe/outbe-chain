//! Rewards logs must use the executing system receipt, not HookEvents.

use super::*;
use outbe_rewards::precompile::IRewards;

#[test]
fn real_cpa_escrow_and_late_settlement_receipts_do_not_duplicate_body_witness_logs() {
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
    let voters = [snapshot.committee[1].address, snapshot.committee[0].address];
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
        outbe_cycle::record_phase1_progress(&ctx, parent_number - 1).unwrap();
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
    for (log, voter) in logs[..2].iter().zip(voters) {
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

#[test]
fn cycle_tick_rewards_retention_slice_fits_budget_and_rolls_back_on_failure() {
    use outbe_primitives::{system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT, time::SECONDS_PER_DAY};

    const VOTERS: u32 = 257;
    let timestamp = TEST_BLOCK_TIMESTAMP_BASE;
    let day = outbe_primitives::time::timestamp_to_date_key(timestamp - 8 * SECONDS_PER_DAY);
    let other_day = outbe_primitives::time::previous_date_key(day);
    // The successful case measures real storage work. The other cases fail after a
    // tail mutation, or after execution at the aggregate system-work admission guard.
    for failure in [None, Some("mid-slice"), Some("work-budget")] {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xa2))], |storage| {
                seed_dense_voter_participation(storage.clone(), day, VOTERS).unwrap();
                let rewards = outbe_rewards::schema::Rewards::new(storage.clone());
                rewards
                    .daily_fee_sum_raw
                    .write(&day, U256::from(987))
                    .unwrap();
                rewards
                    .daily_participation_closed
                    .write(&day, true)
                    .unwrap();
                rewards
                    .daily_participation_closed
                    .write(&other_day, true)
                    .unwrap();
                rewards
                    .daily_fee_sum_raw
                    .write(&other_day, U256::from(654))
                    .unwrap();
                let ctx = BlockRuntimeContext::new(
                    BlockContext::new(1, timestamp, CHAIN_ID, proposer, vec![proposer]),
                    storage,
                );
                outbe_rewards::api::mark_day_settled(&ctx, day).unwrap();
                outbe_rewards::api::mark_day_settled(&ctx, other_day).unwrap();
                if failure == Some("mid-slice") {
                    rewards
                        .daily_participation
                        .get_nested(&day)
                        .write(&numbered_test_address(0x12, 255), 0)
                        .unwrap();
                }
            });
        let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
        let mut env = test_evm_env(1, REWARDS_ADDRESS);
        env.block_env.timestamp = U256::from(timestamp);
        let evm = config.evm_with_env(&mut state, env);
        let mut executor =
            config.create_executor(evm, block_one_execution_ctx(Some(0), Bytes::new()));
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
        if failure == Some("work-budget") {
            executor.system_tx_execution_gas = SYSTEM_TX_ARTIFACT_GAS_LIMIT - 1;
        }
        let internal_before = executor.system_tx_execution_gas;
        let signed_limit = cycle_tx.tx().gas_limit();
        let result = executor.execute_transaction(cycle_tx);
        if let Some(failure) = failure {
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
        drop(executor);
        let mut provider = super::super::DirectStorageProvider::new(
            &mut state,
            BlockContext::new(1, timestamp, CHAIN_ID, proposer, vec![proposer]),
        );
        let storage = StorageHandle::new(&mut provider);
        let rewards = outbe_rewards::schema::Rewards::new(storage);
        assert_eq!(
            rewards.daily_voter_count.read(&day).unwrap(),
            if failure.is_none() { 1 } else { VOTERS }
        );
        for index in 0..VOTERS {
            let voter = numbered_test_address(0x12, u64::from(index));
            let removed = failure.is_none() && index > 0;
            assert_eq!(
                rewards
                    .daily_voter_at
                    .get_nested(&day)
                    .read(&index)
                    .unwrap(),
                if removed { Address::ZERO } else { voter }
            );
            let expected = if removed || (failure == Some("mid-slice") && index == 255) {
                0
            } else {
                1
            };
            assert_eq!(
                rewards
                    .daily_participation
                    .get_nested(&day)
                    .read(&voter)
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&day).unwrap(),
            U256::from(987)
        );
        assert!(rewards.daily_settled.read(&day).unwrap());
        assert!(rewards.daily_participation_closed.read(&day).unwrap());
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 0);
        assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 2);
        assert_eq!(rewards.settled_day_cleanup_day_at.read(&0).unwrap(), day);
        assert_eq!(
            rewards.settled_day_cleanup_day_at.read(&1).unwrap(),
            other_day
        );
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&other_day).unwrap(),
            U256::from(654),
            "one invocation must not process the next expired queued day"
        );
    }
}
