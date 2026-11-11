// Builder coverage for the 4-stage system-transaction schedule.
//
// `CONSENSUS_ORDER` is CertifiedParentAccounting, LateFinalizeCredits,
// BoundaryOutcome, TeeBootstrap, ValidatorLiveness, and RadiclePublication.
// `BEGIN_BLOCK_ORDER` is Vote, Update, EnclaveUpgradeDeadline, Rewards,
// Staking, Oracle, OracleSlash, HyperlaneSlash, and CycleTick.
// `END_BLOCK_ORDER` is NodFactory, GemFactory, IntexFactory, CredisFactory,
// and GemCleanup.

use alloy_evm::{block::CommitChanges, Evm as _, RecoveredTx};
use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, encode_outbe_block_artifacts, ConsensusHeaderArtifact,
    ExecutionSummaryArtifact, OutbeBlockArtifacts,
};
use outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;
use outbe_primitives::OutbeHeader;
use reth_ethereum::{
    evm::revm::db::State,
    primitives::{Header, SealedHeader},
};
use reth_evm::{
    execute::{BlockBuilder, Executor, ProviderError},
    ConfigureEvm,
};
use reth_provider::{HashedPostStateProvider, StateRootProvider};
use revm::{database::CacheDB, database_interface::EmptyDBTyped, state::AccountInfo};

use super::harness::*;

#[test]
fn final_artifact_adapter_binds_execution_summary_and_preserves_consensus_fields() {
    let prefinal = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(999),
        }),
        consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
            b"dealer",
        ))),
        timestamp_millis_part: 999,
        ..Default::default()
    })
    .unwrap();
    let encoded = outbe_evm::builder::encode_final_header_artifacts(
        &prefinal,
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(7),
        },
        321,
        Some(Bytes::from_static(b"witness-v1")),
    )
    .unwrap();
    let decoded = decode_outbe_block_artifacts(&encoded).unwrap();
    assert_eq!(
        decoded.consensus_header_artifact,
        Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
            b"dealer"
        )))
    );
    assert_eq!(
        decoded.execution_summary.unwrap().validator_fee_sum,
        U256::from(7)
    );
    assert_eq!(decoded.timestamp_millis_part, 321);
    assert_eq!(
        decoded.enclave_witness,
        Some(Bytes::from_static(b"witness-v1"))
    );
    let other = outbe_evm::builder::encode_final_header_artifacts(
        &prefinal,
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(8),
        },
        321,
        None,
    )
    .unwrap();
    let first = SealedHeader::seal_slow(OutbeHeader::new(Header {
        extra_data: encoded,
        ..Default::default()
    }));
    let second = SealedHeader::seal_slow(OutbeHeader::new(Header {
        extra_data: other,
        ..Default::default()
    }));
    assert_ne!(first.hash(), second.hash());
}

#[test]
fn builder_keeps_preexecuted_phase1_witness_in_block_body() {
    let bridge = test_bridge();
    let parent = test_parent_at(1);
    bridge.record_execution_summary_with_state_root(
        1,
        parent.hash(),
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        parent.header().inner.timestamp,
        parent.header().inner.state_root,
    );
    let config = test_config(bridge);
    let provider = DeterministicEmptyStateProvider;
    let mut proposer_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut proposer_db, &[GENESIS_OWNER]);
    let mut proposer_state = State::builder()
        .with_database(proposer_db)
        .with_bundle_update()
        .build();

    let mut attrs = next_block_attrs(Bytes::new());
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: parent.hash(),
        ..Default::default()
    };
    attrs.parent_consensus_metadata = Some(metadata);

    let phase1 = config
        .build_signed_phase1_tx(
            2,
            GRAMINE_DIRECT_DEV_CHAIN_ID,
            parent.hash(),
            attrs.parent_consensus_metadata.clone(),
            attrs.proposer_evm_address,
        )
        .expect("Phase 1 prebuild must succeed")
        .expect("block 2 must prebuild Phase 1");
    let phase1_hash = phase1.tx().signature_hash();

    let evm_env = config
        .next_evm_env(&parent, &attrs)
        .expect("next block EVM env must build");
    let ctx = config
        .context_for_next_block(&parent, attrs)
        .expect("next block context must build");
    let begin = config
        .build_begin_system_txs(
            2,
            GRAMINE_DIRECT_DEV_CHAIN_ID,
            evm_env.block_env.gas_limit,
            parent.hash(),
            &ctx.inner.extra_data,
            ctx.parent_consensus_metadata.clone(),
            ctx.proposer_evm_address,
            None,
            None,
        )
        .expect("complete block-2 begin prefix must build");
    let begin_len = begin.len();
    let evm = config.evm_with_env(&mut proposer_state, evm_env);
    let mut builder = config.create_block_builder(evm, &parent, ctx);
    outbe_evm::executor::with_phase1_verify_disabled(|| builder.apply_pre_execution_changes())
        .expect("Phase 1 must be pre-executed before witness validation");
    builder
        .executor_mut()
        .force_preexecuted_phase1_witness_for_test(phase1_hash);

    let gas_used = builder
        .execute_transaction_with_commit_condition(phase1.clone(), |_| CommitChanges::Yes)
        .expect("pre-executed Phase 1 witness must validate");

    assert!(
        gas_used.is_none(),
        "Phase 1 witness validation must not commit or charge gas twice"
    );
    for tx in begin.into_iter().skip(1) {
        builder
            .execute_transaction(tx)
            .expect("remaining begin phase executes");
    }
    let end_inputs = [
        outbe_evm::system_tx::SystemTx::EndBlock { step_index: 0 },
        outbe_evm::system_tx::SystemTx::EndBlock { step_index: 1 },
        outbe_evm::system_tx::SystemTx::EndBlock { step_index: 2 },
        outbe_evm::system_tx::SystemTx::EndBlock { step_index: 3 },
        outbe_evm::system_tx::SystemTx::EndBlock { step_index: 4 },
    ];
    let end_len = end_inputs.len();
    for input in end_inputs {
        let tx = config
            .build_end_system_tx(2, GRAMINE_DIRECT_DEV_CHAIN_ID, GENESIS_OWNER, input)
            .expect("end-block system tx must build");
        builder
            .execute_transaction(tx)
            .expect("end-block phase executes");
    }
    let outcome = builder
        .finish(&provider, None)
        .expect("block with retained Phase 1 witness must finish");
    assert_eq!(
        outcome.block.body().transactions.len(),
        begin_len + end_len,
        "finished block body must retain the Phase 1 witness tx and EndBlock suffix"
    );
    assert_eq!(
        outcome.block.body().transactions[0].signature_hash(),
        phase1_hash,
        "body[0] must be the exact pre-executed Phase 1 witness"
    );
}

