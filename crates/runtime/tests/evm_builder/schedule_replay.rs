// Builder coverage for the 4-stage system-transaction schedule.
//
// `CONSENSUS_ORDER` is CertifiedParentAccounting, LateFinalizeCredits,
// BoundaryOutcome, TeeBootstrap, ValidatorLiveness, and RadiclePublication.
// `BEGIN_BLOCK_ORDER` is Vote, Update, EnclaveUpgradeDeadline, Rewards,
// Staking, Oracle, OracleSlash, HyperlaneSlash, and CycleTick.
// `END_BLOCK_ORDER` is NodFactory, GemFactory, IntexFactory, CredisFactory,
// and GemCleanup.

use alloy_primitives::{Address, Bytes, U256};
use evm2::evm::{AccountInfo, CacheDB, EmptyDB};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, encode_outbe_block_artifacts, ConsensusHeaderArtifact,
    ExecutionSummaryArtifact, OutbeBlockArtifacts,
};
use outbe_primitives::tee_genesis_v1::NO_ATTESTATION_DEV_CHAIN_ID;
use outbe_primitives::OutbeHeader;
use reth_ethereum::primitives::{Header, SealedHeader};
use reth_evm::{
    execute::{BlockBuilder, CommitChanges, Executor},
    ConfigureEvm, RecoveredTx,
};
use reth_provider::{HashedPostStateProvider, StateRootProvider};

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
    let mut proposer_db = CacheDB::new(EmptyDB::default());
    seed_active_validators(&mut proposer_db, &[GENESIS_OWNER]);

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
            NO_ATTESTATION_DEV_CHAIN_ID,
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
            NO_ATTESTATION_DEV_CHAIN_ID,
            evm_env.block.gas_limit.saturating_to::<u64>(),
            parent.hash(),
            &ctx.inner.extra_data,
            ctx.parent_consensus_metadata.clone(),
            ctx.proposer_evm_address,
            None,
            None,
        )
        .expect("complete block-2 begin prefix must build");
    let begin_len = begin.len();
    let evm = config.evm_with_env(&mut proposer_db, evm_env.clone());
    let mut builder = config.create_block_builder(evm, evm_env, &parent, ctx);
    outbe_evm::executor::with_phase1_verify_disabled(|| builder.apply_pre_execution_changes())
        .expect("Phase 1 must be pre-executed before witness validation");
    builder
        .executor_mut()
        .force_preexecuted_phase1_witness_for_test(phase1_hash);

    let gas_used = builder
        .execute_transaction_with_commit_condition(phase1.clone(), |_, _| CommitChanges::Yes)
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
            .build_end_system_tx(2, NO_ATTESTATION_DEV_CHAIN_ID, GENESIS_OWNER, input)
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
    let mut proposer_db = CacheDB::new(EmptyDB::default());
    seed_active_validators(&mut proposer_db, &[GENESIS_OWNER]);

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
            NO_ATTESTATION_DEV_CHAIN_ID,
            evm_env.block.gas_limit.saturating_to::<u64>(),
            parent.hash(),
            &ctx.inner.extra_data,
            ctx.parent_consensus_metadata.clone(),
            ctx.proposer_evm_address,
            None,
            None,
        )
        .expect("complete block-2 begin prefix must build");
    let evm = config.evm_with_env(&mut proposer_db, evm_env.clone());
    let mut builder = config.create_block_builder(evm, evm_env, &parent, ctx);
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
        chain_id: NO_ATTESTATION_DEV_CHAIN_ID,
        gas_limit: 100_000,
        max_fee_per_gas: 2_000_000_000,
        to: TxKind::Call(Address::repeat_byte(0xDD)),
        ..Default::default()
    }
    .into_signed(Signature::test_signature())
    .into();
    let user = user.try_into_recovered().unwrap();
    let seed_user = |db: &mut CacheDB<EmptyDB>| {
        if include_user {
            db.insert_account_info(
                &user.signer(),
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
        let mut proposer_db = CacheDB::new(EmptyDB::default());
        seed_active_validators(&mut proposer_db, &active_set);
        seed_user(&mut proposer_db);
        assert_seeded_validators(&mut proposer_db, &active_set);
        outbe_evm::executor::with_phase1_verify_disabled(|| {
            let mut builder = config
                .builder_for_next_block(&mut proposer_db, &parent, attrs.clone())
                .expect("builder must be created");
            builder
                .apply_pre_execution_changes()
                .expect("pre-execution changes must succeed");
            let begin_system_txs = config
                .build_begin_system_txs(
                    2,
                    NO_ATTESTATION_DEV_CHAIN_ID,
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
            builder
                .finish_with_state_root(&provider, |output| {
                    if prepare_root {
                        let hashed = provider.hashed_post_state(&output.state).unwrap();
                        Ok(Some(provider.state_root_with_updates(hashed).unwrap()))
                    } else {
                        Ok(None)
                    }
                })
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

    let mut validator_db = CacheDB::new(EmptyDB::default());
    seed_active_validators(&mut validator_db, &active_set);
    seed_user(&mut validator_db);
    let validator_bundle = outbe_evm::executor::with_phase1_verify_disabled(|| {
        let mut validator_executor = config.batch_executor(validator_db);
        let replay = validator_executor
            .execute_one(&built_block)
            .expect("validator path must re-execute the built block");
        assert_eq!(replay.receipts, synchronous.execution_result.receipts);
        assert_eq!(replay.gas_used, synchronous.execution_result.gas_used);
        validator_executor.into_state()
    });

    let reexecuted_hashed_state = provider.hashed_post_state(&validator_bundle);
    let (reexecuted_root, _) = provider
        .state_root_with_updates(reexecuted_hashed_state.unwrap())
        .expect("re-executed state root must be computed");

    assert_eq!(
        built_root, reexecuted_root,
        "locally built base block must re-execute to the same state root on validator path",
    );
}

fn fold_state_updates_to_hashed(
    updates: &[reth_evm::StateUpdate],
) -> reth_ethereum::trie::HashedPostState {
    use alloy_primitives::keccak256;
    use reth_ethereum::{primitives::Account, trie::HashedPostState};

    let mut hashed = HashedPostState::default();
    for update in updates {
        for acc in &update.accounts {
            let hashed_addr = keccak256(acc.address);
            let account = acc.current.as_ref().map(|info| Account {
                nonce: info.nonce,
                balance: info.balance,
                bytecode_hash: (!info.is_empty_code_hash()).then_some(info.code_hash),
            });
            hashed.accounts.insert(hashed_addr, account);
            let entry = hashed.storages.entry(hashed_addr).or_default();
            if acc.wiped {
                entry.storage.clear();
            }
            for (slot, val) in &acc.storage {
                let hashed_slot = keccak256(slot.to_be_bytes::<32>());
                entry.storage.insert(hashed_slot, val.present_value);
            }
        }
    }
    hashed
}

fn build_paid_block_with_hook_mode(
    parent: &SealedHeader<OutbeHeader>,
    config: &outbe_evm::OutbeEvmConfig,
    provider: &DeterministicEmptyStateProvider,
    user_tx: &reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>,
    use_parallel_hook: bool,
) -> (
    reth_evm::execute::BlockBuilderOutcome<outbe_primitives::OutbePrimitives>,
    Option<alloy_primitives::B256>,
) {
    use std::sync::{Arc, Mutex};

    let mut db = CacheDB::new(EmptyDB::default());
    seed_active_validators(&mut db, &[GENESIS_OWNER]);
    let marker = evm2::bytecode::Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        &outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    db.insert_account_info(
        &user_tx.signer(),
        AccountInfo {
            balance: U256::from(100_000_000_000_000_000_000u128),
            ..Default::default()
        },
    );

    let mut attrs = next_block_attrs(Bytes::new());
    attrs.parent_consensus_metadata = Some(CertifiedParentAccountingMetadata {
        finalized_block_number: parent.header().inner.number,
        finalized_block_hash: parent.hash(),
        ..Default::default()
    });

    let hook_updates: Arc<Mutex<Vec<reth_evm::StateUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let hook_sink = Arc::clone(&hook_updates);

    outbe_evm::executor::with_phase1_verify_disabled(|| {
        let mut builder = config
            .builder_for_next_block(&mut db, parent, attrs.clone())
            .expect("builder must initialize");
        if use_parallel_hook {
            assert!(builder.set_state_hook(move |update| {
                hook_sink.lock().unwrap().push(update);
            }));
        }
        builder
            .apply_pre_execution_changes()
            .expect("pre-execution changes must succeed");
        let begin = config
            .build_begin_system_txs(
                parent.header().inner.number + 1,
                NO_ATTESTATION_DEV_CHAIN_ID,
                30_000_000,
                parent.hash(),
                &attrs.inner.extra_data,
                attrs.parent_consensus_metadata.clone(),
                attrs.proposer_evm_address,
                None,
                None,
            )
            .expect("begin prefix builds");
        for tx in begin {
            builder.execute_transaction(tx).expect("begin tx executes");
        }
        builder
            .execute_transaction(user_tx.clone())
            .expect("paid user tx executes");
        builder.executor_mut().apply_end_block_phase().unwrap();

        let (pre_len, pre_settlement_root) = if use_parallel_hook {
            let snapshot = hook_updates.lock().unwrap().clone();
            let pre_len = snapshot.len();
            let root = provider
                .state_root(fold_state_updates_to_hashed(&snapshot))
                .expect("pre-settlement root computes");
            (pre_len, Some(root))
        } else {
            (0, None)
        };

        let hook_reader = Arc::clone(&hook_updates);
        let outcome = builder
            .finish_with_state_root(provider, |_output| {
                if !use_parallel_hook {
                    return Ok(None);
                }
                let streamed = hook_reader.lock().unwrap().clone();
                let post_exec = &streamed[pre_len..];
                assert!(
                    !post_exec.is_empty(),
                    "finish() must stream post-execution updates after apply_end_block_phase"
                );
                let settled_nonzero_burned = post_exec.iter().any(|u| {
                    u.account(&outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS)
                        .is_some_and(|acc| {
                            acc.storage
                                .iter()
                                .any(|(_, slot)| slot.present_value > U256::ZERO)
                        })
                });
                assert!(
                    settled_nonzero_burned,
                    "post-exec state hook slice after apply_end_block_phase must observe non-zero PROMIS_LIMIT burned-fee settlement"
                );
                let parallel_root = provider
                    .state_root(fold_state_updates_to_hashed(&streamed))
                    .expect("parallel hooked root computes");
                Ok(Some((
                    parallel_root,
                    reth_ethereum::trie::updates::TrieUpdates::default(),
                )))
            })
            .expect("block finishes cleanly");
        (outcome, pre_settlement_root)
    })
}

/// Wave 3B mandatory seam test: verifies parallel hooked state-root computation equals the
/// synchronous fallback root on a block with non-zero burned fees (`settle_block_burned_fees`),
/// proving every post-execution write reaches the hook before drop.
#[test]
fn parallel_hooked_state_root_matches_sync_fallback_with_burned_fees() {
    use alloy_consensus::{SignableTransaction as _, TxEip1559};
    use alloy_primitives::{Signature, TxKind};
    use reth_primitives_traits::SignedTransaction as _;

    let parent = SealedHeader::seal_slow(OutbeHeader::new(Header {
        number: 1,
        gas_limit: 30_000_000,
        timestamp: TEST_BLOCK_ONE_TIMESTAMP - 1,
        base_fee_per_gas: Some(100_000_001),
        ..Default::default()
    }));
    let bridge = test_bridge();
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

    let user_tx: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: NO_ATTESTATION_DEV_CHAIN_ID,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: 2_000_000_000,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(Address::repeat_byte(0xDD)),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let user_tx = user_tx.try_into_recovered().unwrap();

    let (parallel, pre_settlement_root) =
        build_paid_block_with_hook_mode(&parent, &config, &provider, &user_tx, true);
    let (sync, _) = build_paid_block_with_hook_mode(&parent, &config, &provider, &user_tx, false);

    let parallel_root = parallel.block.header().inner.state_root;
    let sync_root = sync.block.header().inner.state_root;
    assert_ne!(
        parallel_root,
        pre_settlement_root.expect("parallel path records pre-settlement root"),
        "non-zero burned fees settled in finish() must change the final state root"
    );
    assert_eq!(
        parallel_root, sync_root,
        "parallel hooked state root must equal synchronous fallback root with non-zero burned fees"
    );
    assert_eq!(parallel.block, sync.block);
}

fn sign_test_eip1559(
    key_byte: u8,
    nonce: u64,
    to: Address,
    gas_limit: u64,
    input: Bytes,
) -> reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned> {
    use alloy_consensus::TxEip1559;
    use alloy_primitives::TxKind;
    use outbe_evm::signer::OutbeEvmSigner;
    use reth_primitives_traits::SignedTransaction as _;

    let signer = OutbeEvmSigner::from_secret_bytes([key_byte; 32]).unwrap();
    let signed = signer
        .sign_eip1559(TxEip1559 {
            chain_id: NO_ATTESTATION_DEV_CHAIN_ID,
            nonce,
            gas_limit,
            max_fee_per_gas: 2_000_000_000,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(to),
            value: U256::ZERO,
            input,
            access_list: Default::default(),
        })
        .unwrap();
    signed.try_into_recovered().unwrap()
}

fn find_reserved_create2_salt(factory: Address, initcode: &[u8]) -> alloy_primitives::B256 {
    use alloy_primitives::{keccak256, B256};
    use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;

    let hash = keccak256(initcode);
    (0u64..2_000_000)
        .map(|n| B256::from(U256::from(n)))
        .find(|salt| is_reserved_dynamic_precompile_address(factory.create2(*salt, hash)))
        .expect("search range contains a reserved dynamic precompile CREATE2 destination")
}

struct H1H2SeamAddresses {
    user: Address,
    create_caller: Address,
    reserved_create2_salt: alloy_primitives::B256,
    reserved_create2_dest: Address,
    selfdestruct_victim: Address,
    delegated_eoa: Address,
    delegation_caller: Address,
    policy_caller: Address,
}

fn seed_h1_h2_seam_state(db: &mut CacheDB<EmptyDB>, addrs: &H1H2SeamAddresses) {
    use evm2::bytecode::Bytecode as EvmBytecode;
    use outbe_system_common::addresses::{
        PROMIS_LIMIT_ADDRESS, STABLECOIN_POLICY_REGISTRY_ADDRESS, UPDATE_ADDRESS, ZEROFEE_ADDRESS,
    };

    seed_active_validators(db, &[GENESIS_OWNER]);
    let marker = EvmBytecode::new_legacy([0xef].into());
    for addr in [PROMIS_LIMIT_ADDRESS, ZEROFEE_ADDRESS, UPDATE_ADDRESS] {
        db.insert_account_info(
            &addr,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker.clone()),
                ..Default::default()
            },
        );
    }
    db.insert_account_storage(&UPDATE_ADDRESS, &U256::ZERO, &U256::from(2u64));
    db.insert_account_info(
        &addrs.user,
        AccountInfo {
            balance: U256::from(100_000_000_000_000_000_000u128),
            ..Default::default()
        },
    );

    // 1. CREATE2 contract colliding with a real reserved dynamic precompile address:
    //    slot 0 is pre-seeded to 0xDEAD so CREATE2 returning 0 on guard collision is observable.
    let mut create_bytes = vec![0x7f];
    create_bytes.extend_from_slice(addrs.reserved_create2_salt.as_slice());
    create_bytes.extend_from_slice(&[
        0x60, 0x01, 0x60, 0x00, 0x60, 0x00, 0xf5, // CREATE2(0, 0, 1, salt)
        0x60, 0x00, 0x55, 0x00, // SSTORE(0, addr); STOP
    ]);
    let create_code = EvmBytecode::new_legacy(Bytes::from(create_bytes));
    db.insert_account_info(
        &addrs.create_caller,
        AccountInfo {
            code_hash: create_code.hash_slow(),
            code: Some(create_code),
            ..Default::default()
        },
    );
    db.insert_account_storage(&addrs.create_caller, &U256::ZERO, &U256::from(0xDEADu64));

    // 2. Funded contract executing SELFDESTRUCT(beneficiary = ZEROFEE_ADDRESS), which carries
    //    the [0xef] system marker so GuardedSelfdestruct must redirect the balance to Address::ZERO.
    let mut sd_bytes = vec![0x73];
    sd_bytes.extend_from_slice(ZEROFEE_ADDRESS.as_slice());
    sd_bytes.push(0xff); // SELFDESTRUCT
    let sd_code = EvmBytecode::new_legacy(Bytes::from(sd_bytes));
    db.insert_account_info(
        &addrs.selfdestruct_victim,
        AccountInfo {
            balance: U256::from(123_456u64),
            code_hash: sd_code.hash_slow(),
            code: Some(sd_code),
            ..Default::default()
        },
    );

    // 3. EIP-7702 EOA delegated to registered native precompile ZEROFEE_ADDRESS, invoked via
    //    `delegation_caller` which records CALL status at slot 0 and RETURNDATASIZE at slot 1
    //    (pre-seeded to 0xBEEF so empty-code execution returning 0 bytes is explicitly verified).
    let delegation = EvmBytecode::new_eip7702(ZEROFEE_ADDRESS);
    db.insert_account_info(
        &addrs.delegated_eoa,
        AccountInfo {
            code_hash: delegation.hash_slow(),
            code: Some(delegation),
            ..Default::default()
        },
    );
    let mut del_caller_bytes = vec![
        0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x73,
    ];
    del_caller_bytes.extend_from_slice(addrs.delegated_eoa.as_slice());
    del_caller_bytes.extend_from_slice(&[
        0x5a, 0xf1, // GAS; CALL(gas, delegated_eoa, 0, 0, 0, 0, 0)
        0x60, 0x00, 0x55, // SSTORE(0, status)
        0x3d, 0x60, 0x01, 0x55, // SSTORE(1, RETURNDATASIZE)
        0x00,
    ]);
    let del_caller_code = EvmBytecode::new_legacy(Bytes::from(del_caller_bytes));
    db.insert_account_info(
        &addrs.delegation_caller,
        AccountInfo {
            code_hash: del_caller_code.hash_slow(),
            code: Some(del_caller_code),
            ..Default::default()
        },
    );
    db.insert_account_storage(
        &addrs.delegation_caller,
        &U256::from(1u64),
        &U256::from(0xBEEFu64),
    );

    // 4. Contract performing a nested STATICCALL into STABLECOIN_POLICY_REGISTRY_ADDRESS and
    //    storing the returned bool word at slot 0.
    let mut pol_bytes = vec![
        0x36, 0x60, 0x00, 0x60, 0x00, 0x37, // CALLDATACOPY(0, 0, CALLDATASIZE)
        0x60, 0x20, 0x60, 0x40, 0x36, 0x60, 0x00, 0x73,
    ];
    pol_bytes.extend_from_slice(STABLECOIN_POLICY_REGISTRY_ADDRESS.as_slice());
    pol_bytes.extend_from_slice(&[
        0x5a, 0xfa, // GAS; STATICCALL
        0x60, 0x40, 0x51, 0x60, 0x00, 0x55, // MLOAD(0x40) -> SSTORE(0)
        0x00,
    ]);
    let pol_code = EvmBytecode::new_legacy(Bytes::from(pol_bytes));
    db.insert_account_info(
        &addrs.policy_caller,
        AccountInfo {
            code_hash: pol_code.hash_slow(),
            code: Some(pol_code),
            ..Default::default()
        },
    );
}

/// `[C-S.1]` Advancing-height (`H1 -> H2`) seam across `outbe-evm`, `outbe-execution`, and
/// `outbe-runtime`: actively triggers and asserts `create_guard` (reserved dynamic CREATE2
/// collision), `selfdestruct_guard` (`[0xef]` beneficiary redirect to `Address::ZERO`),
/// EIP-7702 native delegation normalization (empty-code execution), stateful precompile nested
/// `STATICCALL`, and non-zero burned fees across two consecutive heights, confirming proposer
/// (`OutbeBlockBuilder`) and follower (`batch_executor`) derive byte-identical state roots,
/// receipts, and header artifacts at both `H1` and `H2`.
#[test]
fn advancing_heights_h1_to_h2_guards_subcalls_delegation_and_follower_parity() {
    use alloy_primitives::keccak256;
    use alloy_sol_types::SolCall as _;
    use evm2::block::BundleSource;
    use outbe_evm::signer::OutbeEvmSigner;
    use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;
    use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
    use outbe_system_common::addresses::ZEROFEE_ADDRESS;

    const USER_KEY: u8 = 0x51;
    let create_caller = Address::repeat_byte(0xC1);
    let reserved_create2_salt = find_reserved_create2_salt(create_caller, &[0x00]);
    let reserved_create2_dest = create_caller.create2(reserved_create2_salt, keccak256([0x00]));
    assert!(is_reserved_dynamic_precompile_address(
        reserved_create2_dest
    ));

    let addrs = H1H2SeamAddresses {
        user: OutbeEvmSigner::from_secret_bytes([USER_KEY; 32])
            .unwrap()
            .address(),
        create_caller,
        reserved_create2_salt,
        reserved_create2_dest,
        selfdestruct_victim: Address::repeat_byte(0xC2),
        delegated_eoa: Address::repeat_byte(0xC3),
        delegation_caller: Address::repeat_byte(0xC5),
        policy_caller: Address::repeat_byte(0xC4),
    };

    let parent_h0 = SealedHeader::seal_slow(OutbeHeader::new(Header {
        number: 1,
        gas_limit: 30_000_000,
        timestamp: TEST_BLOCK_ONE_TIMESTAMP - 1,
        base_fee_per_gas: Some(100_000_001),
        ..Default::default()
    }));
    let bridge = test_bridge();
    bridge.record_execution_summary_with_state_root(
        1,
        parent_h0.hash(),
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        parent_h0.header().inner.timestamp,
        parent_h0.header().inner.state_root,
    );
    // Use a Prague-activated chain spec so EIP-7702 delegation is active alongside CREATE2
    // and SELFDESTRUCT guards across H1 -> H2 (matching Outbe's Prague runtime).
    let prague_spec = {
        use outbe_primitives::tee_test_utils::{
            no_attestation_dev_policy_v1, tee_attestation_v1_extra_field,
        };
        let mut spec = reth_chainspec::ChainSpecBuilder::from(&*reth_ethereum::chainspec::MAINNET)
            .prague_activated()
            .build();
        spec.chain = NO_ATTESTATION_DEV_CHAIN_ID.into();
        spec.genesis.config.chain_id = NO_ATTESTATION_DEV_CHAIN_ID;
        spec.genesis.timestamp = TEST_BLOCK_ONE_TIMESTAMP - 1;
        let policy = no_attestation_dev_policy_v1(spec.chain().id(), spec.genesis_hash())
            .expect("test NoAttestationDev policy is canonical");
        spec.genesis.config.extra_fields.insert(
            "teeAttestationV1".to_owned(),
            tee_attestation_v1_extra_field(&policy)
                .expect("test TEE activation manifest is canonical"),
        );
        std::sync::Arc::new(spec.map_header(OutbeHeader::new))
    };
    let config = outbe_evm::config::OutbeEvmConfig::new_with_bridge(
        prague_spec,
        bridge.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(test_evm_signer());
    let provider = DeterministicEmptyStateProvider;

    let mut proposer_db = CacheDB::new(EmptyDB::default());
    seed_h1_h2_seam_state(&mut proposer_db, &addrs);
    let mut follower_db = proposer_db.clone();

    let h1_txs = vec![
        // 1M gas so the 1/64 remaining after CREATE2 collision covers the trailing SSTORE(0, 0).
        sign_test_eip1559(USER_KEY, 0, addrs.create_caller, 1_000_000, Bytes::new()),
        sign_test_eip1559(
            USER_KEY,
            1,
            addrs.selfdestruct_victim,
            100_000,
            Bytes::new(),
        ),
        sign_test_eip1559(USER_KEY, 2, addrs.delegation_caller, 200_000, Bytes::new()),
    ];
    let policy_calldata = Bytes::from(
        IStablecoinPolicyRegistry::policyExistsCall {
            policyId: U256::from(1u64),
        }
        .abi_encode(),
    );
    let h2_txs = vec![
        sign_test_eip1559(USER_KEY, 3, addrs.policy_caller, 200_000, policy_calldata),
        sign_test_eip1559(
            USER_KEY,
            4,
            Address::repeat_byte(0xDD),
            21_000,
            Bytes::new(),
        ),
    ];

    let mut parent = parent_h0;
    for (step, (height, txs)) in [(2u64, h1_txs), (3u64, h2_txs)].into_iter().enumerate() {
        let mut attrs = next_block_attrs(Bytes::new());
        attrs.inner.timestamp = TEST_BLOCK_ONE_TIMESTAMP + step as u64;
        attrs.inner.parent_beacon_block_root = Some(alloy_primitives::B256::ZERO);
        attrs.parent_consensus_metadata = Some(CertifiedParentAccountingMetadata {
            finalized_block_number: height - 1,
            finalized_block_hash: parent.hash(),
            ..Default::default()
        });

        let built = outbe_evm::executor::with_phase1_verify_disabled(|| {
            let mut builder = config
                .builder_for_next_block(&mut proposer_db, &parent, attrs.clone())
                .expect("proposer builder initializes");
            builder.apply_pre_execution_changes().unwrap();
            let begin = config
                .build_begin_system_txs(
                    height,
                    NO_ATTESTATION_DEV_CHAIN_ID,
                    30_000_000,
                    parent.hash(),
                    &attrs.inner.extra_data,
                    attrs.parent_consensus_metadata.clone(),
                    attrs.proposer_evm_address,
                    None,
                    None,
                )
                .unwrap();
            for tx in begin {
                builder.execute_transaction(tx).unwrap();
            }
            for tx in txs {
                builder.execute_transaction(tx).unwrap();
            }
            builder.executor_mut().apply_end_block_phase().unwrap();
            builder.finish(&provider, None).unwrap()
        });
        assert!(
            built.execution_result.receipts.iter().all(|r| r.success),
            "all transactions at height {height} must succeed, got {:?}",
            built.execution_result.receipts
        );
        proposer_db.commit_source(&BundleSource(&built.execution_state));

        let follower_bundle = outbe_evm::executor::with_phase1_verify_disabled(|| {
            let mut batch = config.batch_executor(&mut follower_db);
            let replay = batch
                .execute_one(&built.block)
                .expect("follower must replay built block");
            assert_eq!(replay.receipts, built.execution_result.receipts);
            assert_eq!(replay.gas_used, built.execution_result.gas_used);
            batch.into_state()
        });
        follower_db.commit_source(&BundleSource(&follower_bundle));

        let follower_root = provider
            .state_root(provider.hashed_post_state(&follower_bundle).unwrap())
            .unwrap();
        assert_eq!(
            built.block.header().inner.state_root,
            follower_root,
            "proposer and follower state roots must match at height {height}"
        );
        // Record H1 execution summary for H2 parent-accounting lookup.
        let summary = decode_outbe_block_artifacts(built.block.header().inner.extra_data.as_ref())
            .unwrap()
            .execution_summary
            .expect("built header must include execution summary");
        bridge.record_execution_summary_with_state_root(
            height,
            built.block.hash(),
            summary,
            built.block.header().inner.timestamp,
            built.block.header().inner.state_root,
        );
        parent = built.block.sealed_header().clone();
    }

    // Verify every guard and sub-call outcome explicitly on BOTH proposer and follower state.
    use evm2::evm::Database as _;
    for (side, db) in [
        ("proposer", &mut proposer_db),
        ("follower", &mut follower_db),
    ] {
        // 1. create_guard blocked reserved CREATE2 collision -> returned 0 (overwrote 0xDEAD),
        //    and deployed no code at the reserved destination.
        assert_eq!(
            db.get_storage(&addrs.create_caller, &U256::ZERO).unwrap(),
            U256::ZERO,
            "{side}: create_guard must return 0 on reserved CREATE2 collision"
        );
        assert!(
            db.account_info(&addrs.reserved_create2_dest).is_none(),
            "{side}: reserved CREATE2 destination must remain undeployed"
        );

        // 2. selfdestruct_guard redirected [0xef]-marked beneficiary (ZEROFEE_ADDRESS) to Address::ZERO.
        assert_eq!(
            db.account_info(&ZEROFEE_ADDRESS)
                .map(|a| a.balance)
                .unwrap_or_default(),
            U256::ZERO,
            "{side}: [0xef] system beneficiary must not receive selfdestruct balance"
        );
        assert_eq!(
            db.account_info(&addrs.selfdestruct_victim)
                .map(|a| a.balance)
                .unwrap_or_default(),
            U256::ZERO,
            "{side}: selfdestruct victim balance must be drained"
        );
        assert_eq!(
            db.account_info(&Address::ZERO)
                .map(|a| a.balance)
                .unwrap_or_default(),
            U256::from(123_456u64),
            "{side}: selfdestruct_guard must redirect victim balance to Address::ZERO"
        );

        // 3. native_delegation normalized EIP-7702 -> ZEROFEE_ADDRESS into empty-code execution
        //    (CALL status == 1, RETURNDATASIZE == 0 overwriting 0xBEEF).
        assert_eq!(
            db.get_storage(&addrs.delegation_caller, &U256::ZERO)
                .unwrap(),
            U256::from(1u64),
            "{side}: delegated native call must succeed"
        );
        assert_eq!(
            db.get_storage(&addrs.delegation_caller, &U256::from(1u64))
                .unwrap(),
            U256::ZERO,
            "{side}: delegated native call must execute empty code (0 returndata)"
        );

        // 4. H2 nested STATICCALL to STABLECOIN_POLICY_REGISTRY persisted policyExists(1) == true.
        assert_eq!(
            db.get_storage(&addrs.policy_caller, &U256::ZERO).unwrap(),
            U256::from(1u64),
            "{side}: nested STATICCALL to STABLECOIN_POLICY_REGISTRY must return true at H2"
        );
    }
}
