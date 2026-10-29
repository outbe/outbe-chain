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