#[test]
fn builder_finish_rejects_block_2_without_end_block_phase() {
    let bridge = test_bridge();
    let parent = test_parent_at(1);
    bridge.record_execution_summary_with_state_root(
        1,
        parent.hash(),
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        parent.header().inner.timestamp,
        parent.header().inner.state_root,
    );
    let config = test_config(bridge);
    let provider = DeterministicEmptyStateProvider;
    let mut proposer_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut proposer_db, &[GENESIS_OWNER]);
    let mut proposer_state = State::builder()
        .with_database(proposer_db)
        .with_bundle_update()
        .build();

    let mut attrs = next_block_attrs(Bytes::new());
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: parent.hash(),
        ..Default::default()
    };
    attrs.parent_consensus_metadata = Some(metadata);

    let evm_env = config
        .next_evm_env(&parent, &attrs)
        .expect("next block EVM env must build");
    let ctx = config
        .context_for_next_block(&parent, attrs)
        .expect("next block context must build");
    let begin = config
        .build_begin_system_txs(
            2,
            GRAMINE_DIRECT_DEV_CHAIN_ID,
            evm_env.block_env.gas_limit,
            parent.hash(),
            &ctx.inner.extra_data,
            ctx.parent_consensus_metadata.clone(),
            ctx.proposer_evm_address,
            None,
            None,
        )
        .expect("complete block-2 begin prefix must build");
    let evm = config.evm_with_env(&mut proposer_state, evm_env);
    let mut builder = config.create_block_builder(evm, &parent, ctx);
    outbe_evm::executor::with_phase1_verify_disabled(|| builder.apply_pre_execution_changes())
        .expect("Phase 1 must be pre-executed");
    for tx in begin {
        builder
            .execute_transaction(tx)
            .expect("begin phase executes");
    }
    let err = match builder.finish(&provider, None) {
        Ok(_) => panic!("builder.finish on block 2 without EndBlock must fail"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("block execution finished in stage MidBlock"),
        "expected IncompleteBlockExecution error, got: {err}"
    );
}

#[test]
fn base_block_height2_reexecutes_with_same_state_root() {
    assert_height2_finalization_parity(false);
}

#[test]
fn mixed_block_height2_reexecutes_with_same_state_root() {
    assert_height2_finalization_parity(true);
}

fn assert_height2_finalization_parity(include_user: bool) {
    use alloy_consensus::{SignableTransaction as _, TxEip1559};
    use alloy_primitives::{Signature, TxKind};
    use reth_primitives_traits::SignedTransaction as _;

    let active_set = [GENESIS_OWNER];
    let parent = test_parent_at(1);

    let bridge = test_bridge();
    bridge.record_execution_summary_with_state_root(
        1,
        parent.hash(),
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        1,
        parent.header().inner.state_root,
    );
    let config = test_config(bridge);
    let provider = DeterministicEmptyStateProvider;
    let user: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: GRAMINE_DIRECT_DEV_CHAIN_ID,
        gas_limit: 100_000,
        max_fee_per_gas: 2_000_000_000,
        to: TxKind::Call(Address::repeat_byte(0xDD)),
        ..Default::default()
    }
    .into_signed(Signature::test_signature())
    .into();
    let user = user.try_into_recovered().unwrap();
    let seed_user = |db: &mut CacheDB<EmptyDBTyped<ProviderError>>| {
        if include_user {
            db.insert_account_info(
                user.signer(),
                AccountInfo {
                    balance: U256::from(100_000_000_000_000_000_000u128),
                    ..Default::default()
                },
            );
        }
    };

    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: parent.hash(),
        ..CertifiedParentAccountingMetadata::default()
    };

    let mut attrs = next_block_attrs(Bytes::new());
    attrs.parent_consensus_metadata = Some(metadata.clone());

    let build = |prepare_root: bool| {
        let mut proposer_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        seed_active_validators(&mut proposer_db, &active_set);
        seed_user(&mut proposer_db);
        assert_seeded_validators(&mut proposer_db, &active_set);
        let mut proposer_state = State::builder()
            .with_database(proposer_db)
            .with_bundle_update()
            .build();
        outbe_evm::executor::with_phase1_verify_disabled(|| {
            let mut builder = config
                .builder_for_next_block(&mut proposer_state, &parent, attrs.clone())
                .expect("builder must be created");
            builder
                .apply_pre_execution_changes()
                .expect("pre-execution changes must succeed");
            let begin_system_txs = config
                .build_begin_system_txs(
                    2,
                    GRAMINE_DIRECT_DEV_CHAIN_ID,
                    30_000_000,
                    parent.hash(),
                    &attrs.inner.extra_data,
                    attrs.parent_consensus_metadata.clone(),
                    attrs.proposer_evm_address,
                    None,
                    None,
                )
                .expect("begin-zone system txs must build");
            assert_eq!(
                begin_system_txs.len(),
                12,
                "block 2 base-block begin-zone must carry the twelve mandatory phases",
            );
            for tx in begin_system_txs {
                builder
                    .execute_transaction(tx)
                    .expect("begin-zone system tx must execute through builder tx loop");
            }
            if include_user {
                builder.execute_transaction(user.clone()).unwrap();
            }
            builder.executor_mut().apply_end_block_phase().unwrap();
            let precomputed = if prepare_root {
                builder
                    .executor_mut()
                    .prepare_final_header_artifacts(0)
                    .unwrap();
                let db = builder.evm_mut().db_mut();
                db.merge_transitions(
                    revm::database::states::bundle_state::BundleRetention::Reverts,
                );
                let hashed = provider.hashed_post_state(&db.bundle_state).unwrap();
                Some(provider.state_root_with_updates(hashed).unwrap())
            } else {
                None
            };
            builder
                .finish(&provider, precomputed)
                .expect("proposer path must build a block")
        })
    };
    let synchronous = build(false);
    let prepared = build(true);
    assert_eq!(
        synchronous.execution_result.receipts,
        prepared.execution_result.receipts
    );
    assert_eq!(
        synchronous.execution_result.gas_used,
        prepared.execution_result.gas_used
    );
    assert_eq!(
        synchronous.block, prepared.block,
        "prepared-root and synchronous end completion must agree"
    );
    let built_block = synchronous.block;
    let built_root = built_block.header().inner.state_root;

    let built_artifacts =
        decode_outbe_block_artifacts(built_block.header().inner.extra_data.as_ref())
            .expect("built header extra_data must decode");
    assert!(
        built_artifacts.consensus_header_artifact.is_none(),
        "base block must not carry a consensus-header artifact",
    );
    assert_eq!(
        built_artifacts.execution_summary,
        Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        "proposer finish must inject execution summary into final header extra_data",
    );

    let mut validator_db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_active_validators(&mut validator_db, &active_set);
    seed_user(&mut validator_db);
    let validator_state = State::builder()
        .with_database(validator_db)
        .with_bundle_update()
        .build();
    let validator_state = outbe_evm::executor::with_phase1_verify_disabled(|| {
        let mut validator_executor = config.batch_executor(validator_state);
        let replay = validator_executor
            .execute_one(&built_block)
            .expect("validator path must re-execute the built block");
        assert_eq!(replay.receipts, synchronous.execution_result.receipts);
        assert_eq!(replay.gas_used, synchronous.execution_result.gas_used);
        validator_executor.into_state()
    });

    let reexecuted_hashed_state = provider.hashed_post_state(&validator_state.bundle_state);
    let (reexecuted_root, _) = provider
        .state_root_with_updates(reexecuted_hashed_state.unwrap())
        .expect("re-executed state root must be computed");

    assert_eq!(
        built_root, reexecuted_root,
        "locally built base block must re-execute to the same state root on validator path",
    );
}
